//! Launch prerequisites and locations, plus the one-shot first-frame observation.
//! This module does not build a kernel, enter terminal modes, or install UI state.

use crate::cli::Resume;
use lattice::preset::PresetConfig;
use std::path::{Path, PathBuf};

/// Keep failure order shared by the terminal and daemon. The prompt inspector
/// deliberately uses only `report_catalog`, not these launch prerequisites.
pub(super) fn config() -> std::io::Result<PresetConfig> {
    let cfg = PresetConfig::from_env();
    require_key(&cfg);
    report_catalog(&cfg);
    ensure_workspace(cfg.workspace.as_ref())?;
    Ok(cfg)
}

/// Refuse to start when the chosen model has no key. Describe the configured
/// choices rather than guessing a provider or a key variable for the user.
fn require_key(cfg: &PresetConfig) {
    if cfg.adapter == "scripted" || !cfg.key_env.is_empty() && std::env::var(&cfg.key_env).is_ok() {
        return;
    }
    let catalog = lattice::models::load();
    let reachable = catalog.iter().filter(|e| e.key_present()).count();
    if reachable > 0 {
        eprintln!(
            "{:?} has no key, and this launch asked for it.\n",
            cfg.model
        );
    } else {
        eprintln!("no model this installation can reach.\n");
    }
    if catalog.is_empty() {
        eprintln!("  the model catalog is empty");
    } else {
        eprintln!("  configured models:");
        for entry in &catalog {
            let key = if entry.key_env.is_empty() {
                "names no key".to_string()
            } else if entry.key_present() {
                format!("{} is set", entry.key_env)
            } else {
                format!("{} is NOT set", entry.key_env)
            };
            let mark = if entry.key_present() { "✓" } else { " " };
            eprintln!("   {mark} {:<12} {:<24} {}", entry.id, entry.model, key);
        }
    }
    let where_to = lattice::models::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.lattice/models.json".to_string());
    eprintln!();
    eprintln!("  set the key of one of them, or write a model into the catalog:");
    eprintln!();
    eprintln!("    {where_to}");
    eprintln!();
    eprintln!("      {{\"models\": {{\"my-model\": {{");
    eprintln!("          \"adapter\": \"openai\", \"model\": \"…\",");
    eprintln!("          \"baseUrl\": \"https://…\", \"apiKey\": \"…\"");
    eprintln!("      }}}}}}");
    eprintln!();
    eprintln!("  `apiKey` holds the key itself; `apiKeyEnv` holds the NAME of a");
    eprintln!("  variable holding it. A key in the file is a key on disk that this");
    eprintln!("  agent can read — see the note in schemas/model_catalog.json.");
    eprintln!();
    eprintln!("  Or run with no model at all: LATTICE_SCRIPTED=1");
    std::process::exit(1);
}

/// Report malformed catalog entries while there is still a plain terminal.
pub(super) fn report_catalog(cfg: &PresetConfig) {
    for problem in &cfg.catalog_problems {
        eprintln!("warning: {problem}");
    }
}

/// Filesystem tools and shell children start here when confined. Without an
/// explicit workspace they use the existing process directory; create nothing.
fn ensure_workspace(workspace: Option<&String>) -> std::io::Result<()> {
    match workspace {
        Some(dir) => std::fs::create_dir_all(dir),
        None => Ok(()),
    }
}

pub(super) fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// The expert host and the panel must agree on this location. Unlike `home`,
/// an absent or non-text HOME leaves this location unknown, not relative.
pub(super) fn experts_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".lattice").join("experts"))
}

/// Real conversations, newest by name rather than modification time. The
/// shared ledger index handles empty files and all supported layouts. Project
/// selection uses the process directory, not the tool confinement setting.
fn past_ledgers(home: &Path) -> Vec<PathBuf> {
    match std::env::current_dir() {
        Ok(cwd) => lattice::ledgers::here(home, &cwd),
        Err(_) => lattice::ledgers::all(home),
    }
}

/// Choose a ledger without opening it. A fresh launch creates only its parent.
pub(super) fn ledger_for(home: &Path, resume: Resume) -> std::io::Result<(PathBuf, bool)> {
    match resume {
        Resume::Fresh => {
            let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
            let dir = lattice::ledgers::dir(home);
            std::fs::create_dir_all(&dir)?;
            Ok((
                dir.join(lattice::ledgers::name_for(&stamp, "tui", None)),
                false,
            ))
        }
        Resume::Latest => match std::env::current_dir()
            .map(|cwd| lattice::ledgers::latest_here(home, &cwd))
            .unwrap_or_else(|_| lattice::ledgers::all(home).into_iter().next())
        {
            Some(path) => Ok((path, true)),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "no conversation to continue — nothing yet under {}",
                    home.join(".lattice").display()
                ),
            )),
        },
        Resume::Named(which) => {
            let bare = which
                .strip_suffix(".jsonl")
                .or_else(|| which.strip_suffix(".ledger"))
                .unwrap_or(&which);
            // Across layouts, still within this project's visible conversations.
            if let Some(path) = past_ledgers(home)
                .into_iter()
                .find(|p| p.file_stem().is_some_and(|s| s == bare))
            {
                return Ok((path, true));
            }
            let recent: Vec<String> = past_ledgers(home)
                .iter()
                .take(5)
                .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect();
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "no conversation called {bare:?}. The most recent are:\n  {}",
                    recent.join("\n  ")
                ),
            ))
        }
    }
}

pub(super) struct Trace {
    started_at: String,
    timer: lattice::startup::PhaseTimer,
    resumed: bool,
    history_events: usize,
    replay_memory: lattice::memory::Breakdown,
}

impl Trace {
    /// Called before argument interpretation, not when entering terminal mode.
    pub fn start() -> Self {
        let timer = lattice::startup::PhaseTimer::start();
        Self {
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            timer,
            resumed: false,
            history_events: 0,
            replay_memory: Default::default(),
        }
    }

    pub fn checkpoint(&mut self, phase: &str) {
        self.timer.checkpoint(phase);
    }

    pub fn selected(&mut self, reopened: bool) {
        self.resumed = reopened;
        self.checkpoint("ledger_select");
    }

    /// The caller freezes this boundary only after subscription and terminal setup.
    pub fn history_snapshot(&mut self, through: u64) {
        self.history_events = through as usize;
        self.checkpoint("history_snapshot");
    }

    pub fn replay_memory(&mut self) -> &mut lattice::memory::Breakdown {
        &mut self.replay_memory
    }

    /// Consume only after a successful draw. This neither writes the note nor
    /// claims that the display hardware has presented the frame.
    pub fn first_frame(
        pending: &mut Option<Self>,
        kernel: &lattice::startup::Timings,
    ) -> Option<lattice::session::StartupNote> {
        pending.take().map(|trace| lattice::session::StartupNote {
            started_at: trace.started_at,
            first_frame_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            resumed: trace.resumed,
            history_events: trace.history_events,
            frontend: trace.timer.finish("first_draw"),
            kernel: kernel.clone(),
            replay_memory: trace.replay_memory,
            history: serde_json::Value::Null,
            ui_counts: serde_json::Value::Null,
        })
    }
}

#[cfg(test)]
#[path = "startup/tests.rs"]
mod tests;
