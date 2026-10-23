use super::*;

fn command(kernel: &mut Kernel, channel: &str) {
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(ce::EXTERNAL_INPUT, &[], json!({"channel":channel})),
    );
    kernel.run_until_quiescent().unwrap();
}

#[test]
fn manual_retry_cannot_overtake_the_previous_failure_handoff() {
    let mut kernel = fixture_controlled(None, true, false, true);
    let (ask, first) = pending(&mut kernel);
    complete(
        &mut kernel,
        &first,
        json!({"status":"error","error":{"code":"failed","message":"synthetic failure","blame":"provider"}}),
    );
    command(&mut kernel, context_gate::COMPACT_CHANNEL);
    let before = kernel.log().replay(1).unwrap();
    assert_eq!(
        before
            .iter()
            .filter(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
            .count(),
        1,
        "a new attempt cannot inherit waits whose failure is recorded but not yet delivered"
    );
    command(&mut kernel, "test.release-handoff");
    let settled = kernel.log().replay(1).unwrap();
    assert_eq!(
        settled.iter().filter(|e| ce::ends_call(e, &ask.id)).count(),
        1
    );
    command(&mut kernel, context_gate::COMPACT_CHANNEL);
    let events = kernel.log().replay(1).unwrap();
    let second = events
        .iter()
        .rev()
        .find(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
        .unwrap()
        .clone();
    assert_ne!(second.id, first.id);
    complete(
        &mut kernel,
        &second,
        json!({"status":"ok","text":GOOD_SUMMARY}),
    );
    assert_eq!(
        main_requests(&kernel.log().replay(1).unwrap()).len(),
        3,
        "manual recovery does not answer the old ordinary request"
    );
    kernel.shutdown();
}

#[test]
fn late_success_from_a_settled_attempt_cannot_replace_a_newer_outcome() {
    for newer_success in [false, true] {
        let mut kernel = fixture(None);
        let (_, first) = pending(&mut kernel);
        complete(&mut kernel, &first, json!({"status":"cancelled"}));
        command(&mut kernel, context_gate::COMPACT_CHANNEL);
        let events = kernel.log().replay(1).unwrap();
        let second = events
            .iter()
            .rev()
            .find(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
            .unwrap()
            .clone();
        assert_ne!(second.id, first.id);
        complete(
            &mut kernel,
            &second,
            if newer_success {
                json!({"status":"ok","text":GOOD_SUMMARY})
            } else {
                json!({"status":"error","error":{"code":"newer.failed","message":"newer failure","blame":"provider"}})
            },
        );
        let before = kernel.log().replay(1).unwrap();
        let summaries: Vec<_> = before
            .iter()
            .filter(|e| e.event_type == context_gate::SUMMARY)
            .map(|e| e.id.clone())
            .collect();
        let reader = kernel.log().reader();
        let status = context_gate::CompactionObserver::default()
            .status_at(&reader, reader.snapshot_end())
            .unwrap();
        complete(
            &mut kernel,
            &first,
            json!({"status":"ok","text":GOOD_SUMMARY}),
        );
        let after = kernel.log().replay(1).unwrap();
        assert_eq!(
            after
                .iter()
                .filter(|e| e.event_type == context_gate::SUMMARY)
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            summaries,
            "a settled older call must not publish a replacement"
        );
        assert_eq!(
            context_gate::CompactionObserver::default()
                .status_at(&reader, reader.snapshot_end())
                .unwrap()
                .unwrap()
                .failure,
            status.unwrap().failure
        );
        assert_eq!(main_requests(&after).len(), 3);
        kernel.shutdown();
    }
}

#[test]
fn interrupted_compactor_settles_only_its_own_call_and_the_gate_ends_the_wait() {
    let mut kernel = fixture_with_compactor(None, true, true);
    let (ask, compact) = pending(&mut kernel);
    let events = kernel.log().replay(1).unwrap();
    let compact_outcome = events
        .iter()
        .find(|e| ce::ends_call(e, &compact.id))
        .unwrap();
    assert_eq!(compact_outcome.event_type, ce::INTERRUPTED);
    assert_eq!(compact_outcome.source, "core");
    let outcomes: Vec<_> = events
        .iter()
        .filter(|e| ce::ends_call(e, &ask.id))
        .collect();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].source, "gate");
    assert_eq!(main_requests(&events).len(), 3);
    assert!(!events.iter().any(|e| e.event_type == ce::ERROR));
    kernel.shutdown();
}

fn pending(kernel: &mut Kernel) -> (lattice::EventEnvelope, lattice::EventEnvelope) {
    say(kernel, "read source and two more results");
    let events = kernel.log().replay(1).unwrap();
    assert_eq!(main_requests(&events).len(), 3);
    let ask = events
        .iter()
        .rev()
        .find(|e| e.source == "loop" && e.event_type == ce::MODEL_CALL_STARTED)
        .unwrap()
        .clone();
    let compact = events
        .iter()
        .find(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
        .unwrap()
        .clone();
    (ask, compact)
}

fn complete(kernel: &mut Kernel, compact: &lattice::EventEnvelope, payload: Value) {
    kernel.injector("cmodel").emit(
        "result",
        EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&compact.id], payload),
    );
    kernel.run_until_quiescent().unwrap();
}

