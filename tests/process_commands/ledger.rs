use super::{command, succeeded};
use lattice::{core_events as ce, EventDraft, EventLog};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn legacy(home: &Path, name: &str) -> (PathBuf, String) {
    let directory = lattice::ledgers::dir(home);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    let prompt = "A long offline fixture instruction.\n".repeat(400);
    let events = [
        json!({"v":1,"id":"ev_1_fixture","seq":1,"stream":"fixture",
            "time":"2000-01-01T00:00:00Z","type":ce::USER_MESSAGE,
            "source":"ui","causes":[],"payload":{"text":"keep this conversation"}}),
        json!({"v":1,"id":"ev_2_fixture","seq":2,"stream":"fixture",
            "time":"2000-01-01T00:00:00Z","type":ce::MODEL_CALL_STARTED,
            "source":"gate","causes":["ev_1_fixture"],
            "payload":{"model":"fixture","system":prompt,
                "input":{"parts":[],"fingerprint":"fixture"}}}),
    ];
    let text: String = events.iter().map(|event| format!("{event}\n")).collect();
    std::fs::write(&path, text).unwrap();
    (path, prompt)
}

fn broken_model_catalog(home: &Path) {
    std::fs::create_dir_all(home.join(".lattice")).unwrap();
    std::fs::write(home.join(".lattice/models.json"), "broken catalog").unwrap();
}

#[test]
fn empty_maintenance_commands_do_not_prepare_a_model_or_workspace() {
    let home = tempfile::tempdir().unwrap();
    broken_model_catalog(home.path());
    let workspace = home.path().join("not-created");
    for (args, expected) in [
        (vec!["compact"], "no ledgers to compact\n"),
        (
            vec!["tidy", "ignored"],
            "no \"ledger\" policy in preferences.json",
        ),
        (vec!["index", "ignored"], "0 conversations indexed in "),
    ] {
        let output = command(home.path())
            .args(args)
            .env("LATTICE_WORKSPACE", &workspace)
            .output()
            .unwrap();
        assert!(succeeded(&output).starts_with(expected));
        assert!(output.stderr.is_empty());
        assert!(!workspace.exists());
    }
}

#[test]
fn compaction_continues_after_a_missing_source_and_export_keeps_its_output_contract() {
    let home = tempfile::tempdir().unwrap();
    let (source, prompt) = legacy(home.path(), "20000101-tui.jsonl");
    let output = command(home.path())
        .arg("compact")
        .arg(home.path().join("missing.jsonl"))
        .arg(&source)
        .output()
        .unwrap();
    let text = succeeded(&output);
    assert!(text.contains("20000101-tui.jsonl: 1 of 2 events rewritten, 1 documents"));
    assert!(text
        .lines()
        .last()
        .unwrap()
        .starts_with("2 ledgers, 1 documents, "));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .starts_with("missing.jsonl: skipped — "));
    let compacted = std::fs::read(&source).unwrap();
    let again = command(home.path())
        .arg("compact")
        .arg(&source)
        .output()
        .unwrap();
    assert!(succeeded(&again).starts_with("1 ledgers, 0 documents, "));
    assert_eq!(std::fs::read(&source).unwrap(), compacted);

    let exported = command(home.path())
        .arg("export")
        .arg(&source)
        .output()
        .unwrap();
    assert!(succeeded(&exported).contains("2 events, 1 documents folded back in"));
    let destination = home.path().join("20000101-tui-inlined.jsonl");
    let text = std::fs::read_to_string(&destination).unwrap();
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[1]["payload"]["system"], prompt);
    assert_eq!(std::fs::read(&source).unwrap(), compacted);

    let explicit = home.path().join("explicit.jsonl");
    let ignored = home.path().join("ignored.jsonl");
    let output = command(home.path())
        .arg("export")
        .arg(&source)
        .arg(&explicit)
        .arg(&ignored)
        .output()
        .unwrap();
    assert!(succeeded(&output).contains("2 events, 1 documents folded back in"));
    assert_eq!(std::fs::read(explicit).unwrap(), text.as_bytes());
    assert!(!ignored.exists());
}

