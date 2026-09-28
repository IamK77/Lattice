use super::*;
use lattice::{core_events as ce, EventDraft, EventLog};
use serde_json::{json, Value};

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, payload), "fixture")
        .unwrap()
}

/// Run only on a disposable copy: recovery publishes derived cache files.
/// This exercises the real frontend projection without starting components,
/// contacting a model, or executing historical tool calls.
#[test]
#[ignore = "requires LATTICE_RECOVERY_FIXTURE pointing to a disposable segmented ledger"]
fn offline_ledger_recovery_matches_full_replay() {
    let root = std::path::PathBuf::from(
        std::env::var_os("LATTICE_RECOVERY_FIXTURE").expect("set LATTICE_RECOVERY_FIXTURE"),
    );
    let start = std::time::Instant::now();
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let through = reader.snapshot_end();
    eprintln!(
        "snapshot opened in {:?}, boundary {through}",
        start.elapsed()
    );
    let mut reference = Ui::replayed(&[]);
    reader
        .visit_range(1, through, |batch| {
            for event in batch {
                reference.absorb(event, SETTLED_TICK);
            }
            Ok(())
        })
        .unwrap();
    eprintln!("reference replay finished in {:?}", start.elapsed());
    let cold = std::time::Instant::now();
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&reader, through).unwrap();
    assert_state(&ui, &reference);
    eprintln!(
        "indexed recovery: {:?}, replayed {}",
        cold.elapsed(),
        ui.recovery.as_ref().unwrap().replayed
    );
    let warm = std::time::Instant::now();
    let mut restored = Ui::replayed(&[]);
    restored.replay_prefix(&reader, through).unwrap();
    assert_state(&restored, &reference);
    assert_eq!(restored.recovery.as_ref().unwrap().replayed, 0);
    eprintln!("warm recovery: {:?}", warm.elapsed());
}

// Exact field order from the release-bound v2 Initial, independent of the
// current release-independent context serializer.
fn release_bound_context(initial: &Initial, release: &str) -> [u8; 32] {
    #[derive(Serialize)]
    struct OldInitial<'a> {
        program_version: &'a str,
        title: &'a String,
        models: &'a view::ModelView,
        running: &'a lattice::models::Entry,
        effort: &'a view::EffortView,
        effective_window: Option<u64>,
        expert_dir: &'a Option<std::path::PathBuf>,
        stream_id: &'a String,
    }
    Sha256::digest(
        serde_json::to_vec(&OldInitial {
            program_version: release,
            title: &initial.title,
            models: &initial.models,
            running: &initial.running,
            effort: &initial.effort,
            effective_window: initial.effective_window,
            expert_dir: &initial.expert_dir,
            stream_id: &initial.stream_id,
        })
        .unwrap(),
    )
    .into()
}

