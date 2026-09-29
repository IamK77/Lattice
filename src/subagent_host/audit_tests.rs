use super::*;

#[test]
fn resumed_history_is_not_work_but_requests_in_this_lifetime_are() {
    let mut log = crate::EventLog::in_memory(
        vec![
            crate::EventTypeDecl::new(STREAM_REQUESTED, "request"),
            crate::EventTypeDecl::new(CANCEL_REQUESTED, "cancel"),
            crate::EventTypeDecl::new(ce::STREAM_RESUMED, "resume"),
        ],
        "parent",
    );
    log.append(
        EventDraft::new(STREAM_REQUESTED, &[], json!({})),
        "subagent",
    )
    .unwrap();
    log.append(
        EventDraft::new(CANCEL_REQUESTED, &[], json!({})),
        "subagent",
    )
    .unwrap();
    log.append(EventDraft::new(ce::STREAM_RESUMED, &[], json!({})), "core")
        .unwrap();
    assert!(request_batch(&log.reader(), 1).unwrap().requests.is_empty());
    let fresh = log
        .append(
            EventDraft::new(STREAM_REQUESTED, &[], json!({})),
            "subagent",
        )
        .unwrap();
    let batch = request_batch(&log.reader(), 1).unwrap();
    assert_eq!(
        batch.requests.iter().map(|e| &e.id).collect::<Vec<_>>(),
        vec![&fresh.id]
    );
    assert_eq!(batch.through, fresh.seq);
}

#[test]
fn request_scan_loads_only_current_requests_and_advances_across_unrelated_events() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let types = || {
        vec![
            crate::EventTypeDecl::new(STREAM_REQUESTED, "request"),
            crate::EventTypeDecl::new(CANCEL_REQUESTED, "cancel"),
            crate::EventTypeDecl::new(ce::STREAM_RESUMED, "resume"),
            crate::EventTypeDecl::new("fixture.noise", "unrelated history"),
        ]
    };
    let mut log = crate::EventLog::open(types(), "parent", Some(file.path().into())).unwrap();
    for _ in 0..256 {
        log.append(
            EventDraft::new("fixture.noise", &[], json!({"text": "history".repeat(128)})),
            "fixture",
        )
        .unwrap();
    }
    for kind in [STREAM_REQUESTED, CANCEL_REQUESTED] {
        log.append(EventDraft::new(kind, &[], json!({})), "subagent")
            .unwrap();
    }
    log.append(EventDraft::new(ce::STREAM_RESUMED, &[], json!({})), "core")
        .unwrap();
    drop(log);
    let mut log = crate::EventLog::open(types(), "parent", Some(file.path().into())).unwrap();
    let reads = |log: &crate::EventLog| {
        let cache = log.reader().memory_stats().unwrap().cache.unwrap();
        cache.hits + cache.decodes
    };
    let before = reads(&log);
    let batch = request_batch(&log.reader(), 1).unwrap();
    assert!(batch.requests.is_empty());
    assert_eq!(batch.through, log.reader().snapshot_end());
    assert_eq!(
        reads(&log),
        before,
        "old requests and unrelated bodies must stay unloaded"
    );
    let cursor = batch.through + 1;
    let fresh = log
        .append(
            EventDraft::new(STREAM_REQUESTED, &[], json!({"job": 1})),
            "subagent",
        )
        .unwrap();
    // A component emitting a similarly named event does not define a lifetime.
    log.append(
        EventDraft::new(ce::STREAM_RESUMED, &[], json!({})),
        "fixture",
    )
    .unwrap();
    let cancel = log
        .append(
            EventDraft::new(CANCEL_REQUESTED, &[], json!({"job": 1})),
            "subagent",
        )
        .unwrap();
    let tail = log
        .append(EventDraft::new("fixture.noise", &[], json!({})), "fixture")
        .unwrap();
    let before = reads(&log);
    let batch = request_batch(&log.reader(), cursor).unwrap();
    assert_eq!(
        batch.requests.iter().map(|e| &e.id).collect::<Vec<_>>(),
        vec![&fresh.id, &cancel.id]
    );
    assert_eq!(batch.through, tail.seq);
    assert_eq!(reads(&log) - before, 2);
    let before = reads(&log);
    assert!(request_batch(&log.reader(), tail.seq + 1)
        .unwrap()
        .requests
        .is_empty());
    assert_eq!(
        reads(&log),
        before,
        "a repeated poll must not reread old requests"
    );
    let encoded = serde_json::to_vec(&batch).unwrap();
    let decoded: RequestBatch = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.through, tail.seq);
    assert_eq!(decoded.requests.len(), 2);
}

