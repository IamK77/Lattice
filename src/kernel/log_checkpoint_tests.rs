use super::*;
use crate::{core_events as ce, EventDraft, EventLog};
use serde_json::json;

fn open(root: &Path) -> EventLog {
    EventLog::open_segmented(ce::core_event_decls(), "checkpoint", root.to_path_buf(), 1).unwrap()
}

#[test]
fn checkpoint_binds_middle_records_not_only_count_or_last_identity() {
    use crate::kernel::segmented::Ledger;
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original.ledger");
    let changed = temp.path().join("changed.ledger");
    let mut source = EventLog::in_memory(ce::core_event_decls(), "checkpoint");
    input(&mut source, "first");
    input(&mut source, "middle");
    input(&mut source, "last");
    let events = source.replay(1).unwrap();
    for (root, replace_middle) in [(&original, false), (&changed, true)] {
        let mut ledger = Ledger::create(root, "checkpoint", 1).unwrap();
        for event in &events {
            let mut event = event.clone();
            if replace_middle && event.seq == 2 {
                event.payload["text"] = json!("edited");
            }
            ledger.append(&event).unwrap();
        }
    }
    let log = open(&original);
    log.reader()
        .save_checkpoint("fixture", 1, 3, &"original state")
        .unwrap();
    drop(log);
    fs::copy(path(&original, "fixture"), path(&changed, "fixture")).unwrap();
    let log = open(&changed);
    assert_eq!(
        serde_json::to_value(log.reader().get(&events[2].id).unwrap().unwrap()).unwrap(),
        serde_json::to_value(&events[2]).unwrap()
    );
    let recovered = log
        .reader()
        .load_checkpoint::<String>("fixture", 1, 3)
        .unwrap();
    assert!(recovered.state.is_none());
    assert!(recovered
        .cold_reason
        .unwrap()
        .contains("different ledger prefix"));
}

fn input(log: &mut EventLog, text: &str) {
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":text})),
        "ui",
    )
    .unwrap();
}

#[test]
fn checkpoints_survive_rotation_and_reopen_and_cannot_move_backwards_or_ahead() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state.ledger");
    let mut log = open(&root);
    input(&mut log, "one");
    let reader = log.reader();
    let cold = reader
        .load_checkpoint::<Vec<String>>("fixture", 1, 1)
        .unwrap();
    assert!(cold.state.is_none());
    assert_eq!(
        cold.cold_reason.as_deref(),
        Some("recovery checkpoint has not been created yet; rebuilding from the ledger")
    );
    assert!(reader
        .save_checkpoint("fixture", 1, 1, &vec!["one"])
        .unwrap());
    input(&mut log, "two");
    drop(reader);
    drop(log);
    let log = open(&root);
    let reader = log.reader();
    let recovered = reader
        .load_checkpoint::<Vec<String>>("fixture", 1, 2)
        .unwrap();
    assert_eq!(recovered.through, 1);
    assert_eq!(recovered.state.unwrap(), ["one"]);
    assert!(recovered.cold_reason.is_none());
    assert!(reader
        .save_checkpoint("fixture", 1, 2, &vec!["one", "two"])
        .unwrap());
    assert!(!reader
        .save_checkpoint("fixture", 1, 1, &vec!["stale"])
        .unwrap());
    let before = fs::read(path(&root, "fixture")).unwrap();
    assert!(reader
        .save_checkpoint("fixture", 1, 3, &vec!["future"])
        .is_err());
    assert_eq!(fs::read(path(&root, "fixture")).unwrap(), before);
    assert_eq!(
        reader
            .load_checkpoint::<Vec<String>>("fixture", 1, 2)
            .unwrap()
            .state
            .unwrap(),
        ["one", "two"]
    );
    assert!(reader
        .load_checkpoint::<Vec<String>>("fixture", 1, 1)
        .unwrap()
        .state
        .is_none());
    assert!(reader
        .load_checkpoint::<Vec<String>>("fixture", 2, 2)
        .unwrap()
        .state
        .is_none());
}

#[test]
fn corrupt_or_unpublished_derived_state_falls_back_without_mutating_the_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state.ledger");
    let mut log = open(&root);
    input(&mut log, "one");
    let reader = log.reader();
    reader
        .save_checkpoint("fixture", 1, 1, &vec!["one"])
        .unwrap();
    let target = path(&root, "fixture");
    let original = fs::read(&target).unwrap();
    fs::write(target.with_extension("next"), b"unfinished").unwrap();
    assert!(reader
        .load_checkpoint::<Vec<String>>("fixture", 1, 1)
        .unwrap()
        .state
        .is_some());
    let mut stored: Stored = serde_json::from_slice(&original).unwrap();
    stored.data.body = "[\"forged\"]".into();
    fs::write(&target, serde_json::to_vec(&stored).unwrap()).unwrap();
    let cold = reader
        .load_checkpoint::<Vec<String>>("fixture", 1, 1)
        .unwrap();
    assert!(cold.state.is_none());
    assert!(cold.cold_reason.unwrap().contains("checksum"));
    assert_eq!(reader.snapshot_end(), 1);
    assert_eq!(
        reader
            .get(&reader.latest_id().unwrap())
            .unwrap()
            .unwrap()
            .payload["text"],
        "one"
    );
    fs::write(&target, b"{").unwrap();
    assert!(reader
        .load_checkpoint::<Vec<String>>("fixture", 1, 1)
        .unwrap()
        .cold_reason
        .is_some());
    fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(root.join("00000000000000000000.jsonl"), &target).unwrap();
    assert!(reader
        .load_checkpoint::<Vec<String>>("fixture", 1, 1)
        .unwrap()
        .state
        .is_none());
}