#[test]
fn old_release_bound_projection_is_rebuilt_after_semantics_change() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "ui-upgrade", root.clone(), 4096).unwrap();
    let event = append(
        &mut log,
        ce::USER_MESSAGE,
        &[],
        json!({"text":"preserved history"}),
    );
    let mut reference = Ui::replayed(&[]);
    reference.absorb(&event, SETTLED_TICK);
    let initial = Initial::of(&Ui::replayed(&[]));
    assert!(serde_json::to_value(&initial)
        .unwrap()
        .get("program_version")
        .is_none());
    let stable: [u8; 32] = Sha256::digest(serde_json::to_vec(&initial).unwrap()).into();
    let legacy = release_bound_context(&initial, "v0.149.5-35cdd84");
    assert_ne!(stable, legacy);
    let mut first = Ui::replayed(&[]);
    first.replay_prefix(&log.reader(), event.seq).unwrap();
    let mut saved: Saved = log
        .reader()
        .load_checkpoint("terminal-state", VERSION, event.seq)
        .unwrap()
        .state
        .unwrap();
    saved.context = legacy;
    log.reader()
        .save_checkpoint("terminal-state", 2, event.seq, &saved)
        .unwrap();
    drop(first);
    drop(log);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let mut upgraded = Ui::replayed(&[]);
    upgraded.replay_prefix(&reader, event.seq).unwrap();
    assert_state(&upgraded, &reference);
    assert_eq!(upgraded.recovery.as_ref().unwrap().replayed, event.seq);
    assert!(upgraded.recovery.as_ref().unwrap().cold_reason.is_some());
    let rewritten: Saved = reader
        .load_checkpoint("terminal-state", VERSION, event.seq)
        .unwrap()
        .state
        .unwrap();
    assert_eq!(rewritten.context, stable);

    // Old releases, changed interpretation inputs and incompatible state
    // versions must not reuse a projection with different semantics.
    for (context, version, title) in [
        (
            release_bound_context(&initial, "v0.0.0-unreviewed"),
            2,
            initial.title.clone(),
        ),
        (legacy, 2, "changed context".to_string()),
        (stable, VERSION + 1, initial.title.clone()),
    ] {
        saved.context = context;
        reader
            .save_checkpoint("terminal-state", version, event.seq, &saved)
            .unwrap();
        let mut changed = Ui::replayed(&[]);
        changed.domain.title = title;
        changed.replay_prefix(&reader, event.seq).unwrap();
        assert_eq!(changed.recovery.as_ref().unwrap().replayed, event.seq);
        assert!(changed.recovery.as_ref().unwrap().cold_reason.is_some());
    }
}

#[test]
fn cancellation_receipts_never_start_commands_and_old_phantoms_are_rebuilt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "background-fix", root.clone(), 4096)
            .unwrap();
    for job in [1, 2] {
        let call = format!("ask-{job}");
        let request = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":call,"tool":"ask","arguments":{"expert":"explorer","prompt":"inspect"}}),
        );
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&request.id],
            json!({"call":call,"status":"ok","result":{"job":job}}),
        );
        let call = format!("cancel-{job}");
        let cancel = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":call,"tool":"CancelExpert","arguments":{"job":job}}),
        );
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&cancel.id],
            json!({"call":call,"status":"ok","result":{"job":job,"cancellation":"requested"}}),
        );
        append(
            &mut log,
            ce::WAKE,
            &[&request.id],
            json!({"source":format!("expert:{job}"),"summary":"cancelled","body":{"job":job}}),
        );
    }
    let reader = log.reader();
    let through = reader.snapshot_end();
    let mut first = Ui::replayed(&[]);
    first.replay_prefix(&reader, through).unwrap();
    assert!(
        first.domain.background.rows().is_empty(),
        "cancellation acknowledgements must not create commands"
    );
    let saved: Saved = reader
        .load_checkpoint("terminal-state", VERSION, through)
        .unwrap()
        .state
        .unwrap();
    let mut old = serde_json::to_value(saved).unwrap();
    old["state"]["background"] = json!([
        {"kind":"command","key":"background:1","label":"","standing":false,"fires":0,"ledger":null},
        {"kind":"command","key":"background:2","label":"","standing":false,"fires":0,"ledger":null}
    ]);
    reader
        .save_checkpoint("terminal-state", 3, through, &old)
        .unwrap();
    drop(reader);
    drop(log);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let mut repaired = Ui::replayed(&[]);
    repaired.replay_prefix(&reader, through).unwrap();
    assert!(
        repaired.domain.background.rows().is_empty(),
        "v3 phantom rows must not survive recovery"
    );
    assert_eq!(repaired.recovery.as_ref().unwrap().replayed, through);
    let mut warm = Ui::replayed(&[]);
    warm.replay_prefix(&reader, through).unwrap();
    assert!(warm.domain.background.rows().is_empty());
    assert_eq!(warm.recovery.as_ref().unwrap().replayed, 0);
}

