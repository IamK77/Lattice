use std::fs::{self, OpenOptions};
use std::io::Write;

use lattice::{EventDraft, EventLog, EventTypeDecl};
use serde_json::json;

#[test]
fn complete_incompatible_json_is_never_deleted_as_a_torn_tail() {
    for extension in ["jsonl", "ledger"] {
        for tail in [
            r#"{"v":2}"#,
            r#"{"v":2,"source":{"future":"structure"}}"#,
            r#"{"v":4294967296}"#,
            "null",
        ] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join(format!("fixture.{extension}"));
            let types = vec![EventTypeDecl::new("fixture.event", "tail")];
            let mut log = EventLog::open(types.clone(), "fixture", Some(path.clone())).unwrap();
            log.append(
                EventDraft::new("fixture.event", &[], json!({"text": "preserve"})),
                "fixture",
            )
            .unwrap();
            drop(log);
            let active = if path.is_dir() {
                path.join("00000000000000000000.jsonl")
            } else {
                path.clone()
            };
            OpenOptions::new()
                .append(true)
                .open(&active)
                .unwrap()
                .write_all(tail.as_bytes())
                .unwrap();
            let before = fs::read(&active).unwrap();
            let result = EventLog::open(types, "fixture", Some(path));
            assert!(
                result.is_err(),
                "complete incompatible {extension} tail must reject: {tail}"
            );
            assert_eq!(
                fs::read(active).unwrap(),
                before,
                "must preserve complete JSON"
            );
        }
    }
}
