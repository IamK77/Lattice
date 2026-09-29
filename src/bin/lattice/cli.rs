//! Interpret process arguments without opening a session or executing commands.
//! Individual command implementations still own their command-specific options.

use std::{ffi::OsString, io, path::PathBuf};

/// Which ledger this launch should write to. Choosing which conversation to
/// continue belongs to the frontend; the runtime receives an ordinary path and
/// performs its existing recovery, regardless of how that path was chosen.
#[derive(Debug, PartialEq, Eq)]
pub enum Resume {
    /// A new one, named for now — the default.
    Fresh,
    /// The most recent one that holds a conversation.
    Latest,
    /// One named outright (a file name, with or without the suffix).
    Named(String),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    Tui(Resume),
    Recover(Vec<String>),
    Migrate(PathBuf),
    Verify(PathBuf),
    Serve,
    Component(Option<String>),
    Prompt,
    Assembly,
    Compact(Vec<String>),
    Index,
    Export(Vec<String>),
    Tidy,
    DebugFrame(Vec<String>),
    DebugTui(Vec<String>),
    Version,
    Help,
    Unknown(String),
}

pub(super) fn parse(
    mode: Option<String>,
    mut tail: impl Iterator<Item = OsString>,
) -> io::Result<Command> {
    Ok(match mode.as_deref() {
        None => Command::Tui(Resume::Fresh),
        Some("-c" | "--continue") => Command::Tui(Resume::Latest),
        Some("--resume") => Command::Tui(match tail.next() {
            Some(name) => Resume::Named(text(name)),
            None => Resume::Latest,
        }),
        Some("--recover") => Command::Recover(tail.map(text).collect()),
        Some("--migrate-ledger") => Command::Migrate(one_path(
            tail,
            "usage: lattice --migrate-ledger OFFLINE_LEGACY.jsonl",
        )?),
        Some("--verify-ledger") => Command::Verify(one_path(
            tail,
            "usage: lattice --verify-ledger SEGMENTED_LEDGER_DIRECTORY",
        )?),
        Some("serve") => Command::Serve,
        Some("component") => Command::Component(tail.next().map(text)),
        Some("prompt") => Command::Prompt,
        Some("assembly") => Command::Assembly,
        Some("compact") => Command::Compact(tail.map(text).collect()),
        Some("index") => Command::Index,
        Some("export") => Command::Export(tail.map(text).collect()),
        Some("tidy") => Command::Tidy,
        Some("debug-frame") => Command::DebugFrame(tail.map(text).collect()),
        Some("debug-tui") => Command::DebugTui(tail.map(text).collect()),
        Some("--version" | "-V" | "version") => Command::Version,
        Some("--help" | "-h" | "help") => Command::Help,
        Some(other) => Command::Unknown(other.to_string()),
    })
}

fn text(value: OsString) -> String {
    // Preserve env::args' strict conversion for consumed textual arguments;
    // maintenance paths deliberately use the original platform bytes instead.
    value.into_string().unwrap()
}

fn one_path(tail: impl Iterator<Item = OsString>, usage: &str) -> io::Result<PathBuf> {
    let mut args: Vec<_> = tail.collect();
    if args.len() != 1 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, usage));
    }
    Ok(args.remove(0).into())
}

pub(super) fn print_help() {
    eprintln!(
        "lattice — a composable, auditable agent runtime\n\n\
         USAGE:\n  \
         lattice          run the in-process terminal UI (default)\n  \
         lattice -c       continue the most recent conversation in the current project\n  \
         lattice --resume [name]\n                   continue a named one in the current project (see ~/.lattice/ledgers/)\n  \
         lattice --recover PATH [--offset BYTES] [--bytes COUNT]\n                   \
         inspect a bounded raw window without credentials, assembly, or writes\n  \
         lattice --migrate-ledger OFFLINE_LEGACY.jsonl\n                   copy to a segmented ledger, retaining original files (requires lsof)\n  \
         lattice --verify-ledger DIRECTORY\n                   verify all segmented ledger source without writes\n  \
         lattice serve    run headless as a daemon on a Unix socket\n  \
         lattice assembly print the selected complete baseline (excludes installs)\n                   \
         LATTICE_ASSEMBLY selects a baseline file; invalid files fail startup.\n  \
         lattice compact [ledger.jsonl…]\n                   \
         move each ledger's documents out beside it (no argument: every\n                   \
         conversation there is). Idempotent; also migrates older records.\n  \
         lattice --version\n                   \
         vX.Y.Z (release) or vX.Y.Z-dev[+g<commit>] (development)\n  \
         lattice index    rebuild ~/.lattice/ledgers/index.jsonl from the ledgers\n  \
         lattice tidy     apply the `ledger` policy in preferences.json\n                   \
         (compactAfterDays / archiveAfterDays / deleteAfterDays; off by default)\n  \
         lattice export <ledger.jsonl> [out.jsonl]\n                   \
         one self-sufficient file, documents folded back in\n  \
         lattice component <name>\n                   \
         run a builtin component as a separate process (bridge child)\n  \
         lattice debug-frame <ledger.jsonl> [--width W --height H --at N]\n                   \
         replay a ledger and print one rendered frame as JSON\n  \
         lattice debug-tui [--width W --height H --json --brain replies.json] [script]\n                   \
         drive the real UI headlessly from a script of actions and print frames\n\n\
         ENV:\n  \
         LATTICE_ADAPTER=anthropic   use the Anthropic wire format (default: openai)\n  \
         LATTICE_MODEL, LATTICE_BASE_URL, LATTICE_API_KEY_ENV\n  \
         LATTICE_THINKING=high|max|off  main-model thinking (saved preference or high;\n                              \
         empty = send no thinking parameter at all)\n  \
         LATTICE_SCRIPTED=1          keyless deterministic run (any value enables; unset to disable)\n  \
         prompt                      print the system prompt and tool list, then exit\n  \
         LATTICE_WORKSPACE=DIR       workspace for bundled tools; not an OS sandbox\n                              \
         (default: tools work from where you started lattice)"
    );
}

#[cfg(test)]
#[path = "cli/tests.rs"]
mod tests;
