use super::*;
use crate::{EventDraft, EventLog};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const CONSUMER: &str = "timer-subscriptions";

fn recover(reader: &LogReader) -> io::Result<Recovery> {
    super::recover(reader, Kind::Timer)
}

mod watches;

fn open(root: &Path) -> EventLog {
    EventLog::open_segmented(ce::core_event_decls(), "timer-recovery", root.into(), 4096).unwrap()
}
fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, payload), "fixture")
        .unwrap()
}
fn start(log: &mut EventLog, args: Value) -> EventEnvelope {
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"reused", "tool":"Schedule", "arguments":args}),
    )
}
fn ack(log: &mut EventLog, event: &EventEnvelope, id: u64) {
    append(
        log,
        ce::TOOL_EXEC_COMPLETED,
        &[&event.id],
        json!({"call":"reused", "status":"ok", "result":{"timer":id}}),
    );
}
fn fire(log: &mut EventLog, id: u64, n: u64) {
    append(
        log,
        ce::WAKE,
        &[],
        json!({"source":"timer", "summary":"fired", "body":{"timer":id,"fire":n}}),
    );
}
fn cancel(log: &mut EventLog, id: u64) {
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"cancel", "tool":"Unschedule", "arguments":{"timer":id}}),
    );
}
fn repeating() -> Value {
    json!({"interval_ms":1000,"max_fires":5,"note":"keep"})
}
fn as_value(state: &Restored) -> Value {
    json!({"next":state.next_id, "live":state.live.iter().map(|s| json!({
        "id":s.id,"fired":s.fired,"arguments":s.arguments,"cause":s.cause
    })).collect::<Vec<_>>()})
}
fn compare(reader: &LogReader) -> Recovery {
    let got = recover(reader).unwrap();
    let old = crate::components::subscription_history::restore(
        reader,
        "Schedule",
        "Unschedule",
        "timer",
        |args| args["interval_ms"].as_u64().is_some(),
    )
    .unwrap();
    assert_eq!(as_value(&got.state), as_value(&old));
    got
}
fn stored(reader: &LogReader) -> State {
    reader
        .load_checkpoint(CONSUMER, VERSION, reader.snapshot_end())
        .unwrap()
        .state
        .unwrap()
}
fn checkpoint(root: &Path) -> PathBuf {
    root.join(format!(
        "checkpoint-{:x}.json",
        Sha256::digest(CONSUMER.as_bytes())
    ))
}

#[test]
fn warm_timer_recovery_skips_old_history_and_unrelated_tool_bodies() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log = open(&root);
    for id in 1..=60 {
        let request = start(&mut log, repeating());
        ack(&mut log, &request, id);
        fire(&mut log, id, 5);
        let unrelated = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"other", "tool":"Read", "arguments":{}}),
        );
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&unrelated.id],
            json!({"call":"other", "status":"ok", "result":{"text":"unrelated"}}),
        );
    }
    let cold = compare(&log.reader());
    assert!(cold.cold_reason.is_some());
    assert_eq!(cold.headers, 300);
    assert_eq!(
        cold.bodies, 180,
        "only timer wakes, acknowledgements and requests are decoded"
    );
    assert_eq!(cold.state.next_id, 61);
    assert!(cold.state.live.is_empty());
    let state = stored(&log.reader());
    assert!(
        state.live.is_empty() && state.future_cancelled.is_empty() && state.future_fired.is_empty()
    );
    drop(log);
    let mut log = open(&root);
    let warm = recover(&log.reader()).unwrap();
    assert!(warm.cold_reason.is_none());
    assert_eq!((warm.headers, warm.bodies), (0, 0));
    let request = start(&mut log, repeating());
    ack(&mut log, &request, 61);
    let tail = compare(&log.reader());
    assert_eq!(tail.headers, 2);
    assert!(tail.cold_reason.is_none());
    assert_eq!(tail.state.live[0].fired, 0);
    let warm_live = recover(&log.reader()).unwrap();
    assert_eq!((warm_live.headers, warm_live.bodies), (0, 1));
    fire(&mut log, 61, 3);
    fire(&mut log, 61, 2);
    let fired = compare(&log.reader());
    assert_eq!(fired.headers, 2);
    assert_eq!(fired.state.live[0].fired, 3);
    cancel(&mut log, 61);
    let cancelled = compare(&log.reader());
    assert_eq!(cancelled.headers, 1);
    assert!(cancelled.state.live.is_empty());
    assert_eq!(cancelled.state.next_id, 62);
}

