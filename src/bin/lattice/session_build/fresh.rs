use super::*;
use crate::terminal_host::{initialization, model_catalog, startup, EffortView};
use lattice::core_events as ce;
use lattice::{EventLog, Session};

#[path = "../../../../tests/common/ledger_snapshot.rs"]
mod snapshot;

#[test]
fn fresh_selection_cannot_adopt_a_closed_ledger_created_before_open() {
    crate::terminal_host::test_support::isolated(|| {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixed-candidate.ledger");
        // Select first; another creator occupies it before this Fresh opens.
        let selected = startup::LedgerSelection::Fresh(path.clone());
        let mut existing = EventLog::open(
            ce::core_event_decls(),
            "existing-stream",
            Some(path.clone()),
        )
        .unwrap();
        existing
            .append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"original history"})),
                "ui",
            )
            .unwrap();
        drop(existing);
        let before = snapshot::tree(root.path());
        let (tx, _) = std::sync::mpsc::channel();
        let result = build_selected(tx, &config(root.path()), selected);
        let error = match result {
            Ok(kernel) => {
                kernel.shutdown();
                None
            }
            Err(error) => Some(error.to_string()),
        };
        // Even a negative control that unexpectedly starts is stopped first.
        assert!(
            error.is_some(),
            "Fresh must reject an occupied candidate, not resume it"
        );
        assert!(error.unwrap().contains(&path.display().to_string()));
        assert_eq!(snapshot::tree(root.path()), before);
        let reopened =
            EventLog::open(ce::core_event_decls(), "existing-stream", Some(path)).unwrap();
        assert_eq!(reopened.len(), 1);
        assert_eq!(
            reopened.replay(1).unwrap()[0].payload["text"],
            "original history"
        );
    });
}

#[test]
fn fresh_identity_reaches_the_ui_and_startup_observation_without_history_replay() {
    crate::terminal_host::test_support::isolated(|| {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("new.ledger");
        let cfg = config(root.path());
        let running = preset::running_entry(&cfg);
        let selected = startup::LedgerSelection::Fresh(path.clone());
        assert!(!selected.reopened());
        let build_cfg = cfg.clone();
        let session =
            Session::spawn("ui", move |tx| build_selected(tx, &build_cfg, selected)).unwrap();
        let actual = session.log_reader().stream().to_owned();
        let mut trace = startup::Trace::start();
        trace.selected(false);
        trace.history_snapshot(0);
        let mut trace = Some(trace);
        let installed = initialization::Main {
            title: "fixture".into(),
            workspace: root.path().display().to_string(),
            effort: EffortView {
                rungs: running.effort_rungs(),
                now: None,
            },
            models: model_catalog::from_config(&cfg),
            running,
            history_end: 0,
            documents: None,
            parts: vec![],
            expert_dir: None,
            stream_id: actual.clone(),
            tab_config: cfg,
            main_ledger: path.clone(),
        }
        .install(&session, &mut trace);
        let note = startup::Trace::first_frame(&mut trace, session.startup_cost());
        session.request_shutdown();
        let closed = session.finish_shutdown().unwrap();
        let (ui, _, _) = installed.unwrap();
        assert!(!actual.is_empty());
        assert_eq!(ui.domain.stream_id, actual);
        assert_eq!(closed.log.stream(), actual);
        assert!(closed.kernel.lingering.is_empty());
        let note = note.unwrap();
        assert!(!note.resumed);
        assert_eq!(note.history_events, 0);
        assert!(!closed
            .log
            .replay(1)
            .unwrap()
            .iter()
            .any(|event| event.event_type == ce::USER_MESSAGE));
        drop(closed);
        // Normal selected-history startup still retains the actual identity.
        let (tx, _) = std::sync::mpsc::channel();
        let reopened = build_selected(
            tx,
            &config(root.path()),
            startup::LedgerSelection::Resume(path),
        )
        .unwrap();
        let resumed_stream = reopened.log().stream().to_owned();
        reopened.shutdown();
        assert_eq!(resumed_stream, actual);
    });
}

#[test]
fn invalid_fresh_preset_does_not_allocate_a_candidate() {
    crate::terminal_host::test_support::isolated(|| {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unallocated.ledger");
        let mut cfg = config(root.path());
        cfg.assembly = Some(root.path().join("missing.json"));
        let (tx, _) = std::sync::mpsc::channel();
        let result = build_selected(tx, &cfg, startup::LedgerSelection::Fresh(path.clone()));
        let rejected = match result {
            Err(KernelError::Inspection(_)) => true,
            Err(_) => false,
            Ok(kernel) => {
                kernel.shutdown();
                false
            }
        };
        assert!(rejected);
        assert!(!path.exists());
    });
}
