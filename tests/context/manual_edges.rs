use super::*;

#[test]
fn late_successful_compaction_from_the_previous_model_is_not_adopted() {
    for native in [false, true] {
        let mut kernel = condensing_kernel_with(
            json!({"script":[]}),
            main_script(900),
            config(native, true),
            true,
            None,
        );
        say(&mut kernel, "one");
        say(&mut kernel, "two");
        let request = kernel
            .log()
            .find_back(|event| {
                event.event_type == ce::MODEL_CALL_STARTED
                    && event.payload["purpose"] == purpose(native)
            })
            .unwrap()
            .unwrap();
        kernel.injector("ui").emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({"channel":context_gate::MODEL_CHANNEL,"model":"new"}),
            ),
        );
        kernel.run_until_quiescent().unwrap();
        kernel.injector("cmodel").emit(
            "result",
            EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&request.id], success(native)),
        );
        kernel.run_until_quiescent().unwrap();
        assert!(
            !kernel
                .log()
                .any(|event| event.event_type == context_gate::SUMMARY)
                .unwrap(),
            "a late successful result cannot change the new configuration's view"
        );
        say(&mut kernel, "new configuration");
        assert_eq!(
            requests(&kernel),
            2,
            "an old result cannot suppress the new configuration's compaction"
        );
        assert!(!kernel
            .log()
            .any(|event| event.event_type == context_gate::DECISION
                && event.payload["action"] == "suspend")
            .unwrap());
    }
}

#[test]
fn compaction_gate_replacement_restores_the_last_setting_without_a_startup_event() {
    let cfg = config(false, false);
    let mut kernel = condensing_kernel_with(
        json!({"script":[]}),
        main_script(10),
        cfg.clone(),
        false,
        None,
    );
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel":context_gate::EFFORT_CHANNEL,"value":"high"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(
        kernel.log().replay(1).unwrap().last().unwrap().payload["value"],
        "high"
    );
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        context_gate::NAME.into(),
        Box::new(|config| Box::new(context_gate::ContextGate::from_config(config))),
    );
    kernel.adopt_factories(factories);
    kernel
        .replace(
            "gate",
            context_gate::NAME,
            Some(cfg),
            "fixture replacement",
            &[],
        )
        .unwrap();
    say(&mut kernel, "after replacement");
    let forwarded = kernel
        .log()
        .find_back(|event| event.event_type == ce::MODEL_CALL_STARTED && event.source == "gate")
        .unwrap()
        .unwrap();
    assert_eq!(forwarded.payload["thinking"], "high");
}

#[test]
fn interrupted_manual_compaction_keeps_the_failure_pause_across_restart() {
    let home = tempfile::tempdir().unwrap();
    let ledger = home.path().join("interrupted.ledger");
    let cfg = config(false, true);
    let mut kernel = condensing_kernel_with(
        json!({"script":[]}),
        main_script(900),
        cfg.clone(),
        true,
        Some(&ledger),
    );
    say(&mut kernel, "one");
    say(&mut kernel, "two");
    let call = kernel
        .log()
        .find_back(|event| {
            event.event_type == ce::MODEL_CALL_STARTED && event.payload["purpose"] == purpose(false)
        })
        .unwrap()
        .unwrap();
    kernel.injector("cmodel").emit(
        "result",
        EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&call.id], failure(false)),
    );
    kernel.run_until_quiescent().unwrap();
    manual(&mut kernel);
    assert_eq!(requests(&kernel), 2);
    let pending = kernel.log().reader();
    let mut observer = context_gate::CompactionObserver::default();
    let status = observer
        .status_at(&pending, pending.snapshot_end())
        .unwrap()
        .unwrap();
    assert!(status.in_flight);
    let failure = status.failure.unwrap();
    drop(kernel);
    // Restart settles the actual unresolved request with core.interrupted.
    let mut resumed = condensing_kernel_with(
        json!({"script":[success(false)]}),
        main_script(900),
        cfg,
        false,
        Some(&ledger),
    );
    resumed.run_until_quiescent().unwrap();
    let reader = resumed.log().reader();
    let status = observer
        .status_at(&reader, reader.snapshot_end())
        .unwrap()
        .unwrap();
    assert!(!status.in_flight);
    assert_eq!(status.failure, Some(failure));
    assert_eq!(requests(&resumed), 2);
    assert!(resumed
        .log()
        .any(|event| event.event_type == ce::INTERRUPTED && event.payload["by"] == "restart")
        .unwrap());
    say(&mut resumed, "still paused");
    assert_eq!(requests(&resumed), 2);
    manual(&mut resumed);
    assert_eq!(requests(&resumed), 3);
    let status = observer
        .status_at(&reader, reader.snapshot_end())
        .unwrap()
        .unwrap();
    assert!(status.failure.is_none());
}