#[test]
fn unreadable_current_request_is_reported_instead_of_skipped() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut log = crate::EventLog::open(
        vec![crate::EventTypeDecl::new(STREAM_REQUESTED, "request")],
        "parent",
        Some(file.path().into()),
    )
    .unwrap();
    log.append(
        EventDraft::new(STREAM_REQUESTED, &[], json!({})),
        "subagent",
    )
    .unwrap();
    file.as_file().set_len(0).unwrap();
    assert!(request_batch(&log.reader(), 1).is_err());
}

#[test]
fn empty_request_batches_still_advance_the_poll_cursor() {
    struct Host(std::cell::RefCell<Vec<u64>>);
    impl Streams for Host {
        fn open_streams(&self) -> Vec<String> {
            vec!["parent".into()]
        }
        fn requests_from(&self, _: &str, from: u64) -> Result<RequestBatch, String> {
            self.0.borrow_mut().push(from);
            Ok(RequestBatch {
                through: 100,
                requests: Vec::new(),
            })
        }
        fn open_detached(&mut self, _: &str, _: &str) -> Result<Kernel, String> {
            panic!("no requests")
        }
        fn ledger_of(&self, _: &str) -> Option<String> {
            None
        }
        fn injector_for(&self, _: &str, _: &str) -> Option<Injector> {
            None
        }
    }
    let mut host = Host(Default::default());
    let mut dispatcher = SubagentHost::new();
    dispatcher.poll(&mut host).unwrap();
    dispatcher.poll(&mut host).unwrap();
    assert_eq!(*host.0.borrow(), vec![1, 101]);
}

#[test]
fn a_closed_input_without_a_reply_is_not_success() {
    let assembly = crate::AssemblyManifest {
        instances: Default::default(),
        wires: Vec::new(),
    };
    let mut kernel = Kernel::start(
        &assembly,
        &Default::default(),
        &mut Default::default(),
        Default::default(),
    )
    .unwrap();
    let report = run_expert_turn(&mut kernel, &Mutex::new(JobState::default()), |_| false).unwrap();
    match report.outcome {
        ExpertOutcome::Failure(error) => assert_eq!(error["code"], "ask.no_reply"),
        other => panic!("closed input must report failure, got {other:?}"),
    }
    kernel.shutdown();
}

#[test]
fn a_quiet_expert_waits_for_external_input_instead_of_reporting_an_empty_answer() {
    use crate::components::scripted_model;
    use crate::preset::{standard, PresetConfig};
    use crate::{Component, Ctx, KernelOptions};

    struct DeferredModel;
    impl Component for DeferredModel {
        fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
    }
    let cfg = PresetConfig {
        adapter: "scripted".into(),
        model: "scripted".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(".".into()),
        context_window: 64000,
        usage_input_field: "input_tokens".into(),
        profile: None,
        catalog_problems: Vec::new(),
        system: "test".into(),
        scripted: Some(json!({"script": []})),
        thinking: None,
        overlay: None,
        assembly: None,
    };
    let (registry, mut factories, assembly) = standard(&cfg).unwrap();
    factories.insert(
        scripted_model::NAME.into(),
        Box::new(|_| Box::new(DeferredModel)),
    );
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"work"})),
    );
    let mut waits = 0;
    let answer = run_expert_turn(&mut kernel, &Mutex::new(JobState::default()), |kernel| {
        waits += 1;
        assert_eq!(waits, 1, "one external completion must finish the turn");
        // This callback runs only after the kernel is quiescent. Injecting here
        // establishes the boundary causally, without guessing a sleep duration.
        let events = kernel.log().replay(1).unwrap();
        let pending = events
            .iter()
            .rev()
            .find(|e| e.event_type == ce::MODEL_CALL_STARTED)
            .unwrap();
        kernel.injector("model").emit(
            "result",
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[&pending.id],
                json!({"status":"ok", "text":"after wake"}),
            ),
        );
        true
    });
    assert!(
        matches!(answer.unwrap().outcome, ExpertOutcome::Success(text) if text == "after wake")
    );
    assert_eq!(waits, 1);
    kernel.shutdown();
}
