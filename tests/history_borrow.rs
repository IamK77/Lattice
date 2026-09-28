//! A history view borrows envelopes; it does not own a second parsed ledger.
use lattice::{core_events as ce, EventDraft, EventLog};
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn a_fixed_prefix_borrows_original_payloads_and_excludes_later_appends() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "borrowed");
    let reader = log.reader();
    for i in 0..130 {
        log.append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text": format!("{i}:{}", "x".repeat(2048))}),
            ),
            "ui",
        )
        .unwrap();
    }
    let through = reader.snapshot_end();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"live"})),
        "ui",
    )
    .unwrap();
    let mut originals = std::collections::HashMap::new();
    reader
        .scan_back::<()>(|event, _| {
            originals.insert(
                event.seq,
                event.payload["text"].as_str().unwrap().as_ptr() as usize,
            );
            Ok(None)
        })
        .unwrap();
    let copied_before = reader.cost().events.load(Ordering::Relaxed);
    let mut seen = Vec::new();
    reader
        .visit_prefix(through, |batch| {
            assert!(batch.len() <= 64);
            for event in batch {
                assert_eq!(
                    event.payload["text"].as_str().unwrap().as_ptr() as usize,
                    originals[&event.seq],
                    "The UI must borrow the original tree, not clone it"
                );
                seen.push(event.seq);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, (1..=130).collect::<Vec<_>>());
    assert_eq!(reader.cost().events.load(Ordering::Relaxed), copied_before);
    reader
        .visit_prefix(0, |_| panic!("An empty prefix must not visit anything"))
        .unwrap();
    let mut count = 0;
    reader
        .visit_prefix(u64::MAX, |batch| {
            count += batch.len();
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 131);
}

#[test]
fn callback_failure_stops_before_later_batches_and_preserves_the_error() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "callback-failure");
    for _ in 0..70 {
        log.append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "fixture"})),
            "ui",
        )
        .unwrap();
    }
    let reader = log.reader();
    let mut batches = 0;
    let error = reader
        .visit_prefix(reader.snapshot_end(), |_| {
            batches += 1;
            // Appending here also proves the history lock is released before callbacks.
            log.append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "later"})),
                "ui",
            )
            .unwrap();
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fixture refusal",
            ))
        })
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(error.to_string(), "fixture refusal");
    assert_eq!(batches, 1);
    assert_eq!(reader.len(), 71);
}

#[test]
fn an_open_ledger_summary_matches_disk_without_copying_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let mut log = EventLog::open(ce::core_event_decls(), "summary", Some(path.clone())).unwrap();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"Title"})),
        "ui",
    )
    .unwrap();
    let reader = log.reader();
    let before = reader.cost().events.load(Ordering::Relaxed);
    assert_eq!(
        lattice::ledgers::summarize_reader(&path, &reader).unwrap(),
        lattice::ledgers::summarize(&path)
    );
    assert_eq!(reader.cost().events.load(Ordering::Relaxed), before);
}