#[test]
fn legacy_pending_queries_share_one_fold_and_only_read_new_headers() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy.jsonl");
    let mut log = EventLog::open(ce::core_event_decls(), "checkpoint", Some(path)).unwrap();
    for _ in 0..100 {
        input(&mut log, "old history");
    }
    let first = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"a", "tool":"fixture", "arguments":{}}),
            ),
            "loop",
        )
        .unwrap();
    for _ in 0..5 {
        assert_eq!(
            log.reader()
                .pending_requests(ce::TOOL_EXEC_STARTED)
                .unwrap(),
            std::slice::from_ref(&first.id)
        );
    }
    assert_eq!(
        log.reader().pending.lock().unwrap().observed_headers,
        101,
        "repeated queries must not rescan the legacy prefix"
    );
    log.append(
        EventDraft::new(ce::INTERRUPTED, &[&first.id], json!({"by":"restart"}))
            .with_reason("work is not repeated"),
        "core",
    )
    .unwrap();
    assert!(log
        .reader()
        .pending_requests(ce::TOOL_EXEC_STARTED)
        .unwrap()
        .is_empty());
    assert_eq!(
        log.reader().pending.lock().unwrap().observed_headers,
        102,
        "a settlement must require only the new suffix"
    );
    let other = EventLog::in_memory(ce::core_event_decls(), "checkpoint");
    assert!(other
        .reader()
        .pending_requests(ce::TOOL_EXEC_STARTED)
        .unwrap()
        .is_empty());
    assert_eq!(other.reader().pending.lock().unwrap().observed_headers, 0);
}

#[test]
fn root_interruption_rebuilds_legacy_pending_state_and_stays_settled() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("interrupted.ledger");
    let mut log = open(&root);
    let payload = json!({"call":"a", "tool":"fixture", "arguments":{}});
    let first = log
        .append(
            EventDraft::new(ce::TOOL_EXEC_STARTED, &[], payload.clone()),
            "loop",
        )
        .unwrap();
    let branch = log
        .append(
            EventDraft::new(ce::TOOL_EXEC_STARTED, &[&first.id], payload),
            "gate",
        )
        .unwrap();
    let reader = log.reader();
    assert_eq!(
        reader
            .pending_requests(ce::TOOL_EXEC_STARTED)
            .unwrap()
            .len(),
        2
    );
    let done = log
        .append(
            EventDraft::new(ce::INTERRUPTED, &[&first.id], json!({"by":"restart"}))
                .with_reason("unfinished work is not repeated"),
            "core",
        )
        .unwrap();
    assert!(reader
        .pending_requests(ce::TOOL_EXEC_STARTED)
        .unwrap()
        .is_empty());
    let key = format!("kernel-pending:{}", ce::TOOL_EXEC_STARTED);
    let mut stale = ce::PendingCalls::new(ce::TOOL_EXEC_STARTED);
    stale.observe(ce::EventRelations {
        id: &branch.id,
        event_type: &branch.event_type,
        causes: &branch.causes,
    });
    reader.save_checkpoint(&key, 1, done.seq, &stale).unwrap();
    drop(reader);
    drop(log);
    for _ in 0..2 {
        let log = open(&root);
        let reader = log.reader();
        assert!(reader
            .pending_requests(ce::TOOL_EXEC_STARTED)
            .unwrap()
            .is_empty());
        assert!(log.hanging(ce::TOOL_EXEC_STARTED).unwrap().is_empty());
        assert_eq!(log.len() as u64, done.seq);
        let saved = reader
            .load_checkpoint::<ce::PendingCalls>(&key, 2, done.seq)
            .unwrap();
        assert!(saved.state.unwrap().requests().is_empty());
    }
}

#[test]
fn pending_checkpoint_plus_tail_matches_full_recovery_without_emitting_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("pending.ledger");
    let mut log = open(&root);
    let first = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"a", "tool":"fixture", "arguments":{}}),
            ),
            "loop",
        )
        .unwrap();
    let branch = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&first.id],
                json!({"call":"a", "tool":"fixture", "arguments":{}}),
            ),
            "gate",
        )
        .unwrap();
    assert_eq!(
        log.hanging(ce::TOOL_EXEC_STARTED).unwrap(),
        std::slice::from_ref(&first.id)
    );
    log.append(
        EventDraft::new(ce::INTERRUPTED, &[&branch.id], json!({"by":"restart"}))
            .with_reason("unfinished work is not repeated"),
        "core",
    )
    .unwrap();
    let other = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"b", "tool":"fixture", "arguments":{}}),
            ),
            "loop",
        )
        .unwrap();
    drop(log);
    let log = open(&root);
    let before = log.len();
    let heads = log.hanging(ce::TOOL_EXEC_STARTED).unwrap();
    let full = ce::hanging_chain_heads(&log.replay(1).unwrap(), ce::TOOL_EXEC_STARTED);
    assert_eq!(heads, full);
    assert_eq!(heads, [other.id]);
    assert_eq!(
        log.len(),
        before,
        "loading state never starts or repeats work"
    );
    let checkpoint = log
        .reader()
        .load_checkpoint::<ce::PendingCalls>(
            &format!("kernel-pending:{}", ce::TOOL_EXEC_STARTED),
            2,
            before as u64,
        )
        .unwrap();
    assert_eq!(checkpoint.through, before as u64);
    assert!(checkpoint.cold_reason.is_none());
}
