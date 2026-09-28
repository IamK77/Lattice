//! File capture and non-destructive restore planning.
//!
//! These primitives do not execute a restore. Applying a plan belongs behind
//! the product's committed-event/confirmation boundary; there is deliberately
//! no "restore this directory now" shortcut here.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::fs::{invalid, relative, Directory, Identity, Kind, Stamp};
use super::objects::{self, Objects};
use super::scope::{FrozenScope, IgnoreKind, IgnoreRule, ScopeInput, Selection};
use super::ObjectRef;

const VERSION: u32 = 1;
const IGNORE_FILE_LIMIT: u64 = 1024 * 1024;
const PATH_METADATA_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Limits {
    pub file_bytes: u64,
    pub total_bytes: u64,
    pub entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: 64 * 1024 * 1024,
            total_bytes: 512 * 1024 * 1024,
            entries: 50_000,
        }
    }
}

/// Raw contents and ordinary permission bits. This is not an assertion that
/// ownership, ACLs, extended attributes or file identity have been archived.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileState {
    pub content: ObjectRef,
    pub mode: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    version: u32,
    workspace: String,
    identity: Identity,
    scope: FrozenScope,
    pub files: BTreeMap<String, FileState>,
    /// Ancestors needed to recreate saved files, not a snapshot of empty dirs.
    pub directories: BTreeMap<String, u32>,
}

impl Snapshot {
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn exclusions(&self) -> &BTreeSet<String> {
        &self.scope.exclusions
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub path: String,
    pub before: Option<FileState>,
    pub after: Option<FileState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    version: u32,
    pub target: ObjectRef,
    /// A separately retained copy of the current state in the target's scope.
    pub before: ObjectRef,
    pub changes: Vec<Change>,
    observed: BTreeMap<String, Stamp>,
}

pub struct Archive {
    workspace: PathBuf,
    directory: Directory,
    identity: Identity,
    objects: Objects,
}

impl Archive {
    /// The storage parent must already exist. A new private archive may be
    /// created there; an unrelated existing directory is never adopted.
    pub fn open(storage: &Path, workspace: &Path) -> io::Result<Self> {
        Self::open_after_check(storage, workspace, || {})
    }

    // Internal fault-injection boundary; no callback crosses a component port.
    pub(super) fn open_after_check(
        storage: &Path,
        workspace: &Path,
        after_check: impl FnOnce(),
    ) -> io::Result<Self> {
        let (workspace, directory) = Directory::open(workspace)?;
        let identity = Identity::of(&directory.metadata()?);
        if workspace.to_str().is_none() {
            return Err(invalid("file snapshots require a UTF-8 workspace path"));
        }
        let location = objects::location(storage)?;
        if location.path.starts_with(&workspace) || workspace.starts_with(&location.path) {
            return Err(invalid(
                "file archive and workspace must be separate directories",
            ));
        }
        after_check();
        let objects = Objects::open_location(location)?;
        let archive = Self {
            workspace,
            directory,
            identity,
            objects,
        };
        archive.check_root()?;
        Ok(archive)
    }

    fn check_root(&self) -> io::Result<()> {
        self.directory.verify_path(&self.workspace)?;
        if Identity::of(&self.directory.metadata()?) != self.identity {
            return Err(invalid("snapshot workspace identity changed"));
        }
        self.objects.validate_location()
    }

    pub fn capture(&self, input: ScopeInput, limits: &Limits) -> io::Result<ObjectRef> {
        self.capture_after(input, limits, || {})
    }

    // An internal observation boundary for deterministic race tests. It is
    // not a component interface, nor a callback passed across runtime parts.
    pub(super) fn capture_after(
        &self,
        input: ScopeInput,
        limits: &Limits,
        after_copy: impl FnOnce(),
    ) -> io::Result<ObjectRef> {
        self.check_root()?;
        if input.tracked.len() > limits.entries {
            return Err(invalid("tracked paths exceed the snapshot entry limit"));
        }
        let mut selection = Selection::new(&self.workspace, FrozenScope::from_input(input))?;
        let first = self.scan(&mut selection, limits, true, true)?;
        after_copy();
        let mut frozen = Selection::new(&self.workspace, selection.frozen.clone())?;
        let second = self.scan(&mut frozen, limits, false, false)?;
        same_scan(&first, &second)?;
        for (path, expected) in &first.probes {
            if self.rule_bytes(path)? != *expected {
                return Err(invalid(format!(
                    "ignore file changed while capturing: {path}"
                )));
            }
        }
        self.check_root()?;
        self.objects
            .put_record(&self.snapshot(selection.frozen, first))
    }

