use super::*;
use crate::{core_events as ce, EventDraft, EventLog};
use serde_json::json;

fn fixture(home: &Path) -> (PathBuf, Vec<EventEnvelope>) {
    let path = crate::ledgers::dir(home).join("20260101-tui.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let docs = crate::document::documents_dir(&path);
    fs::create_dir(&docs).unwrap();
    let attachment = docs.join("keep.txt");
    fs::write(&attachment, "original").unwrap();
    let mut log = EventLog::in_memory(ce::core_event_decls(), "original-stream");
    let user = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"retain this history"})),
            "ui",
        )
        .unwrap();
    let tool = log.append(EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&user.id], json!({"call":"old-call","status":"ok","output":{"file":attachment,"bytes":8,"lines":1,"preview":"original"}})), "tool").unwrap();
    let events = vec![user, tool];
    let mut bytes = Vec::new();
    // Blank physical lines and CRLF are not logical sequence numbers.
    bytes.extend_from_slice(b"\n");
    for event in &events {
        bytes.extend(serde_json::to_vec(event).unwrap());
        bytes.extend_from_slice(b"\r\n");
    }
    fs::write(&path, bytes).unwrap();
    (path, events)
}

#[test]
fn offline_copy_preserves_records_originals_and_absolute_artifacts_without_double_listing() {
    let home = tempfile::tempdir().unwrap();
    let (source, events) = fixture(home.path());
    let original = fs::read(&source).unwrap();
    let report = migrate_checked(&source, 1, |_| Ok(())).unwrap();
    assert_eq!(report.events, 2);
    assert_eq!(report.documents, 1);
    assert_eq!(
        report.source_sha256,
        format!("{:x}", Sha256::digest(&original))
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(
        fs::read_to_string(crate::document::documents_dir(&source).join("keep.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(crate::document::documents_dir(&report.destination).join("keep.txt"))
            .unwrap(),
        "original"
    );
    assert_eq!(
        crate::EventLog::source_paths(&report.destination)
            .unwrap()
            .len(),
        2
    );
    let restored = EventLog::open(
        ce::core_event_decls(),
        "original-stream",
        Some(report.destination.clone()),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(restored.replay(1).unwrap()).unwrap(),
        serde_json::to_value(&events).unwrap()
    );
    let listed: Vec<_> = crate::ledgers::all(home.path())
        .into_iter()
        .map(|path| path.canonicalize().unwrap())
        .collect();
    assert_eq!(listed, vec![report.destination.clone()]);
    assert!(
        EventLog::open(
            ce::core_event_decls(),
            "original-stream",
            Some(source.clone())
        )
        .is_err(),
        "retained originals must not be resumed by this runtime"
    );
    assert!(migrate_checked(&source, 1, |_| Ok(())).is_err());
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn truncated_changed_or_existing_inputs_never_publish_a_partial_copy() {
    let home = tempfile::tempdir().unwrap();
    let (source, _) = fixture(home.path());
    let mut original = fs::read(&source).unwrap();
    original.extend_from_slice(b"{\"v\":");
    fs::write(&source, &original).unwrap();
    assert!(migrate_checked(&source, 1, |_| Ok(())).is_err());
    assert!(!source.with_extension("ledger").exists());
    assert_eq!(fs::read(&source).unwrap(), original);

    let second = tempfile::tempdir().unwrap();
    let (source, _) = fixture(second.path());
    let calls = std::cell::Cell::new(0);
    let result = migrate_checked(&source, 1, |path| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            // Refusing a writer happens before source parsing or publication.
            return Err(io::Error::other(format!("busy {}", path.display())));
        }
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(calls.get(), 1);
    assert!(!source.with_extension("ledger").exists());

    calls.set(0);
    let result = migrate_checked(&source, 1, |path| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            let mut changed = fs::read(path)?;
            changed.push(b'\n');
            fs::write(path, changed)?;
        }
        Ok(())
    });
    assert!(
        result.is_err(),
        "a change during the last writer probe must be caught"
    );
    assert_eq!(calls.get(), 2);
    assert!(!source.with_extension("ledger").exists());

    let from = second.path().join("staged");
    let to = second.path().join("occupied");
    fs::create_dir(&from).unwrap();
    fs::create_dir(&to).unwrap();
    assert!(
        rename_new(&from, &to).is_err(),
        "even an empty destination must not be overwritten"
    );
    assert!(from.is_dir());
    assert!(to.is_dir());
}

#[cfg(target_os = "macos")]
#[test]
fn native_writer_probe_sees_old_runtimes_without_advisory_locks() {
    let file = tempfile::NamedTempFile::new().unwrap();
    assert!(ensure_offline(file.path()).is_err());
    let path = file.into_temp_path();
    ensure_offline(&path).unwrap();
}