#[test]
fn synchronous_expert_phantoms_are_absent_live_and_rebuilt_from_old_checkpoints() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "sync-experts", root.clone(), 4096)
            .unwrap();
    let mut live = Ui::replayed(&[]);
    for (job, background) in [
        (18, false),
        (19, false),
        (20, false),
        (21, false),
        (22, true),
    ] {
        let call = format!("ask-{job}");
        let request = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":call,"tool":"ask","arguments":{"expert":"worker","background":background}}),
        );
        let result = if background {
            json!({"job":job})
        } else {
            json!({"job":job,"text":"done","failed":null,"interrupted":null})
        };
        let completed = append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&request.id],
            json!({"call":call,"status":"ok","result":result}),
        );
        live.note_background(&request, 4);
        live.note_background(&completed, 9);
    }
    let reader = log.reader();
    let through = reader.snapshot_end();
    let mut initial = Ui::replayed(&[]);
    initial.replay_prefix(&reader, through).unwrap();
    let saved: Saved = reader
        .load_checkpoint("terminal-state", VERSION, through)
        .unwrap()
        .state
        .unwrap();
    let mut old = serde_json::to_value(saved).unwrap();
    for job in 18..22 {
        old["state"]["background"].as_array_mut().unwrap().push(json!({"kind":"expert","key":format!("expert:{job}"),"label":"old synchronous result","standing":false,"fires":0,"ledger":null}));
    }
    reader
        .save_checkpoint("terminal-state", 4, through, &old)
        .unwrap();
    drop(reader);
    drop(log);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let mut repaired = Ui::replayed(&[]);
    repaired.replay_prefix(&reader, through).unwrap();
    let keys = |ui: &Ui| {
        ui.domain
            .background
            .rows()
            .iter()
            .map(|row| row.key.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&repaired),
        ["expert:22"],
        "old synchronous phantoms must not survive recovery"
    );
    assert_eq!(repaired.recovery.as_ref().unwrap().replayed, through);
    assert_eq!(keys(&live), ["expert:22"]);
    let mut warm = Ui::replayed(&[]);
    warm.replay_prefix(&reader, through).unwrap();
    assert_eq!(keys(&warm), ["expert:22"]);
    assert_eq!(warm.recovery.as_ref().unwrap().replayed, 0);
}

fn assert_state(ui: &Ui, reference: &Ui) {
    assert!(ui.entries.is_empty());
    let actual = &ui.domain;
    let expected = &reference.domain;
    // Compare fields directly: using State::capture on both sides would hide
    // the same missing or misassigned snapshot field from this test.
    assert_eq!(actual.title, expected.title);
    assert_eq!(actual.skills, expected.skills);
    assert_eq!(actual.background.rows(), expected.background.rows());
    assert_eq!(actual.model.running(), expected.model.running());
    assert_eq!(
        actual.model.effective_window(),
        expected.model.effective_window()
    );
    assert_eq!(actual.unseen.lines(), expected.unseen.lines());
    assert_eq!(actual.turns.busy(), expected.turns.busy());
    assert_eq!(actual.turns.waiting(), expected.turns.waiting());
    assert_eq!(actual.turns.done_at(), expected.turns.done_at());
    assert_eq!(actual.turns.number(), expected.turns.number());
    assert_eq!(actual.turns.history(), expected.turns.history());
    assert_eq!(
        actual.authorizations.history(),
        expected.authorizations.history()
    );
    assert_eq!(
        actual.authorizations.answerable_count(),
        expected.authorizations.answerable_count()
    );
    assert_eq!(
        actual.accounting.last_call(),
        expected.accounting.last_call()
    );
    assert_eq!(
        actual.accounting.turn_total(),
        expected.accounting.turn_total()
    );
    assert_eq!(
        actual.accounting.session_total(),
        expected.accounting.session_total()
    );
    assert_eq!(actual.accounting.parts(), expected.accounting.parts());
    assert_eq!(actual.accounting.previous(), expected.accounting.previous());
    assert_eq!(
        actual.accounting.turn_growth(),
        expected.accounting.turn_growth()
    );
    assert_eq!(
        actual.accounting.session_growth(),
        expected.accounting.session_growth()
    );
    assert_eq!(
        serde_json::to_value(actual.model.catalog()).unwrap(),
        serde_json::to_value(expected.model.catalog()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(actual.model.effort()).unwrap(),
        serde_json::to_value(expected.model.effort()).unwrap()
    );
    assert_eq!(
        actual.accounting.indexed_history().unwrap(),
        expected.accounting.reference_history()
    );
    assert!(actual.accounting.reference_history().is_empty());
    assert_eq!(actual.event_inputs.counts(), [0; 3]);
}

fn pre_accounting_v4_copy(destination: &std::path::Path) -> LogReader {
    // This is an actual pre-refactor ledger and its existing checkpoint, not
    // a checkpoint written by whichever implementation this test is testing.
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/terminal-state-v4");
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json" || extension == "jsonl")
        {
            std::fs::copy(&path, destination.join(path.file_name().unwrap())).unwrap();
        }
    }
    // Snapshot readers require the ledger's empty lock file to exist. This is
    // a fresh disposable copy, not the source ledger or an active writer.
    std::fs::File::create_new(destination.join(".writer.lock")).unwrap();
    LogReader::segmented_snapshot(destination).unwrap()
}

