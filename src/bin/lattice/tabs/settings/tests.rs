use super::*;

#[test]
fn restore_uses_its_own_initial_settings_then_completed_changes_not_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("side.jsonl");
    let mut cfg = super::super::tests::config();
    cfg.model = "original-side".into();
    cfg.thinking = Some(json!("low"));
    cfg.profile = Some(json!({"contextWindow":12345}));
    let initial = Settings::capture(&cfg);
    std::fs::write(&path, "").unwrap();
    cfg.model = "parent-now".into();
    cfg.thinking = Some(json!("high"));
    cfg.profile = None;
    Settings::restore(Some(&initial), &path, &mut cfg).unwrap();
    assert_eq!(cfg.model, "original-side");
    assert_eq!(cfg.thinking, Some(json!("low")));
    assert_eq!(cfg.profile, Some(json!({"contextWindow":12345})));

    let profile = json!({"contextWindow":54321});
    let payloads = [
        (
            core_events::COMPONENT_REPLACED,
            json!({"instance":"model","to":"scripted-model","config":{"model":"changed-side", "entryId":"selected", "baseUrl":"local", "apiKeyEnv":"", "profile":profile}}),
        ),
        (
            core_events::EXTERNAL_INPUT,
            json!({"channel":lattice::components::context_gate::EFFORT_CHANNEL, "value":false}),
        ),
    ];
    let events = payloads.into_iter().enumerate().map(|(i,(kind,payload))| {
        json!({"v":1,"id":format!("ev_{}_test",i+1),"seq":i+1,"stream":"side","time":"2026-09-12T00:00:00Z","type":kind,"source":"test","causes":[],"payload":payload}).to_string()
    }).collect::<Vec<_>>().join("\n");
    std::fs::write(&path, events).unwrap();
    Settings::restore(Some(&initial), &path, &mut cfg).unwrap();
    assert_eq!(cfg.model, "changed-side");
    assert_eq!(cfg.base_url, "local");
    assert_eq!(cfg.thinking, Some(json!(false)));
    assert_eq!(cfg.profile, Some(profile));
    let config = preset::main_model_config(&preset::running_entry(&cfg), cfg.thinking.as_ref());
    assert_eq!(config["model"], "changed-side");
    assert_eq!(config["thinking"], false);
    assert_eq!(config["profile"]["contextWindow"], 54321);
    assert_eq!(cfg.context_window, 54321);

    // A target without a window profile keeps the actual previous window,
    // not the much larger startup fallback.
    let swap = json!({"v":1,"id":"ev_1_swap","seq":1,"stream":"side","time":"2026-09-12T00:00:00Z","type":core_events::COMPONENT_REPLACED,"source":"core","causes":[],"payload":{"instance":"model","to":"scripted-model","config":{"model":"without-profile","profile":null}}});
    std::fs::write(&path, swap.to_string()).unwrap();
    cfg.context_window = 1_000_000;
    Settings::restore(Some(&initial), &path, &mut cfg).unwrap();
    assert_eq!(cfg.context_window, 12345);
    assert!(cfg.profile.is_none());
}

#[test]
fn segmented_settings_resume_from_the_exact_prefix_without_reading_old_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("side.ledger");
    let mut cfg = super::super::tests::config();
    let initial = Settings::capture(&cfg);
    let mut log =
        lattice::EventLog::open_segmented(core_events::core_event_decls(), "side", path.clone(), 1)
            .unwrap();
    for _ in 0..3 {
        log.append(
            lattice::EventDraft::new(
                core_events::EXTERNAL_INPUT,
                &[],
                json!({
                    "channel": lattice::components::context_gate::EFFORT_CHANNEL, "value": "low"
                }),
            ),
            "ui",
        )
        .unwrap();
    }
    let retained = log.reader();
    assert!(lattice::kernel::log::LogReader::segmented_snapshot(&path).is_err());
    drop(log);
    let reader = lattice::kernel::log::LogReader::segmented_snapshot(&path).unwrap();
    Settings::restore_using(Some(&initial), &path, &mut cfg, Some(&reader)).unwrap();
    assert_eq!(cfg.thinking, Some(json!("low")));
    let before = reader.memory_stats().unwrap().cache.unwrap();
    cfg.thinking = Some(json!("high"));
    Settings::restore_using(Some(&initial), &path, &mut cfg, Some(&reader)).unwrap();
    let after = reader.memory_stats().unwrap().cache.unwrap();
    assert_eq!(before.hits + before.decodes, after.hits + after.decodes);
    assert_eq!(cfg.thinking, Some(json!("low")));
    drop(reader);
    // A retained UI reader does not retain the stopped runtime's writer lease.
    let mut log =
        lattice::EventLog::open(core_events::core_event_decls(), "side", Some(path.clone()))
            .unwrap();
    log.append(
        lattice::EventDraft::new(
            core_events::EXTERNAL_INPUT,
            &[],
            json!({
                "channel": lattice::components::context_gate::EFFORT_CHANNEL, "value": false
            }),
        ),
        "ui",
    )
    .unwrap();
    drop(log);
    Settings::restore(Some(&initial), &path, &mut cfg).unwrap();
    assert_eq!(cfg.thinking, Some(json!(false)));
    assert_eq!(retained.snapshot_end(), 3);
    assert_eq!(retained.replay(1).unwrap().len(), 3);
}

#[test]
fn omitted_effort_stays_omitted_and_missing_keys_are_not_silently_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("side.jsonl");
    std::fs::write(&path, "").unwrap();
    let mut cfg = super::super::tests::config();
    let mut initial = Settings::capture(&cfg);
    cfg.thinking = Some(json!("high"));
    Settings::restore(Some(&initial), &path, &mut cfg).unwrap();
    assert!(cfg.thinking.is_none());
    initial.entry.adapter = "openai".into();
    initial.entry.key_env = "LATTICE_TEST_INTENTIONALLY_ABSENT_SIDE_KEY_03080F6".into();
    assert!(Settings::restore(Some(&initial), &path, &mut cfg)
        .unwrap_err()
        .contains("key is unavailable"));
}
