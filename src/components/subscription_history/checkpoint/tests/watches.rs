use super::super::{recover, Kind, Recovery, VERSION};
use super::{append, as_value, open};
use crate::{core_events as ce, EventEnvelope, EventLog, LogReader};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn start(log: &mut EventLog, limit: u64) -> EventEnvelope {
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({
            "call":"reused", "tool":"Watch",
            "arguments":{"path":"/fixture/path", "max_fires":limit}
        }),
    )
}
fn ack(log: &mut EventLog, request: &EventEnvelope, id: u64) {
    append(
        log,
        ce::TOOL_EXEC_COMPLETED,
        &[&request.id],
        json!({
            "call":"reused", "status":"ok", "result":{"watch":id}
        }),
    );
}
fn fire(log: &mut EventLog, id: u64, n: u64) {
    append(
        log,
        ce::WAKE,
        &[],
        json!({
            "source":"watch", "summary":"changed", "body":{"watch":id,"fire":n}
        }),
    );
}
fn cancel(log: &mut EventLog, id: u64) {
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({
            "call":"cancel", "tool":"Unwatch", "arguments":{"watch":id}
        }),
    );
}
fn compare(reader: &LogReader) -> Recovery {
    let got = recover(reader, Kind::Watch).unwrap();
    let old = crate::components::subscription_history::restore(
        reader,
        "Watch",
        "Unwatch",
        "watch",
        |_| true,
    )
    .unwrap();
    assert_eq!(as_value(&got.state), as_value(&old));
    got
}
fn checkpoint(root: &Path) -> PathBuf {
    root.join(format!(
        "checkpoint-{:x}.json",
        Sha256::digest(Kind::Watch.consumer().as_bytes())
    ))
}

#[test]
fn warm_watch_recovery_is_incremental_and_independent_of_timers() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log = open(&root);
    for id in 1..=40 {
        let watch = start(&mut log, 5);
        ack(&mut log, &watch, id);
        fire(&mut log, id, 5);
        let timer = super::start(&mut log, super::repeating());
        super::ack(&mut log, &timer, id);
        super::fire(&mut log, id, 5);
    }
    let cold = compare(&log.reader());
    assert!(cold.cold_reason.is_some());
    assert_eq!((cold.headers, cold.bodies), (240, 160));
    assert_eq!(cold.state.next_id, 41);
    assert!(cold.state.live.is_empty());
    super::compare(&log.reader());
    drop(log);
    let mut log = open(&root);
    let warm = recover(&log.reader(), Kind::Watch).unwrap();
    assert!(warm.cold_reason.is_none());
    assert_eq!((warm.headers, warm.bodies), (0, 0));
    let watch = start(&mut log, 5);
    ack(&mut log, &watch, 41);
    let timer = super::start(&mut log, super::repeating());
    super::ack(&mut log, &timer, 41);
    fire(&mut log, 41, 2);
    fire(&mut log, 41, 1);
    super::fire(&mut log, 41, 3);
    super::cancel(&mut log, 41);
    let tail = compare(&log.reader());
    assert!(tail.cold_reason.is_none());
    assert_eq!(tail.headers, 8);
    assert_eq!(tail.state.live.len(), 1);
    assert_eq!(tail.state.live[0].fired, 2);
    assert!(super::compare(&log.reader()).state.live.is_empty());
    let warm = recover(&log.reader(), Kind::Watch).unwrap();
    assert_eq!((warm.headers, warm.bodies), (0, 1));
    cancel(&mut log, 41);
    let stopped = compare(&log.reader());
    assert_eq!(stopped.headers, 1);
    assert!(stopped.state.live.is_empty());
    assert_eq!(stopped.state.next_id, 42);
    let state: super::super::State = log
        .reader()
        .load_checkpoint(Kind::Watch.consumer(), VERSION, log.reader().snapshot_end())
        .unwrap()
        .state
        .unwrap();
    assert!(
        state.live.is_empty() && state.future_fired.is_empty() && state.future_cancelled.is_empty()
    );
}