#[test]
fn pre_accounting_v4_checkpoint_rebuilds_once_then_restores_without_replaying() {
    let temp = tempfile::tempdir().unwrap();
    let reader = pre_accounting_v4_copy(&temp.path().join("copied.ledger"));
    assert_eq!(reader.snapshot_end(), 4);
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&reader, 4).unwrap();
    let recovery = ui.recovery.as_ref().unwrap();
    assert!(recovery.cold_reason.is_some());
    assert_eq!(
        recovery.replayed, 4,
        "v4 predates synchronous expert filtering"
    );
    let mut warm = Ui::replayed(&[]);
    warm.replay_prefix(&reader, 4).unwrap();
    assert!(warm.recovery.as_ref().unwrap().cold_reason.is_none());
    assert_eq!(warm.recovery.as_ref().unwrap().replayed, 0);
    let ui = warm;
    assert_eq!(ui.domain.turns.number(), 1);
    assert!(!ui.domain.turns.busy() && ui.domain.turns.done_at().is_some());
    let report = lattice::View::usage(&ui).unwrap();
    for usage in [report.call, report.turn, report.session] {
        assert_eq!(
            (usage.prompt, usage.output, usage.millis, usage.calls),
            (17, 5, 5, 1)
        );
    }
    assert_eq!(
        ui.domain.accounting.parts(),
        &vec![
            ("what you said", 26),
            ("system prompt", 22),
            ("tool declarations", 2)
        ]
    );
    assert_eq!(ui.domain.accounting.previous().len(), 3);
    assert_eq!(
        ui.domain.accounting.turn_growth().get("what you said"),
        Some(&26)
    );
    assert_eq!(
        ui.domain.accounting.session_growth().get("what you said"),
        Some(&26)
    );
    assert_eq!(
        ui.domain.accounting.indexed_history().unwrap(),
        vec![(1, 17)]
    );
    assert!(ui.domain.accounting.reference_history().is_empty());
    assert_eq!(ui.domain.event_inputs.counts(), [0; 3]);
}

#[test]
fn v4_authorization_tuples_restore_unsettled_questions_without_local_answers() {
    let temp = tempfile::tempdir().unwrap();
    let reader = pre_accounting_v4_copy(&temp.path().join("copied.ledger"));
    let saved: serde_json::Value = reader
        .load_checkpoint("terminal-state", 4, 4)
        .unwrap()
        .state
        .unwrap();
    let mut state = saved["state"].clone();
    // Extend the fixed old document using its public flat tuple format, not a
    // new owner's serializer. Applying history intentionally drops local marks.
    state["pending_auth"] = json!([["one", "a"], ["two", "b"]]);
    let mut ui = Ui::replayed(&[]);
    ui.domain
        .authorizations
        .restore(vec![("old".into(), "old-call".into())]);
    ui.domain.authorizations.answer_oldest();
    let decoded: State = serde_json::from_value(state.clone()).unwrap();
    decoded.apply(&mut ui.domain, SETTLED_TICK).unwrap();
    assert_eq!(ui.domain.authorizations.next(), Some("one"));
    assert_eq!(ui.domain.authorizations.answerable_count(), 2);
    let captured = serde_json::to_value(State::reference_capture(&ui.domain)).unwrap();
    assert_eq!(captured["pending_auth"], state["pending_auth"]);
    assert!(captured.get("authorizations").is_none());
    assert!(captured.get("answered_locally").is_none());
}

