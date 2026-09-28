use super::*;
use serde_json::json;

fn types() -> Vec<EventTypeDecl> {
    vec![EventTypeDecl::new("fixture.event", "storage fixture")]
}
fn open(path: &std::path::Path) -> std::io::Result<EventLog> {
    EventLog::open(types(), "fixture", Some(path.to_path_buf()))
}
fn append(log: &mut EventLog, value: &str) -> EventEnvelope {
    log.append(
        EventDraft::new("fixture.event", &[], json!({"text": value})),
        "fixture",
    )
    .unwrap()
}
fn lines() -> Vec<EventEnvelope> {
    let mut log = EventLog::in_memory(types(), "fixture");
    vec![append(&mut log, "one"), append(&mut log, "two")]
}

#[test]
fn segmented_runtime_preserves_audit_causality_and_disk_backed_recovery() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("fixture.ledger");
    let mut log = EventLog::open_segmented(types(), "fixture", path.clone(), 1).unwrap();
    let first = append(&mut log, "one");
    assert!(log
        .append(
            EventDraft::new("fixture.event", &["missing"], json!({})),
            "fixture"
        )
        .is_err());
    let second = log
        .append(
            EventDraft::new("fixture.event", &[&first.id], json!({"text": "two"})),
            "fixture",
        )
        .unwrap();
    let reader = log.reader();
    assert_eq!(reader.get(&first.id).unwrap().unwrap().seq, 1);
    assert_eq!(
        reader.get(&second.id).unwrap().unwrap().causes,
        vec![first.id.clone()]
    );
    assert!(path.join("00000000000000000001.jsonl").is_file());
    assert_eq!(EventLog::stream_of(&path).as_deref(), Some("fixture"));
    drop(reader);
    drop(log);
    let mut log = open(&path).unwrap();
    let stats = log.reader().memory_stats().unwrap();
    assert_eq!(stats.in_memory_bodies, 0);
    assert_eq!(stats.cache.unwrap().decodes, 0);
    assert_eq!(log.reader().get(&first.id).unwrap().unwrap().seq, 1);
    assert_eq!(log.reader().get(&second.id).unwrap().unwrap().seq, 2);
    assert_eq!(append(&mut log, "three").seq, 3);
    drop(log);
    assert_eq!(open(&path).unwrap().reader().replay(1).unwrap().len(), 3);
}

#[test]
fn unavailable_metadata_refuses_audit_without_writing_or_poisoning_the_writer() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("fixture.ledger");
    let mut log = EventLog::open_segmented(types(), "fixture", path.clone(), 1).unwrap();
    // One sealed header exceeds the index cache, so the next lookup must
    // consult disk instead of a previously validated retained window.
    let first = log
        .append(
            EventDraft::new("fixture.event", &[], json!({})),
            &"s".repeat(9 * 1024 * 1024),
        )
        .unwrap();
    append(&mut log, "two");
    let active = path.join("00000000000000000001.jsonl");
    let before = std::fs::read(&active).unwrap();
    let directory: Value =
        serde_json::from_slice(&std::fs::read(path.join("00000000000000000000.index")).unwrap())
            .unwrap();
    let pages = path.join(directory["headers"]["file"].as_str().unwrap());
    let file = std::fs::OpenOptions::new().write(true).open(pages).unwrap();
    std::os::unix::fs::FileExt::write_all_at(&file, b"!", 0).unwrap();
    assert!(log.contains_id(&first.id).is_err());
    let error = log
        .append(
            EventDraft::new("fixture.event", &[&first.id], json!({})),
            "fixture",
        )
        .unwrap_err();
    assert!(matches!(error, AuditViolation::HistoryRead(_)));
    assert_eq!(log.len(), 2);
    assert_eq!(std::fs::read(&active).unwrap(), before);
    assert_eq!(append(&mut log, "still usable").seq, 3);
}

#[test]
fn segmented_documents_remain_resolvable_after_rotation_and_reopen() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("fixture.ledger");
    let mut declarations = types();
    declarations[0].documents = vec!["text".into()];
    let text = "document\n".repeat(DOCUMENT_THRESHOLD);
    let mut log =
        EventLog::open_segmented(declarations.clone(), "fixture", path.clone(), 1).unwrap();
    let first = append(&mut log, &text);
    let second = append(&mut log, &text);
    assert_eq!(first.payload["text"], second.payload["text"]);
    let name = first.payload["text"]["file"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("documents").join(name)).unwrap(),
        text
    );
    assert!(path.join("00000000000000000001.jsonl").is_file());
    drop(log);
    let mut log = EventLog::open(declarations, "fixture", Some(path.clone())).unwrap();
    let third = append(&mut log, &text);
    assert_eq!(first.payload["text"], third.payload["text"]);
    assert_eq!(
        log.reader().get(&first.id).unwrap().unwrap().payload,
        first.payload
    );
}

