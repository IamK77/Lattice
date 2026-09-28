//! The project's own rules, put where the agent cannot lose them.
//!
//! Repository conventions from CLAUDE.md or AGENTS.md belong in a persistent
//! prompt fragment. Reading them once as ordinary conversation is not enough:
//! context selection can later discard that earlier tool result.
//!
//! So this is not a reminder to obey them. It is the rules themselves, in the
//! system prompt, on every call — a thing the agent cannot forget because it
//! is never asked to remember. A rule that must hold every turn belongs in the
//! structure, not in the model's recollection.
//!
//! Read once at startup and never re-read, for the same reason the environment
//! is probed once: the system prompt is the cached prefix of every call, and a
//! fragment that changes mid-session re-costs the whole prefix. A rules file
//! that changes while the agent is running is the rare case, and the next
//! start picks it up.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::contracts::component::{ComponentManifest, RuntimeKind};
use crate::contracts::event::EventEnvelope;
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "project-rules";

/// The filenames looked for, nearest directory first. CLAUDE.md is what this
/// project uses; AGENTS.md is the name the wider ecosystem is settling on.
pub const DEFAULT_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md"];

/// Above this, the file is named but not inlined — a rules file this large is
/// more likely a manual, and half a rule in the prompt is worse than none.
const DEFAULT_MAX_BYTES: usize = 65_536;

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: Vec::new(),
        outputs: Vec::new(),
        events: Vec::new(),
        default_wiring: Vec::new(),
        // Reading one file at startup, inside the directory the agent already
        // works in. Nothing here is worth a policy's attention.
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        // Filled in at startup by `restore`: there is no project to read from
        // until there is a process standing in one.
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

pub struct ProjectRules {
    files: Vec<String>,
    /// Where to start looking; the search walks up from here.
    from: Option<PathBuf>,
    max_bytes: usize,
}

impl ProjectRules {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let files = get("files")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| DEFAULT_FILES.iter().map(|f| f.to_string()).collect());
        Self {
            files,
            from: get("from").and_then(Value::as_str).map(PathBuf::from),
            max_bytes: get("maxBytes")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_MAX_BYTES),
        }
    }
}

impl Component for ProjectRules {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}

    fn restore(&mut self, ctx: &mut Ctx) {
        let start = self
            .from
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        ctx.set_prompt(fragment(&start, &self.files, self.max_bytes));
    }
}

/// The fragment for a project, or None when it has written nothing down.
pub fn fragment(start: &Path, files: &[String], max_bytes: usize) -> Option<String> {
    let (dir, found) = nearest_with_rules(start, files)?;
    let mut sections = Vec::new();
    for name in found {
        let path = dir.join(&name);
        let size = std::fs::metadata(&path).map(|m| m.len() as usize).ok()?;
        if size > max_bytes {
            // Named, not inlined. Silence would read as "this project has no
            // rules", which is the one wrong thing to say here.
            sections.push(format!(
                "{name} holds this project's rules, but it is too large to include here \
                 ({size} bytes). Read it before you change anything.",
            ));
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => {
                sections.push(format!("--- {name} ---\n{}", demote(text.trim_end())));
            }
            _ => {}
        }
    }
    if sections.is_empty() {
        return None;
    }
    Some(format!(
        "## This project's own rules\n\n\
         They bind you. Where they disagree with your habits, they win; where they are \
         silent, your judgement stands. The file may address some earlier reader by \
         name, or be named after one — the rules are yours to follow, the name is not \
         yours to take.\n\n{}",
        sections.join("\n\n")
    ))
}

/// Push every heading in the included file two levels down, so a rules file
/// that opens with `# Project` does not become a sibling of the prompt's own
/// `# Who you are` and quietly adopt everything printed after it. Lines inside
/// fenced code blocks are left alone: a `# comment` in a shell example is not
/// a heading, and mangling it would corrupt an instruction rather than nest it.
fn demote(text: &str) -> String {
    let mut fenced = false;
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                fenced = !fenced;
                return line.to_string();
            }
            if !fenced && trimmed.starts_with('#') {
                return format!("##{line}");
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Walk up from `start` and return the first directory holding any of these
/// files, with the names it holds. Walking up is what makes it work when
/// lattice is started in a subdirectory of the project rather than its root.
fn nearest_with_rules(start: &Path, files: &[String]) -> Option<(PathBuf, Vec<String>)> {
    let mut dir = start.to_path_buf();
    loop {
        let here: Vec<String> = files
            .iter()
            .filter(|name| dir.join(name).is_file())
            .cloned()
            .collect();
        if !here.is_empty() {
            return Some((dir, here));
        }
        if !dir.pop() {
            return None;
        }
    }
}