#[test]
fn historical_install_is_atomic_and_round_trips_the_pre_refactor_document() {
    let old: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/fixtures/terminal-background-state-v4/state.json"
    ))
    .unwrap();
    for field in ["background", "models"] {
        let mut value = old.clone();
        if field == "background" {
            value["background"][1]["kind"] = json!("unknown");
        } else {
            value["models"]["now"] = json!(0);
        }
        let mut historical = Historical {
            title: "untouched".into(),
            skills: vec![("kept".into(), "description".into())],
            ..Historical::default()
        };
        historical
            .authorizations
            .restore(vec![("existing".into(), "held".into())]);
        let before = serde_json::to_value(State::capture(&historical)).unwrap();
        let decoded: State = serde_json::from_value(value).unwrap();
        assert_eq!(
            decoded.apply(&mut historical, ()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            serde_json::to_value(State::capture(&historical)).unwrap(),
            before,
            "{field}"
        );
    }
    let mut historical = Historical::default();
    serde_json::from_value::<State>(old.clone())
        .unwrap()
        .apply(&mut historical, ())
        .unwrap();
    assert_eq!(
        serde_json::to_value(State::capture(&historical)).unwrap(),
        old
    );
}

#[test]
fn invalid_v4_state_never_partially_applies_usage_or_other_facts() {
    let temp = tempfile::tempdir().unwrap();
    let reader = pre_accounting_v4_copy(&temp.path().join("copied.ledger"));
    let saved: serde_json::Value = reader
        .load_checkpoint("terminal-state", 4, 4)
        .unwrap()
        .state
        .unwrap();
    for field in [
        "composition",
        "previous",
        "turn_growth",
        "session_growth",
        "background",
        "models",
    ] {
        let mut value = saved["state"].clone();
        value[field] = match field {
            "composition" => json!([["unknown material", 1]]),
            "background" => {
                json!([{"kind":"unknown activity","key":"x","label":"x","standing":false,"fires":0,"ledger":null}])
            }
            "models" => json!({"rows":[],"now":0}),
            _ => json!({"unknown material":1}),
        };
        let decoded: State = serde_json::from_value(value).unwrap();
        let mut ui = Ui::replayed(&[]);
        ui.domain.title = "untouched".into();
        ui.domain.turns.seed_number(9);
        ui.domain.accounting.seed_last_call(Some(lattice::Usage {
            prompt: 999,
            ..Default::default()
        }));
        ui.domain.accounting.seed_turn_total(lattice::Usage {
            prompt: 777,
            ..Default::default()
        });
        ui.domain.accounting.seed_session_total(lattice::Usage {
            prompt: 888,
            ..Default::default()
        });
        ui.domain.accounting.seed_history(vec![(9, 99)]);
        ui.domain.accounting.bind_peaks(&reader).unwrap();
        ui.domain
            .authorizations
            .restore(vec![("old".into(), "old-call".into())]);
        ui.domain.authorizations.answer_oldest();
        let before = ui.domain.accounting.last_call();
        assert!(
            decoded.apply(&mut ui.domain, SETTLED_TICK).is_err(),
            "{field}"
        );
        assert_eq!(ui.domain.title, "untouched", "{field}");
        assert_eq!(ui.domain.turns.number(), 9, "{field}");
        assert_eq!(
            ui.domain.authorizations.history(),
            vec![("old".into(), "old-call".into())],
            "{field}"
        );
        assert_eq!(ui.domain.authorizations.answerable_count(), 0, "{field}");
        assert_eq!(ui.domain.accounting.last_call(), before, "{field}");
        assert_eq!(ui.domain.accounting.turn_total().prompt, 777, "{field}");
        assert_eq!(ui.domain.accounting.session_total().prompt, 888, "{field}");
        assert_eq!(
            ui.domain.accounting.indexed_history().unwrap(),
            vec![(9, 99)],
            "{field}"
        );
        assert!(ui.domain.accounting.parts().is_empty(), "{field}");
        assert!(
            ui.domain.accounting.previous().is_empty()
                && ui.domain.accounting.turn_growth().is_empty()
                && ui.domain.accounting.session_growth().is_empty(),
            "{field}"
        );
    }
}