#[test]
fn watch_future_facts_limits_and_late_answers_match_the_reference() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("main.ledger"));
    cancel(&mut log, 1);
    fire(&mut log, 2, 2);
    compare(&log.reader());
    let cancelled = start(&mut log, 5);
    ack(&mut log, &cancelled, 1);
    let live = start(&mut log, 5);
    ack(&mut log, &live, 2);
    ack(&mut log, &live, 99);
    let limited = start(&mut log, 0);
    ack(&mut log, &limited, 3);
    fire(&mut log, 3, 1);
    let restored = compare(&log.reader());
    assert!(restored.cold_reason.is_none());
    assert_eq!(restored.state.next_id, 4);
    assert_eq!(restored.state.live.len(), 1);
    assert_eq!(restored.state.live[0].fired, 2);
    fire(&mut log, 2, 5);
    assert!(compare(&log.reader()).state.live.is_empty());
    ack(&mut log, &live, 100);
    assert!(compare(&log.reader()).cold_reason.is_some());
    let reused = start(&mut log, 5);
    ack(&mut log, &reused, 1);
    let restored = compare(&log.reader());
    assert!(restored.cold_reason.is_some());
    assert!(restored.state.live.is_empty());
    let late = start(&mut log, 5);
    fire(&mut log, 4, 3);
    compare(&log.reader());
    ack(&mut log, &late, 4);
    let restored = compare(&log.reader());
    assert!(restored.cold_reason.is_some());
    assert_eq!(restored.state.live[0].fired, 3);
}

#[test]
fn watch_bad_cache_rebuilds_and_unreadable_source_cannot_publish() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log = open(&root);
    let request = start(&mut log, 5);
    ack(&mut log, &request, 1);
    compare(&log.reader());
    std::fs::write(checkpoint(&root), b"broken").unwrap();
    assert!(compare(&log.reader()).cold_reason.is_some());
    let before = std::fs::read(checkpoint(&root)).unwrap();
    drop(log);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("00000000000000000000.jsonl"))
        .unwrap();
    file.write_all(b"!").unwrap();
    file.sync_all().unwrap();
    assert!(recover(&reader, Kind::Watch).is_err());
    assert_eq!(std::fs::read(checkpoint(&root)).unwrap(), before);
}

#[test]
#[ignore = "requires LATTICE_RECOVERY_FIXTURE pointing to a disposable segmented ledger"]
fn offline_watch_recovery_matches_reference() {
    let root = PathBuf::from(
        std::env::var_os("LATTICE_RECOVERY_FIXTURE").expect("set LATTICE_RECOVERY_FIXTURE"),
    );
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let began = std::time::Instant::now();
    let old = crate::components::subscription_history::restore(
        &reader,
        "Watch",
        "Unwatch",
        "watch",
        |_| true,
    )
    .unwrap();
    let expected: Value = as_value(&old);
    eprintln!(
        "reference watches: {:?}, live {}",
        began.elapsed(),
        old.live.len()
    );
    let began = std::time::Instant::now();
    let cold = recover(&reader, Kind::Watch).unwrap();
    assert_eq!(as_value(&cold.state), expected);
    eprintln!(
        "watch recovery: {:?}, headers {}, bodies {}, cold {:?}",
        began.elapsed(),
        cold.headers,
        cold.bodies,
        cold.cold_reason
    );
    drop(reader);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let began = std::time::Instant::now();
    let warm = recover(&reader, Kind::Watch).unwrap();
    assert_eq!(as_value(&warm.state), expected);
    assert!(warm.cold_reason.is_none());
    assert_eq!((warm.headers, warm.bodies), (0, warm.state.live.len()));
    eprintln!(
        "warm watches: {:?}, headers {}, bodies {}",
        began.elapsed(),
        warm.headers,
        warm.bodies
    );
}
