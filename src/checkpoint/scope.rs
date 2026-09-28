//! A frozen selection policy. Restoration never reloads today's ignore files.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};

use super::fs::{invalid, relative};

const RULE_BYTES: usize = 4 * 1024 * 1024;
const RULE_COUNT: usize = 4096;
const RULE_LINES: usize = 32_768;
const RULE_LABEL_BYTES: usize = 4096;

/// Higher kinds take precedence. Within a kind, the nearest containing
/// directory wins, just as nested ignore files do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IgnoreKind {
    Global,
    Repository,
    Git,
    Ignore,
}

/// Already-read rules, with an absolute matching base. The contents, not a
/// mutable path to the source file, become part of the saved policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IgnoreRule {
    pub kind: IgnoreKind,
    pub base: String,
    pub source: String,
    pub contents: String,
}

/// Input from the product's workspace discovery. This low-level layer does
/// not invoke Git: callers supply the actual tracked paths and any inherited,
/// repository-local or global rules. Local .gitignore/.ignore files are read
/// from the workspace while capturing. No rule is re-read while restoring.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopeInput {
    pub tracked: BTreeSet<String>,
    pub external_rules: Vec<IgnoreRule>,
    /// Normalized, workspace-relative paths; each protects its whole subtree.
    /// The host must include any live runtime storage inside the workspace.
    pub exclusions: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FrozenScope {
    pub tracked: BTreeSet<String>,
    pub exclusions: BTreeSet<String>,
    pub rules: Vec<IgnoreRule>,
}

impl FrozenScope {
    pub fn from_input(input: ScopeInput) -> Self {
        Self {
            tracked: input.tracked,
            exclusions: input.exclusions,
            rules: input.external_rules,
        }
    }
}

pub(super) struct Selection {
    root: PathBuf,
    pub frozen: FrozenScope,
    matchers: BTreeMap<IgnoreKind, BTreeMap<PathBuf, Gitignore>>,
    rule_bytes: usize,
    rule_lines: usize,
}

impl Selection {
    pub fn new(root: &Path, frozen: FrozenScope) -> io::Result<Self> {
        for path in frozen.tracked.iter().chain(frozen.exclusions.iter()) {
            relative(path)?;
        }
        let rules = frozen.rules.clone();
        let mut selection = Self {
            root: root.to_path_buf(),
            frozen: FrozenScope {
                rules: Vec::new(),
                ..frozen
            },
            matchers: BTreeMap::new(),
            rule_bytes: 0,
            rule_lines: 0,
        };
        for rule in rules {
            selection.add(rule)?;
        }
        Ok(selection)
    }

    pub fn add(&mut self, rule: IgnoreRule) -> io::Result<()> {
        let base = Path::new(&rule.base);
        if !base.is_absolute()
            || base
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            || !(base.starts_with(&self.root) || self.root.starts_with(base))
        {
            return Err(invalid(
                "ignore rule base is unrelated to the snapshot workspace",
            ));
        }
        if rule.source.len() > RULE_LABEL_BYTES || rule.base.len() > RULE_LABEL_BYTES {
            return Err(invalid(
                "snapshot ignore-rule label or base exceeds the size limit",
            ));
        }
        let bytes = self
            .rule_bytes
            .checked_add(rule.contents.len())
            .and_then(|bytes| bytes.checked_add(rule.source.len()))
            .and_then(|bytes| bytes.checked_add(rule.base.len()))
            .ok_or_else(|| invalid("snapshot ignore-rule size overflow"))?;
        let lines = self
            .rule_lines
            .checked_add(rule.contents.lines().take(RULE_LINES + 1).count())
            .ok_or_else(|| invalid("snapshot ignore-rule line count overflow"))?;
        if bytes > RULE_BYTES || lines > RULE_LINES || self.frozen.rules.len() >= RULE_COUNT {
            return Err(invalid(
                "snapshot ignore rules exceed the size, line or count limit",
            ));
        }
        let by_base = self.matchers.entry(rule.kind).or_default();
        if by_base.contains_key(base) {
            return Err(invalid("duplicate snapshot ignore-rule source"));
        }
        let mut builder = GitignoreBuilder::new(base);
        for line in rule.contents.lines() {
            // The frozen rule retains its label once. Giving the builder a
            // source path here would clone that label into every pattern.
            builder
                .add_line(None, line)
                .map_err(|error| invalid(format!("invalid snapshot ignore rule: {error}")))?;
        }
        let matcher = builder
            .build()
            .map_err(|error| invalid(format!("invalid snapshot ignore rules: {error}")))?;
        by_base.insert(base.to_path_buf(), matcher);
        self.rule_bytes = bytes;
        self.rule_lines = lines;
        self.frozen.rules.push(rule);
        Ok(())
    }

    pub fn protected(&self, path: &str) -> bool {
        path.split('/')
            .any(|part| part.eq_ignore_ascii_case(".git"))
            || self
                .frozen
                .exclusions
                .iter()
                .any(|excluded| Path::new(path).starts_with(excluded))
    }

    fn direct_ignore(&self, path: &Path, directory: bool) -> bool {
        for by_base in self.matchers.values().rev() {
            // A directory's own ignore file only applies to its children.
            for base in path.parent().into_iter().flat_map(Path::ancestors) {
                if let Some(matcher) = by_base.get(base) {
                    let matched = matcher.matched(path, directory);
                    if matched.is_ignore() {
                        return true;
                    }
                    if matched.is_whitelist() {
                        return false;
                    }
                }
            }
        }
        false
    }

    fn ignored(&self, path: &str, directory: bool) -> bool {
        // Inherited rules may exclude the workspace itself, or an ancestor.
        // Starting below the workspace would incorrectly resurrect its files.
        if self
            .root
            .ancestors()
            .any(|parent| self.direct_ignore(parent, true))
        {
            return true;
        }
        let components: Vec<_> = path.split('/').collect();
        let mut prefix = self.root.clone();
        for (index, part) in components.iter().enumerate() {
            prefix.push(part);
            let is_directory = index + 1 < components.len() || directory;
            if self.direct_ignore(&prefix, is_directory) {
                // A child whitelist cannot resurrect an excluded parent.
                return true;
            }
        }
        false
    }

    pub fn file(&self, path: &str) -> bool {
        !self.protected(path) && (self.frozen.tracked.contains(path) || !self.ignored(path, false))
    }

    pub fn directory(&self, path: &str) -> bool {
        if self.protected(path) {
            return false;
        }
        if !self.ignored(path, true) {
            return true;
        }
        let prefix = format!("{path}/");
        self.frozen
            .tracked
            .range(prefix.clone()..)
            .next()
            .is_some_and(|tracked| tracked.starts_with(&prefix))
    }
}