#[test]
fn warm_state_matches_full_replay_and_never_saves_live_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state.ledger");
    let mut declarations: Vec<_> = ce::core_event_decls()
        .into_iter()
        .map(|mut d| {
            d.schema = None;
            d
        })
        .collect();
    for kind in [
        lattice::components::trust_policy::AUTH_REQUESTED,
        lattice::components::skill_library::SKILL_LISTING,
    ] {
        declarations.push(serde_json::from_value(json!({"type":kind})).unwrap());
    }
    let mut log = EventLog::open_segmented(declarations, "ui-state", root.clone(), 4096).unwrap();
    let mut events = Vec::new();
    events.push(append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"job","tool":"Run","arguments":{"command":"fixture"}}),
    ));
    events.push(append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[],
        json!({"call":"job","status":"ok","result":{"job":"job","background":true}}),
    ));
    let mut previous = None;
    for turn in 0..140 {
        let user = append(
            &mut log,
            ce::USER_MESSAGE,
            &[],
            json!({"text":format!("question {turn}")}),
        );
        let start = append(
            &mut log,
            ce::MODEL_CALL_STARTED,
            &[],
            json!({"model":"fixture","system":"instructions","tools":[],"input":{"parts":[{"event":user.id},{"event":previous.as_ref().unwrap_or(&user.id)}]}}),
        );
        let done = append(
            &mut log,
            ce::MODEL_CALL_COMPLETED,
            &[&start.id],
            json!({"text":"answer","reasoning":[{"type":"text","text":"thinking"}],"usage":{"prompt_tokens":turn * 13 + 10,"completion_tokens":5}}),
        );
        previous = Some(done.id.clone());
        events.extend([user, start, done]);
    }
    events.push(append(
        &mut log,
        ce::USER_MESSAGE,
        &[],
        json!({"text":"not sent yet"}),
    ));
    events.push(append(
        &mut log,
        lattice::components::skill_library::SKILL_LISTING,
        &[],
        json!({"skills":[{"name":"fixture","description":"saved palette"}]}),
    ));
    for held in ["first held call", "second held call"] {
        events.push(append(
            &mut log,
            lattice::components::trust_policy::AUTH_REQUESTED,
            &[],
            json!({"held":held,"tool":"fixture"}),
        ));
    }
    let through = log.reader().snapshot_end();
    let mut reference = Ui::replayed(&[]);
    for event in &events {
        reference.absorb(event, SETTLED_TICK);
    }
    let mut ui = Ui::replayed(&[]);
    ui.replay_prefix(&log.reader(), through).unwrap();
    assert_state(&ui, &reference);
    assert_eq!(ui.recovery.as_ref().unwrap().replayed, through);
    let before_summary = log.reader().memory_stats().unwrap().cache.unwrap();
    let summary = lattice::ledgers::summarize_reader(&root, &log.reader()).unwrap();
    let after_summary = log.reader().memory_stats().unwrap().cache.unwrap();
    assert_eq!(
        before_summary.hits + before_summary.decodes,
        after_summary.hits + after_summary.decodes,
        "the first exit summary must reuse the UI's completed projection"
    );
    assert_eq!(summary, lattice::ledgers::summarize(&root));
    let mut warm = Ui::replayed(&[]);
    warm.draft.edit().set("unsubmitted draft");
    warm.draft.attach(
        lattice::contracts::document::DocRef {
            file: "draft-only.png".into(),
            bytes: 10,
            lines: None,
            preview: None,
        },
        "image/png",
        "draft picture",
    );
    warm.draft.next_hint(3);
    let draft_before = (
        warm.draft.editor().shown().into_owned(),
        warm.draft.references().to_vec(),
        warm.draft.selected(),
    );
    warm.replay_prefix(&log.reader(), through).unwrap();
    assert_state(&warm, &reference);
    assert_eq!(
        (
            warm.draft.editor().shown().into_owned(),
            warm.draft.references().to_vec(),
            warm.draft.selected()
        ),
        draft_before,
        "restoring historical facts must not overwrite an existing draft"
    );
    assert_eq!(
        warm.recovery.as_ref().unwrap().replayed,
        0,
        "warm UI state must not fold the old prefix again"
    );
    assert!(warm.recovery.as_ref().unwrap().cold_reason.is_none());

    // These changes are not on the ledger. Even a later save must reflect the
    // pure event projection, not a convenient snapshot of the displayed Ui.
    warm.domain.turns.seed_busy(false);
    warm.domain.title = "unrecorded title".into();
    warm.domain.model.sent_effort("unrecorded effort".into());
    warm.domain.accounting.seed_session_total(lattice::Usage {
        calls: 999_999,
        ..warm.domain.accounting.session_total()
    });
    warm.domain
        .background
        .fixture_edit(0, |row| row.tools = 999);
    warm.draft.edit().clear();
    for key in ['y', 'n'] {
        crate::terminal_host::on_key(
            &mut warm,
            None,
            ratatui::crossterm::event::KeyEvent::new(
                crate::terminal_host::KeyCode::Char(key),
                crate::terminal_host::KeyModifiers::NONE,
            ),
            &crate::terminal_host::Hit::default(),
        );
    }
    assert_eq!(warm.domain.authorizations.answerable_count(), 0);
    assert_eq!(warm.domain.authorizations.history().len(), 2);
    let event = append(
        &mut log,
        ce::OUTPUT_REPLY,
        &[],
        json!({"text":"neutral event"}),
    );
    reference.absorb(&event, SETTLED_TICK);
    warm.try_absorb_profiled(&event, SETTLED_TICK, None)
        .unwrap();
    assert!(
        !warm.domain.turns.busy(),
        "a neutral indexed event does not undo local quiet"
    );
    assert_eq!(
        warm.domain.turns.history(),
        reference.domain.turns.history()
    );
    warm.recovery.as_mut().unwrap().save().unwrap();
    let mut restored = Ui::replayed(&[]);
    restored.replay_prefix(&log.reader(), event.seq).unwrap();
    assert_state(&restored, &reference);
    assert_eq!(restored.recovery.as_ref().unwrap().replayed, 0);
    assert!(restored.draft.editor().is_empty());
    assert!(restored.draft.references().is_empty());
    assert_eq!(restored.draft.selected(), 0);

    // Bad derived state requests exact cold replay, never empty counters.
    let key = restored.recovery.as_ref().unwrap().key.clone();
    let checkpoint = root.join(format!(
        "checkpoint-{:x}.json",
        Sha256::digest(key.as_bytes())
    ));
    let original: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    std::fs::write(checkpoint, "broken derived state").unwrap();
    let mut rebuilt = Ui::replayed(&[]);
    rebuilt.replay_prefix(&log.reader(), event.seq).unwrap();
    assert_state(&rebuilt, &reference);
    assert_eq!(rebuilt.recovery.as_ref().unwrap().replayed, event.seq);
    assert!(rebuilt.recovery.as_ref().unwrap().cold_reason.is_some());
    for (path, bytes) in original {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    // Current interpretation inputs are part of the cache identity.
    let mut changed = Ui::replayed(&[]);
    changed.domain.title = "different initial context".into();
    changed.replay_prefix(&log.reader(), event.seq).unwrap();
    assert_eq!(changed.recovery.as_ref().unwrap().replayed, event.seq);
}
