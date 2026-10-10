use super::*;

#[path = "manual_edges.rs"]
mod edges;
#[path = "manual_effective.rs"]
mod effective;
#[path = "health.rs"]
mod health;

fn requests(kernel: &Kernel) -> usize {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.event_type == ce::MODEL_CALL_STARTED && event.payload.get("purpose").is_some()
        })
        .count()
}
fn ordinary_requests(kernel: &Kernel) -> usize {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.event_type == ce::MODEL_CALL_STARTED && event.payload.get("purpose").is_none()
        })
        .count()
}
fn manual(kernel: &mut Kernel) {
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel":context_gate::COMPACT_CHANNEL}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
}
fn purpose(native: bool) -> &'static str {
    if native {
        "context.compact.responses"
    } else {
        context_gate::CONDENSE_PURPOSE
    }
}
fn success(native: bool) -> Value {
    if native {
        json!({"status":"ok","purpose":purpose(native),"nativeCompaction":{"dialect":"responses","output":[{"type":"compaction","encrypted_content":"test-capsule"}]}})
    } else {
        json!({"status":"ok","purpose":purpose(native),"text":"Done: tested\nState: ready\nOpen: continue\nFacts: fixture"})
    }
}
fn failure(native: bool) -> Value {
    json!({"status":"error","purpose":purpose(native),"error":{"code":"stream_read_error","message":"fixture transport failure","blame":"provider","retryable":true,"transient":true}})
}
fn config(native: bool, pressured: bool) -> Value {
    json!({"profile":{"contextWindow":1000,"usageFields":{"input":"prompt_tokens"}},"ratio":if pressured {0.5} else {1.0},"keepRecentParts":1,"minCondense":1,
        "condense":true,"nativeCompaction":native,"system":"keep these instructions"})
}
fn main_script(tokens: u64) -> Value {
    json!({"script":[
        {"status":"ok","text":"one","usage":{"prompt_tokens":tokens,"completion_tokens":1}},
        {"status":"ok","text":"two","usage":{"prompt_tokens":tokens,"completion_tokens":1}},
        {"status":"ok","text":"three","usage":{"prompt_tokens":tokens,"completion_tokens":1}},
        {"status":"ok","text":"four","usage":{"prompt_tokens":tokens,"completion_tokens":1}}
    ]})
}

#[test]
fn explicit_compaction_runs_while_idle_below_the_automatic_threshold() {
    for native in [false, true] {
        let mut kernel = condensing_kernel_with(
            json!({"script":[success(native)]}),
            main_script(10),
            config(native, false),
            false,
            None,
        );
        say(&mut kernel, "first");
        say(&mut kernel, "second");
        assert_eq!(requests(&kernel), 0);
        let ordinary = ordinary_requests(&kernel);
        let assembled = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == ce::MODEL_CALL_STARTED
                    && event.source == "gate"
                    && event.payload.get("purpose").is_none()
            })
            .unwrap()
            .payload
            .clone();
        manual(&mut kernel);
        assert_eq!(requests(&kernel), 1);
        assert_eq!(
            ordinary_requests(&kernel),
            ordinary,
            "manual compaction must not start a foreground answer"
        );
        let command = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == ce::EXTERNAL_INPUT
                    && event.payload["channel"] == context_gate::COMPACT_CHANNEL
            })
            .unwrap();
        let request = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| {
                event.event_type == ce::MODEL_CALL_STARTED && event.payload.get("purpose").is_some()
            })
            .unwrap();
        assert_eq!(request.causes, std::slice::from_ref(&command.id));
        if native {
            assert_eq!(assembled["system"], "keep these instructions");
            assert_eq!(request.payload["system"], assembled["system"]);
            assert_eq!(request.payload["tools"], assembled["tools"]);
        }
        assert!(kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .any(|event| event.event_type == context_gate::DECISION
                && event.payload["action"] == "condense"
                && event.payload["scale"] == "manual"));
        assert!(kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .any(|event| event.event_type == context_gate::SUMMARY));
    }
}

#[test]
fn failed_manual_attempt_stays_paused_and_is_not_replayed_but_success_reopens_automatic_compaction()
{
    for native in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let ledger = home.path().join("manual.ledger");
        let cfg = config(native, true);
        let mut kernel = condensing_kernel_with(
            json!({"script":[failure(native),failure(native)]}),
            main_script(900),
            cfg.clone(),
            false,
            Some(&ledger),
        );
        say(&mut kernel, "first");
        say(&mut kernel, "second");
        assert_eq!(requests(&kernel), 1);
        let before_retry = kernel.log().replay(1).unwrap();
        assert!(
            before_retry
                .iter()
                .all(|event| event.event_type != ce::ERROR),
            "{before_retry:?}"
        );
        assert!(before_retry
            .iter()
            .any(|event| event.event_type == ce::MODEL_CALL_COMPLETED
                && event.payload.get("purpose").is_some()
                && event.payload["status"] == "error"));
        manual(&mut kernel);
        assert_eq!(
            requests(&kernel),
            2,
            "an explicit request bypasses only the failure pause"
        );
        say(&mut kernel, "still paused");
        assert_eq!(
            requests(&kernel),
            2,
            "failure must not cause per-turn retries"
        );
        drop(kernel);
        let mut resumed = condensing_kernel_with(
            json!({"script":[success(native),success(native)]}),
            main_script(900),
            cfg,
            false,
            Some(&ledger),
        );
        resumed.run_until_quiescent().unwrap();
        assert_eq!(
            requests(&resumed),
            2,
            "replay never executes the old manual command"
        );
        say(&mut resumed, "after restart");
        assert_eq!(requests(&resumed), 2);
        manual(&mut resumed);
        assert_eq!(requests(&resumed), 3);
        assert!(resumed
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .any(|event| event.event_type == context_gate::SUMMARY));
        say(&mut resumed, "measure the new summary");
        assert_eq!(
            requests(&resumed),
            3,
            "old usage cannot immediately trigger another compaction"
        );
        say(&mut resumed, "automatic again");
        assert_eq!(
            requests(&resumed),
            4,
            "a successful explicit attempt restores ordinary automatic compaction"
        );
    }
}