#[test]
fn segmented_wrong_stream_does_not_repair_another_streams_tail() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("fixture.ledger");
    let mut log = open(&path).unwrap();
    append(&mut log, "one");
    drop(log);
    let active = path.join("00000000000000000000.jsonl");
    OpenOptions::new()
        .append(true)
        .open(&active)
        .unwrap()
        .write_all(b"{\"x\":")
        .unwrap();
    let before = std::fs::read(&active).unwrap();
    assert!(EventLog::open(types(), "another", Some(path.clone())).is_err());
    assert_eq!(std::fs::read(&active).unwrap(), before);
    assert_eq!(open(&path).unwrap().reader().replay(1).unwrap().len(), 1);
    assert_eq!(
        std::fs::metadata(&active).unwrap().len(),
        before.len() as u64 - 5
    );
}

#[test]
fn recovery_rejects_duplicate_discontinuous_and_reversed_sequences_without_writing() {
    for seqs in [[1, 1], [1, 3], [2, 1]] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut events = lines();
        let mut bytes = Vec::new();
        for (event, seq) in events.iter_mut().zip(seqs) {
            event.seq = seq;
            serde_json::to_writer(&mut bytes, event).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(file.path(), &bytes).unwrap();
        let error = open(file.path())
            .err()
            .expect("invalid sequence must fail recovery");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("expected sequence"));
        assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
    }
}

#[test]
fn recovery_rejects_duplicate_identities_even_when_sequences_are_valid() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut events = lines();
    events[1].id = events[0].id.clone();
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, &event).unwrap();
        bytes.push(b'\n');
    }
    std::fs::write(file.path(), &bytes).unwrap();
    assert_eq!(
        open(file.path()).err().unwrap().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
}

#[test]
fn crlf_blank_lines_and_repaired_tail_preserve_body_positions_after_append() {
    for tail in [b"".as_slice(), b"\n", b"\r\n", b"\n{\"torn\":\"\xf0\x9f"] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let events = lines();
        let mut bytes = b"\r\n  \n".to_vec();
        serde_json::to_writer(&mut bytes, &events[0]).unwrap();
        bytes.extend_from_slice(b"\r\n\n");
        serde_json::to_writer(&mut bytes, &events[1]).unwrap();
        bytes.extend_from_slice(tail);
        std::fs::write(file.path(), bytes).unwrap();
        let mut log = open(file.path()).unwrap();
        let third = append(&mut log, "three");
        for event in events.iter().chain(std::iter::once(&third)) {
            assert_eq!(log.get(&event.id).unwrap().unwrap().payload, event.payload);
        }
        drop(log);
        assert_eq!(open(file.path()).unwrap().replay(1).unwrap().len(), 3);
    }
}

#[test]
fn external_length_changes_fail_before_publishing_an_unreadable_event() {
    for truncate in [false, true] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut log = open(file.path()).unwrap();
        append(&mut log, "one");
        if truncate {
            file.as_file().set_len(0).unwrap();
        } else {
            OpenOptions::new()
                .append(true)
                .open(file.path())
                .unwrap()
                .write_all(b"\n")
                .unwrap();
        }
        let observed = std::fs::read(file.path()).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            append(&mut log, "must not publish")
        }));
        assert!(
            result.is_err(),
            "a displaced writer must fail-stop before delivery"
        );
        assert_eq!(log.reader().len(), 1);
        assert_eq!(std::fs::read(file.path()).unwrap(), observed);
    }
}

#[test]
fn replacement_path_cannot_split_the_pinned_reader_and_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("active.jsonl");
    let moved = dir.path().join("original.jsonl");
    let mut log = open(&path).unwrap();
    let first = append(&mut log, "original");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(&path, b"replacement must remain untouched").unwrap();
    let second = append(&mut log, "still original");
    assert_eq!(
        log.get(&first.id).unwrap().unwrap().payload["text"],
        "original"
    );
    assert_eq!(
        log.get(&second.id).unwrap().unwrap().payload["text"],
        "still original"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"replacement must remain untouched"
    );
    drop(log);
    assert_eq!(open(&moved).unwrap().replay(1).unwrap().len(), 2);
}

#[test]
fn positional_body_reads_do_not_move_a_shared_file_cursor() {
    use std::io::{Seek, SeekFrom};
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut log = open(file.path()).unwrap();
    let event = append(&mut log, "read without seeking the writer");
    let writer = log.writer.as_mut().unwrap();
    writer.seek(SeekFrom::Start(7)).unwrap();
    assert_eq!(log.get(&event.id).unwrap().unwrap().payload, event.payload);
    assert_eq!(log.writer.as_mut().unwrap().stream_position().unwrap(), 7);
}
