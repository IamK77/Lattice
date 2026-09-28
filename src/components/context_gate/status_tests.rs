use super::*;
use crate::{core_events as ce, EventDraft, EventLog};
use serde_json::{json, Value};

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> crate::EventEnvelope {
    log.append(
        EventDraft::new(kind, causes, payload).with_reason("fixture"),
        "fixture",
    )
    .unwrap()
}

fn failed(log: &mut EventLog, message: &str) -> crate::EventEnvelope {
    let request = append(
        log,
        ce::MODEL_CALL_STARTED,
        &[],
        json!({
            "model":"fixture", "input":{"parts":[],"fingerprint":"sha256:fixture"},
            "purpose":super::super::CONDENSE_PURPOSE,
        }),
    );
    append(
        log,
        ce::MODEL_CALL_COMPLETED,
        &[&request.id],
        json!({
            "status":"error", "error":{"code":"transport.failed","message":message,"blame":"provider"},
        }),
    )
}

#[test]
fn compaction_observer_reads_legacy_failure_and_tail_without_rewriting_the_gate_checkpoint() {
    let home = tempfile::tempdir().unwrap();
    let mut log = EventLog::open_segmented(
        ce::core_event_decls(),
        "fixture",
        home.path().join("status.ledger"),
        4096,
    )
    .unwrap();
    let failure = failed(&mut log, "legacy failure");
    let reader = log.reader();
    let mut gate = recovery::Recovery::default();
    let mut legacy = gate
        .read(&reader, failure.seq, false, |state| {
            serde_json::to_value(state).unwrap()
        })
        .unwrap();
    legacy.as_object_mut().unwrap().remove("condense_failure");
    legacy["pending"] = json!({});
    let key = "context-gate-observations";
    reader
        .save_checkpoint(key, 2, failure.seq, &legacy)
        .unwrap();
    let mut observer = CompactionObserver::default();
    let status = observer.status_at(&reader, failure.seq).unwrap().unwrap();
    assert_eq!(status.failure.unwrap().event, failure.id);
    let changed = append(
        &mut log,
        ce::EXTERNAL_INPUT,
        &[],
        json!({"channel":super::super::MODEL_CHANNEL}),
    );
    assert!(observer.status_at(&reader, changed.seq).unwrap().is_none());
    assert_eq!(
        reader
            .load_checkpoint::<Value>(key, 2, changed.seq)
            .unwrap()
            .state,
        Some(legacy)
    );
    assert!(reader
        .load_checkpoint::<Value>(key, 3, changed.seq)
        .unwrap()
        .state
        .is_none());
}

#[test]
fn forwarded_compaction_outcomes_settle_the_whole_chain_live_and_after_checkpoint_recovery() {
    for (kind, payload) in [
        (ce::MODEL_CALL_COMPLETED, json!({"status":"ok"})),
        (ce::MODEL_CALL_COMPLETED, json!({"status":"cancelled"})),
        (ce::INTERRUPTED, json!({"by":"restart"})),
    ] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("forwarded.ledger");
        let mut log =
            EventLog::open_segmented(ce::core_event_decls(), "fixture", root.clone(), 4096)
                .unwrap();
        let request = json!({"model":"fixture","input":{"parts":[],"fingerprint":"sha256:fixture"},"purpose":super::super::CONDENSE_PURPOSE});
        let first = append(&mut log, ce::MODEL_CALL_STARTED, &[], request.clone());
        let forwarded = append(&mut log, ce::MODEL_CALL_STARTED, &[&first.id], request);
        let reader = log.reader();
        let mut observer = CompactionObserver::default();
        assert!(
            observer
                .status_at(&reader, forwarded.seq)
                .unwrap()
                .unwrap()
                .in_flight
        );
        let done = append(&mut log, kind, &[&forwarded.id], payload);
        let status = observer.status_at(&reader, done.seq).unwrap();
        assert!(
            !status.is_some_and(|status| status.in_flight),
            "{kind} must settle every forwarding copy"
        );
        let mut recovered = recovery::Recovery::default();
        let mut legacy = recovered
            .read(&reader, done.seq, false, |state| {
                serde_json::to_value(state).unwrap()
            })
            .unwrap();
        legacy.as_object_mut().unwrap().remove("condense_failure");
        legacy["pending"] = json!({first.id: null});
        reader
            .save_checkpoint("context-gate-observations", 2, done.seq, &legacy)
            .unwrap();
        drop(log);
        let reader = LogReader::segmented_snapshot(&root).unwrap();
        let mut cold = CompactionObserver::default();
        assert!(
            !cold
                .status_at(&reader, done.seq)
                .unwrap()
                .is_some_and(|status| status.in_flight),
            "legacy checkpoints must not preserve a falsely pending root"
        );
        recovery::Recovery::default()
            .read(&reader, done.seq, true, |_| ())
            .unwrap();
        assert!(!CompactionObserver::default()
            .status_at(&reader, done.seq)
            .unwrap()
            .is_some_and(|status| status.in_flight));
    }
}

#[test]
fn compaction_forwarding_cannot_move_an_old_request_into_a_new_model_epoch() {
    for status in ["ok", "error"] {
        let mut declarations = ce::core_event_decls();
        declarations.extend(super::super::manifest().events);
        let mut log = EventLog::in_memory(declarations, "fixture");
        let request = json!({"model":"fixture","input":{"parts":[],"fingerprint":"sha256:fixture"},"purpose":super::super::CONDENSE_PURPOSE});
        let first = append(&mut log, ce::MODEL_CALL_STARTED, &[], request.clone());
        append(
            &mut log,
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel":super::super::MODEL_CHANNEL}),
        );
        let copy = append(&mut log, ce::MODEL_CALL_STARTED, &[&first.id], request);
        let done = append(
            &mut log,
            ce::MODEL_CALL_COMPLETED,
            &[&copy.id],
            json!({"status":status}),
        );
        if status == "ok" {
            append(
                &mut log,
                super::super::SUMMARY,
                &[&done.id],
                json!({"covers":[],"text":"old summary"}),
            );
        }
        let reader = log.reader();
        let mut recovery = recovery::Recovery::default();
        recovery
            .read(&reader, reader.snapshot_end(), false, |state| {
                assert!(!state.in_flight());
                assert!(state.condense_failure.is_none());
                assert!(state.latest_summary(false, None).is_none());
            })
            .unwrap();
    }
}

#[test]
fn compaction_failure_details_are_bounded_and_do_not_leak_between_readers() {
    let mut first = EventLog::in_memory(ce::core_event_decls(), "same");
    let mut second = EventLog::in_memory(ce::core_event_decls(), "same");
    let error = format!("\u{1b}[31m{}\ntrailing\u{0}", "界".repeat(3000));
    let a = failed(&mut first, &error);
    let b = failed(&mut second, "other failure");
    let mut observer = CompactionObserver::default();
    let status = observer.status_at(&first.reader(), a.seq).unwrap().unwrap();
    let message = status.failure.as_ref().unwrap().message.as_str();
    assert!(message.chars().count() < 2100);
    assert!(!message.contains('\u{1b}'));
    assert!(!message.contains('\u{0}'));
    assert!(message.contains("see recorded event"));
    assert_eq!(
        observer.status_at(&first.reader(), a.seq).unwrap(),
        Some(status)
    );
    let other = observer
        .status_at(&second.reader(), b.seq)
        .unwrap()
        .unwrap();
    assert_eq!(other.failure.unwrap().message, "other failure");
    assert!(
        observer.status_at(&second.reader(), 0).is_err(),
        "a later state cannot masquerade as an earlier prefix"
    );
}
