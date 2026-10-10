use super::*;

#[test]
fn wizard_keeps_the_original_latest_target_and_rejects_its_disappearance() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".lattice/ledgers");
    std::fs::create_dir_all(&dir).unwrap();
    let original = dir.join("20260101-000000.jsonl");
    std::fs::write(&original, "{\"seq\":1}\n").unwrap();
    let selected = ConversationSelection::capture(home.path(), Resume::Latest).unwrap();
    std::fs::write(dir.join("20260102-000000.jsonl"), "{\"seq\":1}\n").unwrap();
    let selected = selected.finish(home.path()).unwrap();
    assert!(selected.reopened());
    assert_eq!(selected.path(), original);
    let selected =
        ConversationSelection::capture(home.path(), Resume::Named("20260101-000000".into()))
            .unwrap();
    std::fs::remove_file(original).unwrap();
    assert!(selected.finish(home.path()).is_err());
}

#[test]
fn cancelled_fresh_selection_has_no_filesystem_side_effects() {
    let home = tempfile::tempdir().unwrap();
    let _selection = ConversationSelection::capture(home.path(), Resume::Fresh).unwrap();
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn startup_note_preserves_entry_time_and_is_consumed_by_the_first_frame() {
    let mut trace = Trace::start();
    // A wall-clock label is deliberately unrelated to elapsed measurement.
    trace.started_at = "2000-01-01T00:00:00.000Z".into();
    trace.checkpoint("config");
    trace.selected(true);
    trace.history_snapshot(42);
    let mut pending = Some(trace);
    let kernel = lattice::startup::Timings {
        total_ms: 15.0,
        phases_ms: [("log_open".to_string(), 15.0)].into(),
        ..Default::default()
    };
    let note = Trace::first_frame(&mut pending, &kernel).unwrap();
    assert_eq!(note.started_at, "2000-01-01T00:00:00.000Z");
    assert!(chrono::DateTime::parse_from_rfc3339(&note.first_frame_at).is_ok());
    assert!(note.resumed);
    assert_eq!(note.history_events, 42);
    assert_eq!(note.kernel.total_ms, 15.0);
    let sum: f64 = note.frontend.phases_ms.values().sum();
    assert!((sum - note.frontend.total_ms).abs() < 1e-6);
    for phase in ["config", "ledger_select", "history_snapshot", "first_draw"] {
        assert!(
            note.frontend.phases_ms.contains_key(phase),
            "missing {phase}"
        );
    }
    assert!(Trace::first_frame(&mut pending, &kernel).is_none());
}

#[test]
fn continuing_picks_the_last_real_conversation_not_the_last_file() {
    let fake_home = tempfile::tempdir().unwrap();
    let dir = fake_home.path().join(".lattice").join("ledgers");
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, body: &str| {
        std::fs::write(dir.join(name), body).unwrap();
    };
    write("20260101-000000.jsonl", "{\"seq\":1}\n");
    write("20260102-000000.jsonl", "{\"seq\":1}\n{\"seq\":2}\n");
    write("20260103-000000.jsonl", "");
    write("not-a-ledger.txt", "hello");

    let (picked, reopened) = ledger_for(fake_home.path(), Resume::Latest).unwrap();
    assert!(reopened, "continuing means reopening");
    assert_eq!(
        picked.file_name().unwrap(),
        "20260102-000000.jsonl",
        "the empty file is newer, and is not a conversation"
    );
    // Name order, not mtime: touching the older conversation does not select it.
    let touched = dir.join("20260101-000000.jsonl");
    std::fs::write(&touched, "{\"seq\":1}\n{\"seq\":2}\n").unwrap();
    let (again, _) = ledger_for(fake_home.path(), Resume::Latest).unwrap();
    assert_eq!(again.file_name().unwrap(), "20260102-000000.jsonl");
}

#[test]
fn a_fresh_launch_writes_somewhere_new() {
    let fake_home = tempfile::tempdir().unwrap();
    let dir = fake_home.path().join(".lattice").join("ledgers");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("20260101-000000.jsonl"), "{}\n").unwrap();
    let (fresh, reopened) = ledger_for(fake_home.path(), Resume::Fresh).unwrap();
    assert!(!reopened);
    assert!(!fresh.exists(), "selection must not open the new ledger");
    assert_eq!(fresh.extension().unwrap(), "ledger");
}

#[test]
fn a_name_that_is_not_there_says_what_is() {
    let fake_home = tempfile::tempdir().unwrap();
    let dir = fake_home.path().join(".lattice").join("ledgers");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("20260102-000000.jsonl"), "{}\n").unwrap();
    let refused =
        ledger_for(fake_home.path(), Resume::Named("20260101-000000".into())).unwrap_err();
    let said = refused.to_string();
    assert!(
        said.contains("20260102-000000"),
        "it lists the real ones: {said}"
    );
    let (found, _) = ledger_for(
        fake_home.path(),
        Resume::Named("20260102-000000.jsonl".into()),
    )
    .expect("with the suffix");
    assert_eq!(found.file_name().unwrap(), "20260102-000000.jsonl");
}

#[test]
fn continuing_with_no_history_refuses_rather_than_starting_blank() {
    let fake_home = tempfile::tempdir().unwrap();
    assert!(ledger_for(fake_home.path(), Resume::Latest).is_err());
}

#[test]
fn ensure_workspace_creates_a_missing_directory() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("workspace");
    let configured = dir.display().to_string();
    ensure_workspace(Some(&configured)).unwrap();
    assert!(dir.is_dir());
    ensure_workspace(Some(&configured)).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    ensure_workspace(None).unwrap();
    assert!(
        !dir.exists(),
        "no workspace is made when none is configured"
    );
}
