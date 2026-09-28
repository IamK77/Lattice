use super::*;

#[test]
fn prepared_build_installs_actual_model_controls_after_history_replay() {
    crate::terminal_host::test_support::isolated(check_prepared_build);
}

fn check_prepared_build() {
    let dir = tempfile::tempdir().unwrap();
    let mut parent =
        lattice::EventLog::open(core_events::core_event_decls(), "parent", None).unwrap();
    parent
        .append(
            lattice::EventDraft::new(core_events::EXTERNAL_INPUT, &[], json!({"channel":"test"})),
            "ui",
        )
        .unwrap();
    let reader = parent.reader();
    let origin = StreamRef {
        stream: reader.stream().into(),
        event: reader
            .scan_back(|event, _| Ok(Some(event.id.clone())))
            .unwrap()
            .unwrap(),
    };
    let mut cfg = super::super::tests::config();
    cfg.model = "initial-side-model".into();
    cfg.thinking = Some(json!("low"));
    cfg.profile = Some(json!({"contextWindow":12345}));
    let settings = Settings::capture(&cfg);
    cfg.model = "unrelated-parent-model".into();
    cfg.thinking = Some(json!("high"));
    cfg.profile = None;
    let path = dir.path().join("side.jsonl");
    let events = [
        (core_events::COMPONENT_REPLACED, json!({"instance":"model", "to":"scripted-model",
            "config":{"model":"historical-side-model", "entryId":"historical-entry-id",
                "profile":{"contextWindow":54321}}})),
        (core_events::EXTERNAL_INPUT, json!({"channel":lattice::components::context_gate::EFFORT_CHANNEL,"value":"max"})),
        (core_events::EXTERNAL_INPUT, json!({"channel":lattice::components::context_gate::MODEL_CHANNEL,"contextWindow":99999})),
    ].into_iter().enumerate().map(|(i, (kind, payload))| json!({
        "v":1,"id":format!("ev_{}_side",i+1),"seq":i+1,"stream":"side",
        "time":"2026-09-19T00:00:00Z","type":kind,"source":"test","causes":[],"payload":payload
    }).to_string()).collect::<Vec<_>>().join("\n");
    std::fs::write(&path, events).unwrap();
    let mut actual_cfg = cfg.clone();
    Settings::restore(Some(&settings), &path, &mut actual_cfg).unwrap();
    let actual_running = preset::running_entry(&actual_cfg);
    let ready = Prepared {
        record: Record {
            file: "side.jsonl".into(),
            settings: Some(settings),
            parent: 0,
            origin,
        },
        cfg,
        reader,
        directory: dir.path().into(),
        workspace: "fixture-workspace".into(),
        bar: vec![],
        host_services: false,
        active_streams: vec!["parent".into()],
    }
    .build()
    .unwrap();
    assert_eq!(
        ready.ui.domain.title,
        "historical-side-model · fixture-workspace"
    );
    assert_eq!(ready.ui.domain.model.running(), &actual_running);
    assert_eq!(ready.ui.domain.model.effort().now.as_deref(), Some("max"));
    assert_eq!(
        ready.ui.domain.model.effort().rungs,
        actual_running.effort_rungs()
    );
    assert_eq!(
        ready.ui.domain.model.effective_window(),
        Some(54321),
        "actual profile overrides the replayed window"
    );
    let current = ready
        .ui
        .domain
        .model
        .catalog()
        .current()
        .expect("actual target has a catalog row");
    assert_eq!(current.model, "historical-side-model");
    assert_eq!(current.window, Some(54321));
    // Ready owns and shuts down this isolated, idle scripted session.
    drop(ready);
}
