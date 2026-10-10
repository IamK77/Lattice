//! Real terminal acquisition and shutdown. The loop borrows resources; it does
//! not acquire them, join sessions, restore terminal modes, or write the index.
use super::*;
use ratatui::crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute, terminal,
};

pub(crate) fn run_tui(resume: Resume, mut startup: StartupTrace) -> std::io::Result<()> {
    // Resolve existing history read-only before a potentially long wizard.
    // Fresh allocation must wait until the user actually completes setup.
    let selected = startup::ConversationSelection::capture(&home(), resume)?;
    let Some(cfg) = crate::setup::prepare()? else {
        return Ok(());
    };
    startup.checkpoint("setup");
    startup::report_catalog(&cfg);
    startup::ensure_workspace(cfg.workspace.as_ref())?;
    startup.checkpoint("config");
    let selected = selected.finish(&home())?;
    let reopened = selected.reopened();
    startup.selected(reopened);
    let ledger_at = selected.path().to_owned();
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
    // thread from the frontend's final model selection.
    let expert_cfg = cfg.clone();
    let experts: Box<dyn FnOnce() -> lattice::StreamHost + Send> = Box::new(move || {
        // Keep the model explicitly selected in setup, including launch-only overrides.
        // Experts disappear after answering; their ledgers remain readable.
        lattice::preset::expert_host(&expert_cfg).with_ledger_path(|stream| {
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
        // Session takes the actual stream from its kernel, not this legacy hint.
        Some((String::new(), experts)),
        move |render_tx| session_build::build_selected(render_tx, &cfg, selected),
    )
    .map_err(std::io::Error::other)?;
    let main_stream = session.log_reader().stream().to_owned();
    startup.checkpoint("session_start");

    // The ledger now exists. Capture before entering raw/full-screen mode,
    // and keep stderr off-screen until all background sessions finish.
    let acquire = (|| -> std::io::Result<_> {
        let diagnostics =
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
        let term = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;
        Ok((diagnostics, term))
    })();
    let (mut diagnostics, mut term) = match acquire {
        Ok(resources) => resources,
        Err(error) => {
            session.request_shutdown();
            let restored = restore_all(|step| match step {
                Restore::Raw => terminal::disable_raw_mode(),
                Restore::Screen => execute!(
                    std::io::stdout(),
                    DisableMouseCapture,
                    DisableBracketedPaste,
                    terminal::LeaveAlternateScreen
                ),
                Restore::Cursor => execute!(std::io::stdout(), ratatui::crossterm::cursor::Show),
            });
            for failure in restored.into_iter().filter_map(Result::err) {
                eprintln!("terminal restoration failed: {failure}");
            }
            if let Err(failure) = session.finish_shutdown() {
                eprintln!("startup cleanup failed: {failure}");
            }
            return Err(error);
        }
    };

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
