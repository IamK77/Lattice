//! Where the agent is standing — the half of the system prompt that is not a
//! component describing itself.
//!
//! An agent that cannot see its own surroundings spends its first turn asking
//! for them, and guesses wrong when it doesn't: it writes yesterday's date,
//! reaches for GNU flags on a BSD userland, or works relative to a directory
//! it never confirmed. None of that is reasoning it should have to do.
//!
//! WHAT GOES IN IS DECIDED BY THE PROMPT CACHE. The system prompt is the
//! cached prefix of every single call; change one character and the provider
//! recomputes the whole thing. So this fragment carries only facts that hold
//! still for as long as the process lives — the directory, the platform, the
//! shell, the date, whether this is a repository. The volatile neighbours of
//! those facts (the branch, whether the tree is dirty, what is in a directory
//! right now) are deliberately absent: they would re-cost the prefix every
//! time they moved, and every one of them is one cheap tool call away. The
//! fragment says so out loud, so their absence reads as a boundary rather
//! than as silence.
//!
//! Probing happens once, in `restore`, and the answer never changes after —
//! not even if the process later moves. A stale directory line would be a bug
//! worth fixing; a directory line that rewrites the cached prefix mid-session
//! is a tax on every remaining turn.

use serde_json::Value;

use crate::contracts::component::{ComponentManifest, RuntimeKind};
use crate::contracts::event::EventEnvelope;
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "environment";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        // It listens to nothing and says nothing. Its whole contribution is a
        // prompt fragment, which is a general capability of any component —
        // the core has no notion of "environment" and needs none.
        inputs: Vec::new(),
        outputs: Vec::new(),
        events: Vec::new(),
        default_wiring: Vec::new(),
        // Reading the working directory and the clock is not an effect worth
        // declaring: no file is opened, nothing outside the process is touched.
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        // Filled in at startup, once, by `restore` — a manifest is built
        // before there is a process to describe.
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

#[derive(Default)]
pub struct Environment {
    /// Override for the probe, so a test can state the surroundings instead
    /// of inheriting the machine it happens to run on.
    fixed: Option<String>,
}

impl Environment {
    pub fn from_config(config: Option<&Value>) -> Self {
        Self {
            fixed: config
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

impl Component for Environment {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}

    fn restore(&mut self, ctx: &mut Ctx) {
        // The ledger's address comes from the kernel, not from config: the
        // daemon builds ONE assembly for many streams, each with its own
        // ledger file, so a configured path would be right for at most one
        // of them.
        let ledger = ctx.ledger_path().map(|p| p.display().to_string());
        let text = self
            .fixed
            .clone()
            .unwrap_or_else(|| probe(ledger.as_deref()));
        ctx.set_prompt(Some(text));
    }
}

/// File-tool instructions shared by the current stream and parent excerpts.
/// Do not equate a logical event sequence with a physical legacy-file line.
pub(super) fn ledger_reading_note(path: &std::path::Path) -> String {
    let segmented = path.is_dir() || path.extension().is_some_and(|ext| ext == "ledger");
    let location = if segmented {
        "This is a segmented ledger directory. Read catalog.json there to locate an event: \
         segments lists each volume's number and first sequence; the next volume's first \
         sequence ends the preceding range. A volume is named with its number padded to \
         20 decimal digits plus .jsonl. Within that volume, physical line = event seq - \
         volume first + 1. Use the seq field as the authority; do not treat an arbitrary \
         event id as a sequence. Search the volume JSONL files by exact id if needed."
    } else {
        "This is a legacy JSONL file. Locate an event by its exact id with Grep, then use \
         Read from=<reported line> limit=1. IDs generated as ev_42_... carry logical sequence \
         42, not an unconditional physical line number: legacy files can contain blank \
         lines, and imported event IDs need not follow this naming convention."
    };
    let documents = crate::contracts::document::documents_dir(path);
    format!(
        "{location}\n  Read original records with ordinary file tools; do not edit the ledger. \
         Large prompts, tool lists and outputs may be document references rather than inline \
         text. Relative document names resolve under {}. Follow the reference's file, size \
         and preview; older records can also contain absolute artifact paths, which must \
         be followed as recorded rather than rewritten to this directory.",
        serde_json::json!(documents)
    )
}

/// The surroundings, as one prompt fragment.
fn probe(ledger: Option<&str>) -> String {
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
    let today = chrono::Local::now().format("%Y-%m-%d");

    let mut lines = vec![
        "Where you are:".to_string(),
        format!("- Working directory: {cwd}"),
        format!(
            "- Platform: {} ({})",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        format!("- Shell: {shell}"),
        format!("- Today: {today}"),
    ];
    if let Some(root) = git_root() {
        lines.push(format!("- Git repository, rooted at {root}"));
    }
    // The ledger, by name. The product prompt already tells the agent that
    // its own record is a file and that `Run` reaches it, which was true and
    // useless: asked to look at its own ledger it went to `git log`, because
    // nothing had ever said where the file was. An address is what makes the
    // claim actionable, and it costs one cached line.
    if let Some(path) = ledger {
        lines.push(format!(
            "- This conversation's ledger: {path}\n  \
             It retains the original history, including text later omitted from model context.\n  {}",
            ledger_reading_note(std::path::Path::new(path))
        ));
    }
    lines.push(
        "Everything above was read once, when this process started, and does not \
         update. Anything that MOVES — the branch, whether the tree is dirty, what a \
         directory holds right now — was left out on purpose rather than left stale; \
         look those up when you need them."
            .to_string(),
    );
    lines.join("\n")
}

/// The repository root, found by walking up for a `.git` — no `git` process
/// spawned, because startup should not depend on a program being installed.
fn git_root() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.display().to_string());
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn ledger_reading_notes_distinguish_sequences_volumes_and_document_roots() {
        let legacy = ledger_reading_note(Path::new("/fixture/history.jsonl"));
        assert!(legacy.contains("legacy JSONL file"));
        assert!(legacy.contains("from=<reported line>"));
        assert!(legacy.contains("blank lines"));
        assert!(legacy.contains("\"/fixture/history\""));
        assert!(!legacy.contains("catalog.json"));
        let segmented = ledger_reading_note(Path::new("/fixture/history.ledger"));
        assert!(segmented.contains("catalog.json"));
        assert!(segmented.contains("line = event seq - volume first + 1"));
        assert!(segmented.contains("20 decimal digits"));
        assert!(segmented.contains("\"/fixture/history.ledger/documents\""));
        assert!(segmented.contains("absolute artifact paths"));
        let temp = tempfile::tempdir().unwrap();
        assert!(ledger_reading_note(temp.path()).contains("segmented ledger directory"));
    }
}
