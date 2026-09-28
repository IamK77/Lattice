use super::*;
use crate::{EventDraft, EventEnvelope, EventLog};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn open(root: &Path) -> EventLog {
    EventLog::open_segmented(ce::core_event_decls(), "shell-recovery", root.into(), 4096).unwrap()
}

fn append(log: &mut EventLog, kind: &str, causes: &[&str], body: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, body), "fixture")
        .unwrap()
}

fn start(log: &mut EventLog) -> EventEnvelope {
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({
            "call": "reused-call", "tool": "Run", "arguments": {"command": "do not execute"}
        }),
    )
}

fn ack(log: &mut EventLog, started: &EventEnvelope) -> EventEnvelope {
    append(
        log,
        ce::TOOL_EXEC_COMPLETED,
        &[&started.id],
        json!({
            "call": "reused-call", "status": "ok", "result": {
                "background": true, "job": "same-job-label", "pid": 123
            }
        }),
    )
}

fn wake(log: &mut EventLog, starts: &[&EventEnvelope]) {
    let causes: Vec<_> = starts.iter().map(|event| event.id.as_str()).collect();
    append(
        log,
        ce::WAKE,
        &causes,
        json!({
            "source": "background:same-job-label", "summary": "ended",
            "body": {"interrupted": "restart"}
        }),
    );
}

fn checkpoint(root: &Path) -> PathBuf {
    root.join(format!(
        "checkpoint-{:x}.json",
        Sha256::digest(CONSUMER.as_bytes())
    ))
}

fn stored(reader: &LogReader) -> State {
    reader
        .load_checkpoint(CONSUMER, VERSION, reader.snapshot_end())
        .unwrap()
        .state
        .unwrap()
}

/// The previous implementation, retained only as an independent semantic oracle.
fn reference(reader: &LogReader) -> Vec<Job> {
    let mut receipts = Vec::new();
    reader
        .scan_back_types(&[ce::TOOL_EXEC_COMPLETED], |event, _| {
            if event.payload["result"]["background"] == true {
                if let Some(started) = event.causes.first() {
                    receipts.push((started.clone(), event.payload["result"].clone()));
                }
            }
            Ok(None::<()>)
        })
        .unwrap();
    receipts
        .into_iter()
        .rev()
        .filter_map(|(started, result)| {
            if reader
                .any_header(|header| {
                    header.event_type == ce::WAKE && header.causes.contains(&started)
                })
                .unwrap()
            {
                return None;
            }
            let command = reader.get(&started).unwrap().map_or(Value::Null, |event| {
                event.payload["arguments"]["command"].clone()
            });
            Some(Job {
                started,
                job: result["job"].clone(),
                pid: result["pid"].clone(),
                command,
            })
        })
        .collect()
}

#[test]
fn warm_recovery_reads_only_the_suffix_and_keeps_only_unfinished_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log = open(&root);
    for _ in 0..60 {
        let started = start(&mut log);
        ack(&mut log, &started);
        wake(&mut log, &[&started]);
    }
    let first = recover(&log.reader()).unwrap();
    assert!(first.cold_reason.is_some());
    assert_eq!(first.headers as u64, log.reader().snapshot_end());
    assert!(first.jobs.is_empty());
    assert!(stored(&log.reader()).pending.is_empty());
    drop(log);

    // Reopen the source too: process-local caches must not explain the result.
    let mut log = open(&root);
    let reader = log.reader();
    let warm = recover(&reader).unwrap();
    assert!(warm.cold_reason.is_none());
    assert_eq!((warm.headers, warm.bodies), (0, 0));
    assert!(warm.jobs.is_empty());
    let started = start(&mut log);
    ack(&mut log, &started);
    let tail = recover(&reader).unwrap();
    assert!(tail.cold_reason.is_none());
    assert_eq!(tail.headers, 2);
    assert_eq!(tail.jobs, reference(&reader));
    assert_eq!(tail.jobs.len(), 1);
    assert_eq!(stored(&reader).pending.len(), 1);
    let again = recover(&reader).unwrap();
    assert_eq!(again.headers, 0);
    assert_eq!(
        again.bodies, 2,
        "only unresolved receipt and request bodies are needed"
    );
    assert_eq!(
        again.jobs, tail.jobs,
        "a proposed settlement is not yet a recorded wake"
    );
    wake(&mut log, &[&started]);
    let settled = recover(&reader).unwrap();
    assert_eq!((settled.headers, settled.bodies), (1, 0));
    assert!(settled.jobs.is_empty());
    assert!(stored(&reader).pending.is_empty());
}

