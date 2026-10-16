use super::*;
use crate::{EventEnvelope, EventLog};
use std::path::Path;

fn open(root: &Path) -> EventLog {
    let mut types = ce::core_event_decls();
    types.extend([declaration(Kind::Timer), declaration(Kind::Watch)]);
    EventLog::open_segmented(types, "cancellation-fixture", root.into(), 4096).unwrap()
}

fn append(log: &mut EventLog, draft: EventDraft, source: &str) -> EventEnvelope {
    log.append(draft, source).unwrap()
}

fn request(log: &mut EventLog, kind: Kind, id: u64) -> EventEnvelope {
    let (_, tool, field) = kind.tools();
    append(
        log,
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({
                "call":"cancel", "tool":tool, "arguments":{(field):id}
            }),
        ),
        "caller",
    )
}

fn completed(log: &mut EventLog, request: &EventEnvelope, mut payload: Value, source: &str) {
    payload["call"] = json!("cancel");
    append(
        log,
        EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&request.id], payload),
        source,
    );
}

fn view(log: &EventLog, kind: Kind) -> LiveCancellations {
    LiveCancellations::new(log.reader(), kind, 0, Protocol::default(), BTreeSet::new())
}

#[test]
fn requests_and_buffered_receipts_do_not_stop_workers_but_committed_receipts_do() {
    for kind in [Kind::Timer, Kind::Watch] {
        let temp = tempfile::tempdir().unwrap();
        let mut log = open(&temp.path().join("ledger"));
        append(&mut log, activation(kind, 0), "provider");
        let live = view(&log, kind);
        let request = request(&mut log, kind, 7);
        assert!(!live.stopped(7).unwrap(), "a request is not cancellation");
        let buffered = receipt(kind, 7);
        assert!(
            !live.stopped(7).unwrap(),
            "a buffered emission is not committed state"
        );
        completed(&mut log, &request, buffered, "provider");
        assert!(live.stopped(7).unwrap());
        assert!(live.stopped(7).unwrap());
        assert!(!live.stopped(8).unwrap());
    }
}

#[test]
fn denied_interrupted_and_wrong_owner_outcomes_do_not_cancel() {
    for kind in [Kind::Timer, Kind::Watch] {
        let temp = tempfile::tempdir().unwrap();
        let mut log = open(&temp.path().join("ledger"));
        append(&mut log, activation(kind, 0), "provider");
        let live = view(&log, kind);
        let denied = request(&mut log, kind, 1);
        completed(
            &mut log,
            &denied,
            json!({"status":"error","error":{
                "code":"policy.denied","message":"denied","blame":"request"
            }}),
            "gate",
        );
        let interrupted = request(&mut log, kind, 2);
        append(
            &mut log,
            EventDraft::new(ce::INTERRUPTED, &[&interrupted.id], json!({"by":"restart"}))
                .with_reason("fixture interruption"),
            "core",
        );
        let foreign = request(&mut log, kind, 3);
        completed(&mut log, &foreign, receipt(kind, 3), "another-provider");
        for id in 1..=3 {
            assert!(!live.stopped(id).unwrap());
        }
    }
}

#[test]
fn reopening_observes_a_cancellation_that_no_worker_had_seen() {
    for kind in [Kind::Timer, Kind::Watch] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ledger");
        let mut log = open(&root);
        append(&mut log, activation(kind, 0), "provider");
        let request = request(&mut log, kind, 999);
        completed(&mut log, &request, receipt(kind, 999), "provider");
        // No live cancellation reader was used before the writer closed.
        drop(log);
        let reopened = open(&root);
        assert!(view(&reopened, kind).stopped(999).unwrap());
    }
}

#[test]
fn a_late_activation_cannot_apply_the_legacy_rule_to_live_requests() {
    for kind in [Kind::Timer, Kind::Watch] {
        let temp = tempfile::tempdir().unwrap();
        let mut log = open(&temp.path().join("ledger"));
        let live = view(&log, kind);
        request(&mut log, kind, 1);
        assert!(!live.stopped(1).unwrap());
        append(&mut log, activation(kind, 0), "provider");
        assert!(!live.stopped(1).unwrap());
    }
}

#[test]
fn first_activation_requires_rebuilding_legacy_facts_after_its_cutoff() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    let legacy = request(&mut log, Kind::Timer, 1);
    let modern = request(&mut log, Kind::Timer, 2);
    let activated = append(&mut log, activation(Kind::Timer, legacy.seq), "provider");
    let reader = log.reader();
    let mut protocol = Protocol::default();
    assert!(matches!(
        protocol
            .observe(&reader, &Header::from_event(&modern), Kind::Timer)
            .unwrap(),
        Observation::LegacyCancellation(2)
    ));
    assert!(matches!(
        protocol
            .observe(&reader, &Header::from_event(&activated), Kind::Timer)
            .unwrap(),
        Observation::Activated { first: true }
    ));
    let mut rebuilt = protocol.for_rebuild();
    assert!(matches!(
        rebuilt
            .observe(&reader, &Header::from_event(&legacy), Kind::Timer)
            .unwrap(),
        Observation::LegacyCancellation(1)
    ));
    assert!(matches!(
        rebuilt
            .observe(&reader, &Header::from_event(&modern), Kind::Timer)
            .unwrap(),
        Observation::None
    ));
    assert!(matches!(
        rebuilt
            .observe(&reader, &Header::from_event(&activated), Kind::Timer)
            .unwrap(),
        Observation::Activated { first: false }
    ));
}

#[test]
fn malformed_owned_receipts_and_changed_migration_boundaries_fail_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    append(&mut log, activation(Kind::Timer, 0), "provider");
    let request = request(&mut log, Kind::Timer, 1);
    completed(&mut log, &request, receipt(Kind::Timer, 2), "provider");
    assert!(view(&log, Kind::Timer)
        .stopped(1)
        .unwrap_err()
        .to_string()
        .contains("different subscription"));
    let reader = log.reader();
    let mut protocol = Protocol {
        legacy_through: Some(0),
        owner: Some("provider".into()),
    };
    let changed = append(&mut log, activation(Kind::Timer, 1), "provider");
    assert!(protocol
        .observe(&reader, &Header::from_event(&changed), Kind::Timer)
        .err()
        .unwrap()
        .to_string()
        .contains("boundary changed"));
}

#[test]
fn unreadable_source_is_not_an_empty_cancellation_set() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("ledger");
    let mut log = open(&root);
    append(&mut log, activation(Kind::Timer, 0), "provider");
    let request = request(&mut log, Kind::Timer, 1);
    completed(&mut log, &request, receipt(Kind::Timer, 1), "provider");
    let live = view(&log, Kind::Timer);
    std::fs::write(root.join("00000000000000000000.jsonl"), b"broken\n").unwrap();
    assert!(live.stopped(1).is_err());
}