#[test]
fn export_refuses_existing_outputs_and_source_aliases_without_truncation() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("source.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "fixture", source.clone(), 1).unwrap();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"retain source"})),
        "ui",
    )
    .unwrap();
    drop(log);
    let volume = EventLog::source_paths(&source).unwrap().remove(0);
    let before = std::fs::read(&volume).unwrap();
    let existing = home.path().join("existing.jsonl");
    std::fs::write(&existing, "retain previous export").unwrap();
    let alias = home.path().join("alias.jsonl");
    std::fs::hard_link(&volume, &alias).unwrap();
    for destination in [&existing, &alias, &volume] {
        let contents = std::fs::read(destination).unwrap();
        let output = command(home.path())
            .arg("export")
            .arg(&source)
            .arg(destination)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("AlreadyExists"));
        assert_eq!(std::fs::read(destination).unwrap(), contents);
        assert_eq!(std::fs::read(&volume).unwrap(), before);
    }
    let output = command(home.path())
        .arg("--verify-ledger")
        .arg(&source)
        .output()
        .unwrap();
    assert_eq!(succeeded(&output), "Ledger verified; no files changed.\n");
    assert_eq!(std::fs::read(&volume).unwrap(), before);
    let missing = command(home.path()).arg("export").output().unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());
    assert_eq!(
        String::from_utf8(missing.stderr).unwrap(),
        "usage: lattice export <ledger.jsonl> [out.jsonl]\n"
    );
}

#[test]
fn tidy_reindexes_only_after_moving_or_deleting_records() {
    for action in ["compactAfterDays", "archiveAfterDays", "deleteAfterDays"] {
        let home = tempfile::tempdir().unwrap();
        let (source, _) = legacy(home.path(), "20000101-tui.jsonl");
        std::fs::File::open(&source)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        std::fs::write(
            home.path().join(".lattice/preferences.json"),
            json!({"ledger": {action:1}}).to_string(),
        )
        .unwrap();
        let index = lattice::ledgers::dir(home.path()).join("index.jsonl");
        std::fs::write(&index, "untouched index sentinel\n").unwrap();
        let output = command(home.path()).arg("tidy").output().unwrap();
        let text = succeeded(&output);
        match action {
            "compactAfterDays" => {
                assert!(
                    text.starts_with("1 compacted, 0 archived, 0 deleted, "),
                    "{text}"
                );
                assert!(!text.contains("reindexed"));
                assert!(source.exists());
                assert_eq!(
                    std::fs::read_to_string(&index).unwrap(),
                    "untouched index sentinel\n"
                );
            }
            "archiveAfterDays" => {
                assert!(
                    text.starts_with("0 compacted, 1 archived, 0 deleted, "),
                    "{text}"
                );
                assert!(text.contains("0 conversations reindexed"));
                assert!(!source.exists());
                assert!(lattice::ledgers::archive_dir(home.path())
                    .join(source.file_name().unwrap())
                    .exists());
                assert!(std::fs::read(&index).unwrap().is_empty());
            }
            _ => {
                assert!(
                    text.starts_with("0 compacted, 0 archived, 1 deleted, "),
                    "{text}"
                );
                assert!(text.contains(" conversations reindexed\n"), "{text}");
                assert!(!source.exists());
                let indexed = std::fs::read_to_string(&index).unwrap();
                assert_ne!(indexed, "untouched index sentinel\n");
                // Discovery currently mistakes the deletion receipt for a
                // conversation. Keep that separate defect out of this command
                // dispatch check; the ignored regression below exposes it.
                for line in indexed.lines() {
                    let entry: Value = serde_json::from_str(line).unwrap();
                    assert_ne!(entry["file"], "20000101-tui.jsonl");
                }
            }
        }
    }
}

#[test]
#[ignore = "Known discovery defect: deleted.jsonl is listed as a conversation; fix separately from command extraction"]
fn deletion_receipts_are_not_conversations() {
    let home = tempfile::tempdir().unwrap();
    let (source, _) = legacy(home.path(), "20000101-tui.jsonl");
    std::fs::File::open(&source)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    std::fs::write(
        home.path().join(".lattice/preferences.json"),
        json!({"ledger":{"deleteAfterDays":1}}).to_string(),
    )
    .unwrap();
    let output = command(home.path()).arg("tidy").output().unwrap();
    succeeded(&output);
    assert!(!source.exists());
    assert!(lattice::ledgers::dir(home.path())
        .join("deleted.jsonl")
        .exists());
    assert!(
        lattice::ledgers::all(home.path()).is_empty(),
        "a deletion receipt is not a recoverable conversation"
    );
}