#[test]
fn one_shots_limits_and_future_cancellations_survive_checkpoint_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("main.ledger"));
    // Cancelling a future identifier is accepted by the existing tool.
    cancel(&mut log, 7);
    fire(&mut log, 8, 3);
    compare(&log.reader());
    assert!(stored(&log.reader()).future_cancelled.contains(&7));
    assert_eq!(stored(&log.reader()).future_fired[&8], 3);
    let cancelled = start(&mut log, repeating());
    ack(&mut log, &cancelled, 7);
    let early = start(&mut log, repeating());
    ack(&mut log, &early, 8);
    let one_shot = start(&mut log, json!({"delay_ms":1000}));
    ack(&mut log, &one_shot, 9);
    let clamped = start(&mut log, json!({"interval_ms":1000,"max_fires":0}));
    ack(&mut log, &clamped, 10);
    fire(&mut log, 10, 1);
    let restored = compare(&log.reader());
    assert!(restored.cold_reason.is_none());
    assert_eq!(restored.state.next_id, 11);
    assert_eq!(restored.state.live.len(), 1);
    assert_eq!(restored.state.live[0].id, 8);
    assert_eq!(restored.state.live[0].fired, 3);
    let state = stored(&log.reader());
    assert!(state.future_fired.is_empty() && state.future_cancelled.is_empty());
    fire(&mut log, 8, 5);
    assert!(compare(&log.reader()).state.live.is_empty());
}

#[test]
fn late_and_duplicate_assignments_keep_the_first_answer_and_closed_ids() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("main.ledger"));
    let first = start(&mut log, repeating());
    ack(&mut log, &first, 1);
    ack(&mut log, &first, 99);
    fire(&mut log, 1, 5);
    let cold = compare(&log.reader());
    assert_eq!(cold.state.next_id, 2);
    assert!(cold.state.live.is_empty());
    ack(&mut log, &first, 100);
    assert!(compare(&log.reader()).cold_reason.is_some());
    let reused_id = start(&mut log, repeating());
    ack(&mut log, &reused_id, 1);
    let restored = compare(&log.reader());
    assert!(restored.cold_reason.is_some());
    assert!(restored.state.live.is_empty());
    let late = start(&mut log, repeating());
    fire(&mut log, 2, 2);
    compare(&log.reader());
    ack(&mut log, &late, 2);
    let restored = compare(&log.reader());
    assert!(restored.cold_reason.is_some());
    assert_eq!(restored.state.live[0].fired, 2);
    let warm = recover(&log.reader()).unwrap();
    assert!(warm.cold_reason.is_none());
    assert_eq!((warm.headers, warm.bodies), (0, 1));
}

#[test]
fn damaged_cache_rebuilds_but_damaged_live_source_does_not_advance() {
    use std::io::{Seek, SeekFrom, Write};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log = open(&root);
    let request = start(&mut log, repeating());
    ack(&mut log, &request, 1);
    compare(&log.reader());
    std::fs::write(checkpoint(&root), b"broken").unwrap();
    let restored = compare(&log.reader());
    assert!(restored.cold_reason.is_some());
    let before = std::fs::read(checkpoint(&root)).unwrap();
    drop(log);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("00000000000000000000.jsonl"))
        .unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"!").unwrap();
    file.sync_all().unwrap();
    assert!(recover(&reader).is_err());
    assert_eq!(std::fs::read(checkpoint(&root)).unwrap(), before);
}

#[test]
fn exhausted_ids_fail_without_publishing_and_distinct_ledgers_do_not_share_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("one.ledger");
    let mut log = open(&root);
    recover(&log.reader()).unwrap();
    let before = std::fs::read(checkpoint(&root)).unwrap();
    let request = start(&mut log, repeating());
    ack(&mut log, &request, u64::MAX);
    assert!(recover(&log.reader()).is_err());
    assert_eq!(std::fs::read(checkpoint(&root)).unwrap(), before);
    let other = open(&temp.path().join("two.ledger"));
    let recovered = recover(&other.reader()).unwrap();
    assert!(recovered.cold_reason.is_some());
    assert_eq!(recovered.state.next_id, 1);
    assert!(recovered.state.live.is_empty());
}

#[test]
#[ignore = "requires LATTICE_RECOVERY_FIXTURE pointing to a disposable segmented ledger"]
fn offline_timer_recovery_matches_reference() {
    let root = PathBuf::from(
        std::env::var_os("LATTICE_RECOVERY_FIXTURE").expect("set LATTICE_RECOVERY_FIXTURE"),
    );
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let began = std::time::Instant::now();
    let expected = crate::components::subscription_history::restore(
        &reader,
        "Schedule",
        "Unschedule",
        "timer",
        |args| args["interval_ms"].as_u64().is_some(),
    )
    .unwrap();
    eprintln!(
        "reference timers: {:?}, live {}",
        began.elapsed(),
        expected.live.len()
    );
    let began = std::time::Instant::now();
    let actual = recover(&reader).unwrap();
    assert_eq!(as_value(&actual.state), as_value(&expected));
    eprintln!(
        "timer recovery: {:?}, headers {}, bodies {}, cold {:?}",
        began.elapsed(),
        actual.headers,
        actual.bodies,
        actual.cold_reason
    );
    drop(reader);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let began = std::time::Instant::now();
    let warm = recover(&reader).unwrap();
    assert_eq!(as_value(&warm.state), as_value(&expected));
    assert!(warm.cold_reason.is_none());
    assert_eq!(warm.headers, 0);
    assert_eq!(warm.bodies, warm.state.live.len());
    eprintln!(
        "warm timers: {:?}, headers {}, bodies {}",
        began.elapsed(),
        warm.headers,
        warm.bodies
    );
}
