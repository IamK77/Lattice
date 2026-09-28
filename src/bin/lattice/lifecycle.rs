//! Real terminal acquisition and shutdown. The loop borrows resources; it does
//! not acquire them, join sessions, restore terminal modes, or write the index.
use super::*;
use ratatui::crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute, terminal,
};

pub(crate) fn run_tui(resume: Resume, mut startup: StartupTrace) -> std::io::Result<()> {
    let cfg = startup::config()?;

    startup.checkpoint("config");
    // The ledger goes to disk: this session is replayable and auditable
    let (ledger_path, reopened) = startup::ledger_for(&home(), resume)?;
    startup.selected(reopened);
    let ledger_at = ledger_path.clone();
    // The brand's meta line: model · where the tools are working. Confined,
    // that is the workspace; open, the directory lattice was started in.
    let workspace = cfg.workspace.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    let title = format!("{} · {}", cfg.model, workspace);

    // Read off the config before it moves into the session thread: the
    // palette needs this model's rungs and where the dial landed after the
    // environment and the saved preference had their say.
    let effort = EffortView {
        rungs: lattice::preset::running_entry(&cfg).effort_rungs(),
        now: cfg
            .thinking
            .as_ref()
            .map(|value| value.as_str().unwrap_or("off").to_string()),
    };
    let models = model_catalog::from_config(&cfg);
    // Taken before the config moves into the kernel-building closure
    let running = lattice::preset::running_entry(&cfg);
    // The same manifest the kernel is about to be built from, read here so the
    // panel can say what is assembled. Anything the overlay put in is removable;
    // the base assembly is not — see `View::components`.
    let parts = session_build::preview(&cfg);

    // Where a subagent runs. One template per expert, built on the session's
    // thread, reading the environment there just as the daemon does.
    let main_stream = lattice::EventLog::stream_of(&ledger_path).unwrap_or_default();
    let experts: Box<dyn FnOnce() -> lattice::StreamHost + Send> = Box::new(|| {
        let cfg = PresetConfig::from_env();
        // Experts disappear after answering; their ledgers remain readable.
        lattice::preset::expert_host(&cfg).with_ledger_path(|stream| {
            let dir = experts_dir()?;
            std::fs::create_dir_all(&dir).ok();
            Some(lattice::ledgers::named_path(&dir, stream))
        })
    });

    // The host's half of installing a tool: the agent asks, the trust card
    // gets the human's y/n, and this is who actually rewires the kernel.
    startup.checkpoint("prepare");
    let tab_config = cfg.clone();
    let session = Session::spawn_with_subagents(
        "ui",
        Some(lattice::workshop::Workshop::standard()),
        Some((main_stream.clone(), experts)),
        move |render_tx| session_build::build(render_tx, &cfg, ledger_path.clone()),
    )
    .map_err(std::io::Error::other)?;
    startup.checkpoint("session_start");

    // The ledger now exists. Capture before entering raw/full-screen mode,
    // and keep stderr off-screen until all background sessions finish.
    let mut diagnostics =
        diagnostics::Capture::start(&lattice::contracts::document::documents_dir(&ledger_at))?;
    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    // Bracketed paste delivers a paste as one event. With mouse capture,
    // native text selection still uses the terminal's Shift escape hatch.
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    // Subscription is already active. Freeze its overlap boundary without
    // allocating a second object tree for the entire conversation.
    startup.checkpoint("terminal_setup");
    let history_end = if reopened {
        session.log_reader().snapshot_end()
    } else {
        0
    };
    startup.history_snapshot(history_end);
    let (result, children, mut closing) = tui_loop(
        &mut term,
        &session,
        Some(startup),
        initialization::Main {
            title,
            workspace,
            effort,
            models,
            running,
            history_end,
            documents: Some(lattice::contracts::document::documents_dir(&ledger_at)),
            parts,
            expert_dir: experts_dir(),
            stream_id: main_stream.clone(),
            tab_config,
            main_ledger: ledger_at.clone(),
        },
        &mut diagnostics,
    );

    session.request_shutdown();
    closing.timer.checkpoint("request_stop");
    // Attempt every restoration before joining any session, even on failure.
    let [raw_result, screen_result, cursor_result] = restore_all(|step| match step {
        Restore::Raw => terminal::disable_raw_mode(),
        Restore::Screen => execute!(
            term.backend_mut(),
            DisableMouseCapture,
            DisableBracketedPaste,
            terminal::LeaveAlternateScreen
        ),
        Restore::Cursor => term.show_cursor(),
    });
    closing.timer.checkpoint("terminal_restore");
    let children = children.finish();
    closing.timer.checkpoint("children_finish");
    let main = session.finish_shutdown().map_err(std::io::Error::other)?;
    closing.timer.checkpoint("main_finish");
    let entry = lattice::ledgers::summarize_reader(&ledger_at, &main.log.reader());
    closing.timer.checkpoint("index_summary");
    let errors = [
        ("terminal_raw", raw_result.as_ref().err()),
        ("terminal_screen", screen_result.as_ref().err()),
        ("terminal_cursor", cursor_result.as_ref().err()),
        ("frontend", result.as_ref().err()),
        ("index_summary", entry.as_ref().err()),
    ]
    .into_iter()
    .filter_map(|(phase, error)| error.map(|e| format!("{phase}: {e}")))
    .collect();
    let final_event = closing.record(main, children, errors)?;
    let mut entry = entry?;
    // Include the final observation without another history traversal. The
    // observation cannot measure its own append or this small index write.
    if let Some(entry) = entry.as_mut() {
        entry["events"] = json!(final_event.seq);
        entry["last"] = json!(final_event.time);
        entry["bytes"] = json!(std::fs::metadata(&ledger_at)?.len());
        lattice::ledgers::note_entry(&home(), entry)?;
    }
    raw_result?;
    screen_result?;
    cursor_result?;
    diagnostics.restore()?;
    if diagnostics.has_output()? {
        eprintln!("{}", diagnostics.description());
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Restore {
    Raw,
    Screen,
    Cursor,
}

fn restore_all(
    mut restore: impl FnMut(Restore) -> std::io::Result<()>,
) -> [std::io::Result<()>; 3] {
    [
        restore(Restore::Raw),
        restore(Restore::Screen),
        restore(Restore::Cursor),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restoration_attempts_all_modes_in_order_even_when_any_subset_fails() {
        for failures in 0..8 {
            let mut seen = Vec::new();
            let results = restore_all(|step| {
                seen.push(step);
                if failures & (1 << (seen.len() - 1)) != 0 {
                    Err(std::io::Error::other(format!("{step:?}")))
                } else {
                    Ok(())
                }
            });
            assert_eq!(seen, [Restore::Raw, Restore::Screen, Restore::Cursor]);
            for (i, result) in results.into_iter().enumerate() {
                assert_eq!(result.is_err(), failures & (1 << i) != 0);
                if let Err(error) = result {
                    assert_eq!(error.to_string(), format!("{:?}", seen[i]));
                }
            }
        }
    }
}