#[test]
fn wake_order_duplicates_and_reused_labels_match_full_history() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("main.ledger"));
    recover(&log.reader()).unwrap();
    let early = start(&mut log);
    wake(&mut log, &[&early]);
    ack(&mut log, &early);
    ack(&mut log, &early);
    let pending = start(&mut log);
    ack(&mut log, &pending);
    ack(&mut log, &pending);
    let quiet = start(&mut log);
    append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&quiet.id],
        json!({
            "call": "reused-call", "status": "ok", "result": {"background": false}
        }),
    );
    let recovered = recover(&log.reader()).unwrap();
    assert_eq!(recovered.jobs, reference(&log.reader()));
    assert_eq!(recovered.jobs.len(), 2);
    assert!(recovered.jobs.iter().all(|job| job.started == pending.id));
    wake(&mut log, &[&early, &pending]);
    assert!(recover(&log.reader()).unwrap().jobs.is_empty());
    assert!(stored(&log.reader()).pending.is_empty());
}

#[test]
fn a_late_receipt_rebuilds_instead_of_forgetting_a_pre_checkpoint_wake() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("main.ledger"));
    let early = start(&mut log);
    wake(&mut log, &[&early]);
    recover(&log.reader()).unwrap();
    assert!(stored(&log.reader()).pending.is_empty());
    ack(&mut log, &early);
    let recovered = recover(&log.reader()).unwrap();
    assert!(recovered.cold_reason.unwrap().contains("pre-checkpoint"));
    assert_eq!(recovered.jobs, reference(&log.reader()));
    assert!(recovered.jobs.is_empty());
    let warm = recover(&log.reader()).unwrap();
    assert_eq!((warm.headers, warm.bodies), (0, 0));
    // A duplicate acknowledgement after a completed checkpoint is also closed.
    ack(&mut log, &early);
    assert!(recover(&log.reader()).unwrap().jobs.is_empty());
}

#[test]
fn invalid_derived_state_rebuilds_but_source_failure_never_advances_it() {
    use std::io::{Seek, SeekFrom, Write};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log = open(&root);
    let started = start(&mut log);
    ack(&mut log, &started);
    let first = recover(&log.reader()).unwrap();
    std::fs::write(checkpoint(&root), b"invalid checkpoint").unwrap();
    let rebuilt = recover(&log.reader()).unwrap();
    assert!(rebuilt.cold_reason.is_some());
    assert_eq!(rebuilt.jobs, first.jobs);
    let before = std::fs::read(checkpoint(&root)).unwrap();
    drop(log);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    // Damage an unresolved source after opening it, before its first body read.
    let volume = root.join("00000000000000000000.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(volume)
        .unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"!").unwrap();
    file.sync_all().unwrap();
    assert!(recover(&reader).is_err());
    assert_eq!(std::fs::read(checkpoint(&root)).unwrap(), before);
}

#[test]
fn same_stream_name_does_not_share_recovery_state_between_ledgers() {
    let temp = tempfile::tempdir().unwrap();
    let mut one = open(&temp.path().join("one.ledger"));
    let started = start(&mut one);
    ack(&mut one, &started);
    assert_eq!(recover(&one.reader()).unwrap().jobs.len(), 1);
    let other = open(&temp.path().join("other.ledger"));
    let empty = recover(&other.reader()).unwrap();
    assert!(empty.cold_reason.is_some());
    assert!(empty.jobs.is_empty());
}

/// A disposable real ledger exercises the same production reducer and compares
/// it with the old algorithm. No components or historical commands are started.
#[test]
#[ignore = "requires LATTICE_RECOVERY_FIXTURE pointing to a disposable segmented ledger"]
fn offline_background_recovery_matches_reference() {
    let root = PathBuf::from(
        std::env::var_os("LATTICE_RECOVERY_FIXTURE").expect("set LATTICE_RECOVERY_FIXTURE"),
    );
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let began = std::time::Instant::now();
    let expected = reference(&reader);
    eprintln!(
        "reference background recovery: {:?}, pending {}",
        began.elapsed(),
        expected.len()
    );
    let began = std::time::Instant::now();
    let recovered = recover(&reader).unwrap();
    assert_eq!(recovered.jobs, expected);
    eprintln!(
        "background recovery: {:?}, headers {}, bodies {}, cold {:?}",
        began.elapsed(),
        recovered.headers,
        recovered.bodies,
        recovered.cold_reason
    );
    drop(reader);
    let reader = LogReader::segmented_snapshot(&root).unwrap();
    let began = std::time::Instant::now();
    let warm = recover(&reader).unwrap();
    assert_eq!(warm.jobs, expected);
    assert!(warm.cold_reason.is_none());
    assert_eq!(warm.headers, 0);
    assert_eq!(warm.bodies, warm.jobs.len() * 2);
    eprintln!(
        "warm background recovery: {:?}, headers {}, bodies {}",
        began.elapsed(),
        warm.headers,
        warm.bodies
    );
}
