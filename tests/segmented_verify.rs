use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use lattice::{EventDraft, EventLog, EventTypeDecl};
use serde_json::json;

fn snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn verify(path: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lattice"))
        .arg("--verify-ledger")
        .arg(path)
        // If this command accidentally starts a normal assembly, it must fail.
        .env("LATTICE_ASSEMBLY", path.join("missing-assembly.json"))
        .output()
        .unwrap()
}

#[test]
fn offline_verification_is_read_only_and_does_not_start_an_assembly() {
    for mode in [
        "valid",
        "locked",
        "sealed-damage",
        "torn-active",
        "missing-newline",
        "missing-index",
    ] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("fixture.ledger");
        let mut log = EventLog::open_segmented(
            vec![EventTypeDecl::new("fixture.event", "verification")],
            "fixture",
            root.clone(),
            1,
        )
        .unwrap();
        for text in ["first", "second"] {
            log.append(
                EventDraft::new("fixture.event", &[], json!({"text": text})),
                "fixture",
            )
            .unwrap();
        }
        log.verify_segments().unwrap();
        if mode == "locked" {
            let before = snapshot(&root);
            assert!(
                !verify(&root).status.success(),
                "must refuse an active writer"
            );
            assert_eq!(snapshot(&root), before);
            continue;
        }
        drop(log);
        let active = root.join("00000000000000000001.jsonl");
        match mode {
            "sealed-damage" => {
                let file = root.join("00000000000000000000.jsonl");
                let bytes = fs::read_to_string(&file).unwrap().replace("first", "FIRST");
                fs::write(file, bytes).unwrap();
            }
            "torn-active" => {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&active)
                    .unwrap()
                    .write_all(b"{torn")
                    .unwrap();
            }
            "missing-newline" => {
                let file = fs::OpenOptions::new().write(true).open(active).unwrap();
                file.set_len(file.metadata().unwrap().len() - 1).unwrap();
            }
            "missing-index" => fs::remove_file(root.join("00000000000000000000.index")).unwrap(),
            _ => {}
        }
        let before = snapshot(&root);
        let output = verify(&root);
        assert_eq!(
            output.status.success(),
            matches!(mode, "valid" | "missing-index"),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            snapshot(&root),
            before,
            "verification changed files in {mode}"
        );
        if output.status.success() {
            assert!(String::from_utf8_lossy(&output.stdout).contains("Ledger verified"));
        }
    }
}

#[test]
fn offline_verification_does_not_create_a_missing_lock_or_ledger() {
    let home = tempfile::tempdir().unwrap();
    assert!(!verify(home.path()).status.success());
    assert!(snapshot(home.path()).is_empty());
}