#[test]
fn failed_cancelled_and_invalid_compaction_settle_the_wait_without_forwarding_or_retry() {
    for payload in [
        json!({"status":"error","error":{"code":"unavailable","message":"synthetic failure","blame":"provider","retryable":true}}),
        json!({"status":"cancelled"}),
        json!({"status":"ok","text":"incomplete"}),
        json!({"status":"ok","text":GOOD_SUMMARY,"stopReason":"length"}),
    ] {
        let mut kernel = fixture(None);
        let (ask, compact) = pending(&mut kernel);
        say(&mut kernel, "queued while compaction is pending");
        complete(&mut kernel, &compact, payload);
        let events = kernel.log().replay(1).unwrap();
        assert_eq!(main_requests(&events).len(), 3);
        assert_eq!(
            events.iter().filter(|e| ce::ends_call(e, &ask.id)).count(),
            1
        );
        let interrupted = events.iter().find(|e| ce::ends_call(e, &ask.id)).unwrap();
        assert_eq!(interrupted.event_type, ce::INTERRUPTED);
        assert!(interrupted
            .reason
            .as_deref()
            .unwrap()
            .contains("Original context was retained"));
        assert_eq!(
            events
                .iter()
                .filter(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
                .count(),
            1
        );
        assert!(!events.iter().any(|e| e.event_type == context_gate::SUMMARY));
        assert!(!events.iter().any(|e| e.event_type == ce::ERROR));
        kernel.shutdown();
    }
}

#[test]
fn user_stop_settles_the_wait_and_late_compaction_cannot_resume_it() {
    let mut kernel = fixture(None);
    let (ask, compact) = pending(&mut kernel);
    kernel.injector("ui").emit(
        "interrupt",
        EventDraft::new(ce::INTERRUPTED, &[], json!({"by":"user"})).with_reason("stop"),
    );
    kernel.run_until_quiescent().unwrap();
    let stopped = kernel.log().replay(1).unwrap();
    assert_eq!(
        stopped.iter().filter(|e| ce::ends_call(e, &ask.id)).count(),
        1
    );
    complete(
        &mut kernel,
        &compact,
        json!({"status":"ok","text":GOOD_SUMMARY}),
    );
    let after = kernel.log().replay(1).unwrap();
    assert_eq!(main_requests(&after).len(), 3);
    assert_eq!(
        after.iter().filter(|e| ce::ends_call(e, &ask.id)).count(),
        1
    );
    assert!(!after.iter().any(|e| e.event_type == ce::ERROR));
    kernel.shutdown();
}

#[test]
fn restart_after_compaction_completion_still_settles_the_unforwarded_main_request() {
    for finish in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handoff.ledger");
        let mut kernel = fixture_with_delivery(Some(&path), false);
        let (ask, compact) = pending(&mut kernel);
        if finish {
            complete(
                &mut kernel,
                &compact,
                json!({"status":"ok","text":GOOD_SUMMARY}),
            );
        }
        let before = kernel.log().replay(1).unwrap();
        assert_eq!(main_requests(&before).len(), 3);
        assert!(
            ce::hanging_chain_heads(&before, ce::MODEL_CALL_STARTED).contains(&ask.id),
            "compactor completion must not settle the ordinary request"
        );
        let tool_results = before
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
            .count();
        kernel.shutdown();
        let mut reopened = fixture(Some(&path));
        reopened.run_until_quiescent().unwrap();
        let after = reopened.log().replay(1).unwrap();
        assert_eq!(
            main_requests(&after).len(),
            3,
            "recovery never resubmits a held model call"
        );
        assert_eq!(
            after.iter().filter(|e| ce::ends_call(e, &ask.id)).count(),
            1
        );
        assert_eq!(
            after
                .iter()
                .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
                .count(),
            tool_results
        );
        assert!(!after.iter().any(|e| e.event_type == ce::ERROR));
        reopened.shutdown();
    }
}