    pub fn snapshot_record(&self, reference: &ObjectRef) -> io::Result<Snapshot> {
        self.check_root()?;
        let snapshot: Snapshot = self.objects.record(reference)?;
        self.validate_snapshot(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_snapshot(&self, snapshot: &Snapshot) -> io::Result<()> {
        if snapshot.version != VERSION
            || Path::new(&snapshot.workspace) != self.workspace
            || snapshot.identity != self.identity
        {
            return Err(invalid(
                "snapshot version or workspace identity does not match",
            ));
        }
        let selection = Selection::new(&self.workspace, snapshot.scope.clone())?;
        let mut ancestors = BTreeSet::new();
        for (path, state) in &snapshot.files {
            relative(path)?;
            if !selection.file(path) || state.mode & !0o777 != 0 {
                return Err(invalid(
                    "snapshot file is outside its policy or has invalid permissions",
                ));
            }
            state.content.validate()?;
            for parent in parents(path) {
                if !snapshot.directories.contains_key(parent) {
                    return Err(invalid(
                        "snapshot ancestor directory metadata is incomplete",
                    ));
                }
                ancestors.insert(parent);
            }
        }
        if snapshot.directories.len() != ancestors.len()
            || snapshot.directories.values().any(|mode| mode & !0o777 != 0)
        {
            return Err(invalid(
                "snapshot ancestor directory metadata is incomplete or invalid",
            ));
        }
        for path in snapshot.files.keys() {
            if snapshot.directories.contains_key(path) {
                return Err(invalid("snapshot path is both a file and a directory"));
            }
        }
        Ok(())
    }

    fn verify_contents(&self, snapshot: &Snapshot, limits: &Limits) -> io::Result<()> {
        if snapshot.files.len() > limits.entries
            || snapshot.directories.len() > limits.entries - snapshot.files.len()
        {
            return Err(invalid("saved snapshot exceeds the entry limit"));
        }
        let mut total = 0u64;
        for state in snapshot.files.values() {
            if state.content.bytes > limits.file_bytes {
                return Err(invalid("saved file exceeds the snapshot byte limit"));
            }
            total = total
                .checked_add(state.content.bytes)
                .ok_or_else(|| invalid("saved snapshot byte count overflow"))?;
            if total > limits.total_bytes {
                return Err(invalid("saved snapshot exceeds the total byte limit"));
            }
        }
        // Check all declared sizes before reading any potentially large blob.
        for state in snapshot.files.values() {
            self.objects.verify(&state.content)?;
        }
        Ok(())
    }

    /// Capture a safety copy and produce a complete, immutable file plan. This
    /// never changes workspace files and does not imply user authorization.
    pub fn prepare(&self, target: &ObjectRef, limits: &Limits) -> io::Result<ObjectRef> {
        let desired = self.snapshot_record(target)?;
        self.verify_contents(&desired, limits)?;
        let mut selection = Selection::new(&self.workspace, desired.scope.clone())?;
        let first = self.scan(&mut selection, limits, false, true)?;
        let second = self.scan(&mut selection, limits, false, false)?;
        same_scan(&first, &second)?;
        let observed = first.stamps.clone();
        let before = self.snapshot(desired.scope.clone(), first);
        let changes = changes(&before, &desired);
        self.check_change_paths(&changes, limits)?;
        self.check_root()?;
        let before = self.objects.put_record(&before)?;
        self.objects.put_record(&Plan {
            version: VERSION,
            target: target.clone(),
            before,
            changes,
            observed,
        })
    }

    pub fn plan_record(&self, reference: &ObjectRef) -> io::Result<Plan> {
        self.check_root()?;
        let plan: Plan = self.objects.record(reference)?;
        let target = self.snapshot_record(&plan.target)?;
        let before = self.snapshot_record(&plan.before)?;
        if plan.version != VERSION
            || target.scope != before.scope
            || plan.changes != changes(&before, &target)
            || plan.observed.keys().collect::<Vec<_>>() != before.files.keys().collect::<Vec<_>>()
        {
            return Err(invalid(
                "restore plan does not match its retained snapshots",
            ));
        }
        for (path, stamp) in &plan.observed {
            let state = &before.files[path];
            if stamp.bytes != state.content.bytes
                || stamp.mode & 0o777 != state.mode
                || stamp.links != 1
            {
                return Err(invalid(
                    "restore plan has inconsistent observed file versions",
                ));
            }
        }
        Ok(plan)
    }

    /// Revalidate just before a future execution boundary. Success means the
    /// observed files still match, not that other writers have been frozen.
    pub fn validate_plan(&self, reference: &ObjectRef, limits: &Limits) -> io::Result<()> {
        let plan = self.plan_record(reference)?;
        let target = self.snapshot_record(&plan.target)?;
        let before = self.snapshot_record(&plan.before)?;
        self.verify_contents(&target, limits)?;
        self.verify_contents(&before, limits)?;
        let mut selection = Selection::new(&self.workspace, before.scope.clone())?;
        let current = self.scan(&mut selection, limits, false, false)?;
        if current.files != before.files
            || current.directories != before.directories
            || current.stamps != plan.observed
        {
            return Err(invalid(
                "workspace changed since the restore preview; prepare a new plan",
            ));
        }
        self.check_change_paths(&plan.changes, limits)?;
        self.check_root()
    }

    fn check_change_paths(&self, changes: &[Change], limits: &Limits) -> io::Result<()> {
        for change in changes {
            let current = self.current_file(&change.path, limits)?;
            if current.as_ref().map(|(state, _)| state) != change.before.as_ref() {
                return Err(invalid(format!(
                    "restore path changed, aliases another name, or has a conflicting type: {}",
                    change.path,
                )));
            }
        }
        Ok(())
    }

    fn current_file(&self, path: &str, limits: &Limits) -> io::Result<Option<(FileState, Stamp)>> {
        let (parent, leaf) = match self.directory.parent(path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match parent.kind(&leaf)? {
            None => Ok(None),
            Some(Kind::File) => self
                .read_file(&parent, &leaf, limits.file_bytes, false)
                .map(Some),
            Some(_) => Err(invalid(format!(
                "restore path is not a regular file: {path}"
            ))),
        }
    }

    fn snapshot(&self, scope: FrozenScope, scan: Scan) -> Snapshot {
        Snapshot {
            version: VERSION,
            workspace: self
                .workspace
                .to_str()
                .expect("validated UTF-8 workspace")
                .to_owned(),
            identity: self.identity.clone(),
            scope,
            files: scan.files,
            directories: scan.directories,
        }
    }

    fn read_file(
        &self,
        parent: &Directory,
        leaf: &str,
        limit: u64,
        save: bool,
    ) -> io::Result<(FileState, Stamp)> {
        let mut file = parent.file(leaf)?;
        let before = file.metadata()?;
        if before.nlink() != 1 || before.mode() & 0o7000 != 0 {
            return Err(invalid(
                "file snapshots refuse hard links and special permission bits",
            ));
        }
        if before.len() > limit {
            return Err(invalid(format!(
                "file exceeds the snapshot byte limit: {leaf}"
            )));
        }
        let content = if save {
            self.objects.put(&mut file, limit)?
        } else {
            objects::transfer(&mut file, &mut io::sink(), limit)?
        };
        let stamp = Stamp::of(&before);
        if stamp != Stamp::of(&file.metadata()?)
            || stamp != Stamp::of(&parent.file(leaf)?.metadata()?)
            || content.bytes != before.len()
        {
            return Err(invalid(format!("file changed while capturing: {leaf}")));
        }
        Ok((
            FileState {
                content,
                mode: before.mode() & 0o777,
            },
            stamp,
        ))
    }

    fn rule_bytes(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        let (parent, leaf) = self.directory.parent(path)?;
        match parent.kind(&leaf)? {
            None => Ok(None),
            Some(Kind::File) => {
                let mut file = parent.file(&leaf)?;
                let before = Stamp::of(&file.metadata()?);
                let mut contents = Vec::new();
                file.by_ref()
                    .take(IGNORE_FILE_LIMIT + 1)
                    .read_to_end(&mut contents)?;
                if contents.len() as u64 > IGNORE_FILE_LIMIT {
                    return Err(invalid("snapshot ignore file exceeds the size limit"));
                }
                if before != Stamp::of(&file.metadata()?)
                    || before != Stamp::of(&parent.file(&leaf)?.metadata()?)
                {
                    return Err(invalid("snapshot ignore file changed while reading"));
                }
                Ok(Some(contents))
            }
            Some(_) => Err(invalid("snapshot ignore source is not a regular file")),
        }
    }

    fn check_exclusion_spelling(&self, selection: &Selection, limits: &Limits) -> io::Result<()> {
        if selection.frozen.exclusions.len() > limits.entries {
            return Err(invalid("excluded paths exceed the snapshot entry limit"));
        }
        // Borrow prefixes from the frozen paths instead of copying a long
        // prefix once per ancestor; all directory listings share one budget.
        let mut listings = BTreeMap::<&str, Vec<String>>::new();
        let mut remaining = limits.entries;
        for excluded in &selection.frozen.exclusions {
            let mut directory = self.directory.clone_handle()?;
            let mut prefix = "";
            for leaf in excluded.split('/') {
                let Some(kind) = directory.kind(leaf)? else {
                    break;
                };
                let names = match listings.entry(prefix) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let names = directory.entries(remaining)?;
                        remaining -= names.len();
                        entry.insert(names)
                    }
                };
                // A lookup may succeed under an alias on a case-insensitive or
                // normalization-insensitive filesystem. Refuse ambiguity rather
                // than treating that live directory as outside the exclusion.
                if names
                    .binary_search_by(|name| name.as_str().cmp(leaf))
                    .is_err()
                {
                    return Err(invalid(format!(
                        "excluded path is a filesystem spelling alias: {excluded}"
                    )));
                }
                let length = prefix.len() + usize::from(!prefix.is_empty()) + leaf.len();
                // A whole excluded directory need not be readable. Only enter
                // an ancestor when another path component still needs checking.
                if kind != Kind::Directory || length == excluded.len() {
                    break;
                }
                directory = directory.child(leaf)?;
                prefix = &excluded[..length];
            }
        }
        Ok(())
    }

    fn scan(
        &self,
        selection: &mut Selection,
        limits: &Limits,
        discover: bool,
        save: bool,
    ) -> io::Result<Scan> {
        self.check_root()?;
        self.check_exclusion_spelling(selection, limits)?;
        let mut scan = Scan::default();
        self.walk(
            &self.directory,
            "",
            selection,
            limits,
            discover,
            save,
            &mut scan,
        )?;
        // A tracked path that exists but was not captured must never quietly
        // disappear from the snapshot (including case aliases on macOS).
        for path in &selection.frozen.tracked {
            if selection.protected(path) {
                continue;
            }
            if self.current_file(path, limits)?.is_some() && !scan.files.contains_key(path) {
                return Err(invalid(format!(
                    "tracked path was not captured exactly: {path}"
                )));
            }
        }
        let required: BTreeSet<_> = scan.files.keys().flat_map(|path| parents(path)).collect();
        scan.directories
            .retain(|path, _| required.contains(path.as_str()));
        self.check_exclusion_spelling(selection, limits)?;
        self.check_root()?;
        Ok(scan)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        directory: &Directory,
        prefix: &str,
        selection: &mut Selection,
        limits: &Limits,
        discover: bool,
        save: bool,
        scan: &mut Scan,
    ) -> io::Result<()> {
        if prefix.split('/').count() > super::fs::MAX_DEPTH {
            return Err(invalid("snapshot directory nesting exceeds the limit"));
        }
        let directory_stamp = Stamp::of(&directory.metadata()?);
        if discover {
            for (leaf, kind) in [
                (".gitignore", IgnoreKind::Git),
                (".ignore", IgnoreKind::Ignore),
            ] {
                let path = joined(prefix, leaf);
                let contents = self.rule_bytes(&path)?;
                if let Some(bytes) = &contents {
                    let contents = std::str::from_utf8(bytes)
                        .map_err(|_| invalid("snapshot ignore files must be UTF-8"))?
                        .to_owned();
                    selection.add(IgnoreRule {
                        kind,
                        base: self
                            .workspace
                            .join(prefix)
                            .to_str()
                            .expect("UTF-8 paths")
                            .to_owned(),
                        source: path.clone(),
                        contents,
                    })?;
                }
                scan.probes.insert(path, contents);
            }
        }
        let remaining = limits.entries.saturating_sub(scan.visited);
        for leaf in directory.entries(remaining)? {
            if scan.visited >= limits.entries {
                return Err(invalid("workspace exceeds the snapshot entry limit"));
            }
            scan.visited += 1;
            let path = joined(prefix, &leaf);
            scan.account_path(&path)?;
            if selection.protected(&path) {
                continue;
            }
            relative(&path)?;
            match directory.kind(&leaf)? {
                Some(Kind::Directory) if selection.directory(&path) => {
                    let child = directory.child(&leaf)?;
                    let mode = child.metadata()?.mode();
                    if mode & 0o7000 != 0 {
                        return Err(invalid(
                            "snapshot directories with special permissions are unsupported",
                        ));
                    }
                    scan.directories.insert(path.clone(), mode & 0o777);
                    self.walk(&child, &path, selection, limits, discover, save, scan)?;
                }
                Some(Kind::File) if selection.file(&path) => {
                    let room = limits
                        .total_bytes
                        .saturating_sub(scan.bytes)
                        .min(limits.file_bytes);
                    let (state, stamp) = self.read_file(directory, &leaf, room, save)?;
                    scan.bytes = scan
                        .bytes
                        .checked_add(state.content.bytes)
                        .ok_or_else(|| invalid("snapshot byte count overflow"))?;
                    scan.files.insert(path.clone(), state);
                    scan.stamps.insert(path, stamp);
                }
                Some(Kind::Other) if selection.file(&path) => {
                    return Err(invalid(format!(
                        "snapshot entry is a link or special file: {path}"
                    )));
                }
                None => {
                    return Err(invalid(format!(
                        "entry disappeared while capturing: {path}"
                    )))
                }
                _ => {}
            }
        }
        if directory_stamp != Stamp::of(&directory.metadata()?) {
            return Err(invalid(
                "directory changed while collecting snapshot entries",
            ));
        }
        scan.directory_stamps
            .insert(prefix.to_owned(), directory_stamp);
        Ok(())
    }
}

#[derive(Default)]
struct Scan {
    files: BTreeMap<String, FileState>,
    directories: BTreeMap<String, u32>,
    directory_stamps: BTreeMap<String, Stamp>,
    stamps: BTreeMap<String, Stamp>,
    probes: BTreeMap<String, Option<Vec<u8>>>,
    visited: usize,
    bytes: u64,
    path_bytes: usize,
}

impl Scan {
    fn account_path(&mut self, path: &str) -> io::Result<()> {
        // A deep shared prefix otherwise gets copied into every full-path key,
        // even when all file bodies are empty and the entry count is small.
        self.path_bytes = self
            .path_bytes
            .checked_add(path.len())
            .filter(|bytes| *bytes <= PATH_METADATA_BYTES)
            .ok_or_else(|| invalid("snapshot path metadata exceeds the byte limit"))?;
        Ok(())
    }
}

fn same_scan(first: &Scan, second: &Scan) -> io::Result<()> {
    if first.files != second.files
        || first.directories != second.directories
        || first.directory_stamps != second.directory_stamps
        || first.stamps != second.stamps
    {
        return Err(invalid(
            "workspace changed while capturing; no snapshot was published",
        ));
    }
    Ok(())
}

fn parents(path: &str) -> impl Iterator<Item = &str> {
    Path::new(path)
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_str().expect("UTF-8 relative paths"))
}

fn joined(prefix: &str, leaf: &str) -> String {
    if prefix.is_empty() {
        leaf.to_owned()
    } else {
        format!("{prefix}/{leaf}")
    }
}

fn changes(before: &Snapshot, target: &Snapshot) -> Vec<Change> {
    before
        .files
        .keys()
        .chain(target.files.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.files.get(*path) != target.files.get(*path))
        .map(|path| Change {
            path: path.clone(),
            before: before.files.get(path).cloned(),
            after: target.files.get(path).cloned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_long_prefixes_have_one_byte_budget() {
        let mut scan = Scan::default();
        let path = "a".repeat(64 * 1024);
        for _ in 0..256 {
            scan.account_path(&path).unwrap();
        }
        assert_eq!(scan.path_bytes, PATH_METADATA_BYTES);
        assert!(scan.account_path("x").is_err());
        assert_eq!(
            scan.path_bytes, PATH_METADATA_BYTES,
            "a failed addition must not advance the counter"
        );
    }
}