#[test]
fn cancelled_or_rejected_manual_compaction_never_clears_a_recorded_pause() {
    for native in [false, true] {
        let mut kernel = condensing_kernel_with(
            json!({"script":[failure(native),
                {"status":"cancelled","purpose":purpose(native)},
                {"status":"ok","purpose":purpose(native),"text":"not a summary"},
                success(native)]}),
            main_script(900),
            config(native, true),
            false,
            None,
        );
        say(&mut kernel, "one");
        say(&mut kernel, "two");
        let reader = kernel.log().reader();
        let mut observer = context_gate::CompactionObserver::default();
        let original = observer
            .status_at(&reader, reader.snapshot_end())
            .unwrap()
            .unwrap()
            .failure
            .unwrap();
        manual(&mut kernel);
        assert_eq!(requests(&kernel), 2);
        let status = observer
            .status_at(&reader, reader.snapshot_end())
            .unwrap()
            .unwrap();
        assert!(!status.in_flight);
        assert_eq!(status.failure.as_ref(), Some(&original));
        say(&mut kernel, "still paused after cancellation");
        assert_eq!(requests(&kernel), 2);
        manual(&mut kernel);
        assert_eq!(requests(&kernel), 3);
        let rejected = observer
            .status_at(&reader, reader.snapshot_end())
            .unwrap()
            .unwrap();
        assert_eq!(rejected.failure.unwrap().code, "summary_rejected");
        say(&mut kernel, "still paused after rejected summary");
        assert_eq!(requests(&kernel), 3);
        manual(&mut kernel);
        assert_eq!(requests(&kernel), 4);
        let status = observer
            .status_at(&reader, reader.snapshot_end())
            .unwrap()
            .unwrap();
        assert!(!status.in_flight);
        assert!(status.failure.is_none());
        assert!(kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .all(|event| event.event_type != ce::ERROR));
    }
}

#[test]
fn queued_manual_compaction_commands_submit_only_one_attempt() {
    for native in [false, true] {
        let mut kernel = condensing_kernel_with(
            json!({"script":[success(native),success(native)]}),
            main_script(10),
            config(native, false),
            false,
            None,
        );
        say(&mut kernel, "one");
        say(&mut kernel, "two");
        for _ in 0..2 {
            kernel.injector("ui").emit(
                "answer",
                EventDraft::new(
                    ce::EXTERNAL_INPUT,
                    &[],
                    json!({"channel":context_gate::COMPACT_CHANNEL}),
                ),
            );
        }
        kernel.run_until_quiescent().unwrap();
        assert_eq!(
            requests(&kernel),
            1,
            "queued commands cannot race the request's ledger append"
        );
        manual(&mut kernel);
        assert_eq!(
            requests(&kernel),
            1,
            "the completed handoff is released and already-covered context is skipped"
        );
        assert!(kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|event| event.event_type == context_gate::DECISION
                && event.payload["action"] == "compact_skipped"
                && event
                    .reason
                    .as_deref()
                    .unwrap_or_default()
                    .contains("Not enough older context")));
    }
}

#[test]
fn compaction_restore_includes_a_setting_at_the_last_committed_event() {
    let home = tempfile::tempdir().unwrap();
    let ledger = home.path().join("last-setting.ledger");
    let mut kernel = condensing_kernel_with(
        json!({"script":[]}),
        main_script(10),
        config(false, false),
        false,
        Some(&ledger),
    );
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({
                "channel":context_gate::EFFORT_CHANNEL,"value":"high",
            }),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(
        kernel.log().replay(1).unwrap().last().unwrap().payload["value"],
        "high"
    );
    drop(kernel);
    let mut resumed = condensing_kernel_with(
        json!({"script":[]}),
        main_script(10),
        config(false, false),
        false,
        Some(&ledger),
    );
    say(&mut resumed, "after restart");
    let forwarded = resumed
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == ce::MODEL_CALL_STARTED && event.source == "gate")
        .unwrap();
    assert_eq!(
        forwarded.payload["thinking"], "high",
        "restore has no in-progress delivery to exclude"
    );
}

#[test]
fn explicit_compaction_does_not_duplicate_in_flight_work_and_explains_skipped_requests() {
    let mut kernel = condensing_kernel_with(
        json!({"script":[]}),
        main_script(10),
        config(false, false),
        true,
        None,
    );
    manual(&mut kernel);
    assert_eq!(requests(&kernel), 0);
    assert!(kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .any(|event| event.event_type == context_gate::DECISION
            && event.payload["action"] == "compact_skipped"
            && event
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("no conversation")));
    say(&mut kernel, "one");
    manual(&mut kernel);
    assert_eq!(requests(&kernel), 0, "the recent tail remains protected");
    say(&mut kernel, "two");
    manual(&mut kernel);
    assert_eq!(requests(&kernel), 1);
    manual(&mut kernel);
    assert_eq!(requests(&kernel), 1);
    assert!(kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .any(|event| event.event_type == context_gate::DECISION
            && event.payload["action"] == "compact_skipped"
            && event
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("already in progress")));
}
