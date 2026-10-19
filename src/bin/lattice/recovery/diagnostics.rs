use super::*;
use sha2::{Digest, Sha256};

#[test]
fn expected_cache_absence_is_quiet_but_corruption_remains_visible() {
    const ENV: &str = "LATTICE_TEST_RECOVERY_DIAGNOSTIC_ROOT";
    if let Some(root) = std::env::var_os(ENV) {
        let reader = LogReader::segmented_snapshot(std::path::Path::new(&root)).unwrap();
        let mut ui = Ui::replayed(&[]);
        ui.replay_prefix(&reader, reader.snapshot_end()).unwrap();
        assert!(ui.recovery.is_some());
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("history.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "fixture", root.clone(), 4096).unwrap();
    append(&mut log, ce::USER_MESSAGE, &[], json!({"text":"hello"}));
    drop(log);
    let run = |succeeds| {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "terminal_host::recovery::tests::diagnostics::expected_cache_absence_is_quiet_but_corruption_remains_visible", "--nocapture"])
            .env(ENV, &root).output().unwrap();
        assert_eq!(output.status.success(), succeeds, "{output:?}");
        String::from_utf8(output.stderr).unwrap()
    };
    let first = run(true);
    assert!(first.is_empty(), "{first}");
    let key = "terminal-state";
    let cache = root.join(format!(
        "checkpoint-{:x}.json",
        Sha256::digest(key.as_bytes())
    ));
    assert!(cache.is_file());
    std::fs::write(&cache, b"not valid json").unwrap();
    let diagnostic = run(true);
    assert!(diagnostic.contains("terminal state"), "{diagnostic}");
    assert!(run(true).is_empty());
    std::fs::remove_file(&cache).unwrap();
    std::fs::create_dir(&cache).unwrap();
    // Rebuilding cannot publish over a directory: recovery must still fail.
    let unreadable = run(false);
    assert!(unreadable.contains("Is a directory"), "{unreadable}");
}
