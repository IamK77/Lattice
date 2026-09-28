use super::*;
use lattice::components::{context_gate, silent_ui};
use lattice::{core_events as ce, EventDraft, EventLog};

#[test]
fn compact_command_is_control_input_not_a_chat_turn_or_model_change() {
    let (sent, received) = std::sync::mpsc::channel();
    let session = Session::spawn("ui", move |_| {
        let manifest = silent_ui::manifest();
        let registry = [(silent_ui::NAME.into(), manifest)].into();
        let mut factories: std::collections::HashMap<String, lattice::Factory> = [(
            silent_ui::NAME.into(),
            Box::new(|_: Option<&Value>| -> Box<dyn lattice::Component> {
                Box::new(silent_ui::SilentUi::new(Default::default()))
            }) as lattice::Factory,
        )]
        .into();
        let assembly = lattice::AssemblyManifest {
            instances: [(
                "ui".into(),
                lattice::ComponentInstance {
                    component: silent_ui::NAME.into(),
                    requires: vec![],
                    config: None,
                },
            )]
            .into(),
            wires: vec![],
        };
        let mut kernel =
            lattice::Kernel::start(&assembly, &registry, &mut factories, Default::default())?;
        kernel.subscribe_log(move |event| {
            let _ = sent.send(event.clone());
        });
        Ok(kernel)
    })
    .unwrap();
    let reader = session.log_reader();
    let mut ui = Ui::replayed(&[]);
    assert!(slash_catalog::SLASH
        .iter()
        .any(|command| command.name == "/compact"));
    assert!(!run_slash(&mut ui, "/compact", None));
    assert_eq!(ui.flash.as_deref(), Some("no session to compact"));
    assert!(!run_slash(&mut ui, "/compact extra", Some(&session)));
    assert_eq!(ui.flash.as_deref(), Some("usage: /compact (no arguments)"));
    assert!(!run_slash(&mut ui, "/compact", Some(&session)));
    assert!(!ui.busy());
    assert_eq!(ui.entry_count(), 0);
    let event = received
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    session.shutdown();
    assert_eq!(event.event_type, ce::EXTERNAL_INPUT);
    assert_eq!(
        event.payload,
        json!({"channel":context_gate::COMPACT_CHANNEL})
    );
    assert!(event.causes.is_empty());
    let mut controls = Vec::new();
    reader
        .visit_range(1, reader.snapshot_end(), |events| {
            for event in events {
                assert!(!matches!(
                    event.event_type.as_str(),
                    ce::USER_MESSAGE | ce::MODEL_CALL_STARTED
                ));
                if event.event_type == ce::EXTERNAL_INPUT {
                    controls.push(event.clone());
                }
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(controls.len(), 1, "only the valid command is emitted");
    assert_eq!(controls[0].id, event.id);
}

fn append(
    log: &mut EventLog,
    kind: &str,
    causes: &[&str],
    payload: Value,
) -> lattice::EventEnvelope {
    log.append(
        EventDraft::new(kind, causes, payload).with_reason("compaction fixture"),
        "fixture",
    )
    .unwrap()
}

fn request(log: &mut EventLog) -> lattice::EventEnvelope {
    append(
        log,
        ce::MODEL_CALL_STARTED,
        &[],
        json!({
            "model":"fixture", "input":{"parts":[],"fingerprint":"sha256:fixture"},
            "purpose":context_gate::CONDENSE_PURPOSE,
        }),
    )
}

#[test]
fn compaction_pause_survives_terminal_checkpoints_and_updates_at_the_delivered_prefix() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("compaction.ledger");
    let mut declarations = ce::core_event_decls();
    declarations.extend(context_gate::manifest().events);
    let mut log = EventLog::open_segmented(declarations, "fixture", root, 4096).unwrap();
    let call = request(&mut log);
    let failed = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&call.id],
        json!({
            "status":"error", "purpose":context_gate::CONDENSE_PURPOSE,
            "error":{"code":"stream_read_error", "message":"fixture failure", "blame":"provider"},
        }),
    );
    let reader = log.reader();
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&reader, failed.seq).unwrap();
    let expected = ui.compaction_status().unwrap().clone();
    assert_eq!(expected.failure.as_ref().unwrap().event, failed.id);
    assert!(!expected.in_flight);
    let mut warm = Ui::replayed(&[]);
    warm.replay_prefix(&reader, failed.seq).unwrap();
    assert_eq!(warm.recovery.as_ref().unwrap().replayed, 0);
    assert_eq!(warm.compaction_status(), Some(&expected));

    let retry = request(&mut log);
    let cancelled = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&retry.id],
        json!({
            "status":"cancelled", "purpose":context_gate::CONDENSE_PURPOSE,
        }),
    );
    let changed = append(
        &mut log,
        ce::EXTERNAL_INPUT,
        &[],
        json!({"channel":context_gate::MODEL_CHANNEL}),
    );
    // Later records already exist, but the terminal may not display their
    // effects ahead of the event currently being absorbed.
    warm.try_absorb_profiled(&retry, 0, None).unwrap();
    let status = warm.compaction_status().unwrap();
    assert!(status.in_flight);
    assert_eq!(status.failure, expected.failure);
    warm.try_absorb_profiled(&cancelled, 0, None).unwrap();
    assert_eq!(warm.compaction_status(), Some(&expected));
    warm.try_absorb_profiled(&changed, 0, None).unwrap();
    assert!(warm.compaction_status().is_none());
}

#[test]
fn skipped_compaction_has_a_local_notice_without_changing_the_conversation() {
    let mut declarations = ce::core_event_decls();
    declarations.extend(context_gate::manifest().events);
    let mut log = EventLog::in_memory(declarations, "fixture");
    let skipped = log
        .append(
            EventDraft::new(
                context_gate::DECISION,
                &[],
                json!({
                    "scale":"manual", "action":"compact_skipped",
                }),
            )
            .with_reason("Compaction is already in progress"),
            "fixture",
        )
        .unwrap();
    let mut ui = Ui::replayed(&[]);
    ui.absorb(&skipped, 0);
    assert_eq!(
        ui.flash.as_deref(),
        Some("Compaction is already in progress")
    );
    assert_eq!(ui.entry_count(), 0);
    assert!(!ui.busy());
}
