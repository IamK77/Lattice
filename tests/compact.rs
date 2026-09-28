//! Rewriting an existing ledger into today's shape.
//!
//! The same pass does two jobs: it compacts a record that has grown, and it
//! migrates one written before documents moved out. What it may never do is
//! change the record — an event's id names its own line, so a pass that added,
//! dropped or reordered a line would break every address on the ledger.

use serde_json::{json, Value};

use lattice::core_events as ce;
use lattice::{compact, EventDraft, EventLog};

/// A ledger with no file to write beside it cannot move anything out, so this
/// builds one on disk the plain way: append, then take the file as it stands.
fn ledger_with(prompt: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.jsonl");
    let mut log = EventLog::open(
        ce::core_event_decls(),
        "main".to_string(),
        Some(path.clone()),
    )
    .unwrap();
    let said = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hello"})),
            "ui",
        )
        .unwrap();
    log.append(
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[&said.id],
            json!({
                "model": "m",
                "system": prompt,
                "input": {"parts": [{"event": said.id}], "fingerprint": "sha256:x"},
            }),
        ),
        "gate",
    )
    .unwrap();
    (dir, path)
}

#[test]
fn segmented_export_inlines_documents_but_compaction_cannot_rewrite_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "main", path.clone(), 1).unwrap();
    let text = "a long original message\n".repeat(1000);
    for _ in 0..2 {
        log.append(
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[],
                json!({
                    "model":"m", "system":text, "input":{"parts":[],"fingerprint":"sha256:x"}
                }),
            ),
            "ui",
        )
        .unwrap();
    }
    drop(log);
    let files = EventLog::source_paths(&path).unwrap();
    let originals: Vec<_> = files
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let mut output = Vec::new();
    let report = lattice::export(&path, &mut output, &ce::core_event_decls()).unwrap();
    assert_eq!(report.events, 2);
    assert_eq!(report.inlined, 2);
    assert!(report.missing.is_empty());
    for line in String::from_utf8(output).unwrap().lines() {
        assert_eq!(
            serde_json::from_str::<Value>(line).unwrap()["payload"]["system"],
            text
        );
    }
    assert!(compact(&path, &ce::core_event_decls()).is_err());
    assert!(!dir.path().join("main.ledger.compacting").exists());
    for (file, original) in files.iter().zip(originals) {
        assert_eq!(std::fs::read(file).unwrap(), original);
    }
}

/// Read a ledger back as lines of JSON.
fn lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The migration case: a record written before documents moved out.
///
/// Written by hand rather than through the log, because the log would move it
/// out on the way in — which is exactly the difference between a record made
/// today and one made last week.
#[test]
fn an_old_record_is_migrated_without_a_single_line_moving() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.jsonl");
    let prompt: String = (1..=400)
        .map(|n| format!("Rule {n}: say what you checked.\n"))
        .collect();
    let old = format!(
        concat!(
            r#"{{"v":1,"id":"ev_1_aa","seq":1,"stream":"main","time":"t","#,
            r#""type":"core.input.user_message","source":"ui","causes":[],"#,
            r#""payload":{{"text":"hello"}}}}"#,
            "\n",
            r#"{{"v":1,"id":"ev_2_bb","seq":2,"stream":"main","time":"t","#,
            r#""type":"core.model.call_started","source":"gate","causes":["ev_1_aa"],"#,
            r#""payload":{{"model":"m","system":{},"input":{{"parts":[],"fingerprint":"x"}}}}}}"#,
            "\n"
        ),
        serde_json::to_string(&prompt).unwrap()
    );
    std::fs::write(&path, &old).unwrap();

    let report = compact(&path, &ce::core_event_decls()).unwrap();
    assert_eq!(report.events, 2);
    assert_eq!(report.rewritten, 1, "only the event holding a document");
    assert_eq!(report.documents, 1);
    assert!(report.bytes_after < report.bytes_before / 2);

    let after = lines(&path);
    assert_eq!(after.len(), 2, "no line was added or dropped");
    assert_eq!(after[0]["id"], "ev_1_aa", "and none moved");
    assert_eq!(after[1]["id"], "ev_2_bb");
    assert_eq!(
        after[1]["payload"]["system"]["file"], "ev_2-system.txt",
        "named for the event it came from: {}",
        after[1]["payload"]["system"]
    );

    let written = std::fs::read_to_string(dir.path().join("main").join("ev_2-system.txt")).unwrap();
    assert_eq!(written, prompt, "byte for byte");
    assert_eq!(written.lines().count(), 400, "and with its lines back");

    // Twice costs a read and changes nothing
    let again = compact(&path, &ce::core_event_decls()).unwrap();
    assert_eq!(again.rewritten, 0, "already compacted");
    assert_eq!(lines(&path), after, "and the ledger is untouched");
}

/// A ledger written today is already in this shape, so a pass finds nothing.
#[test]
fn a_ledger_written_today_has_nothing_left_to_compact() {
    let prompt: String = (1..=400)
        .map(|n| format!("Rule {n}: say what you checked.\n"))
        .collect();
    let (_dir, path) = ledger_with(&prompt);
    let report = compact(&path, &ce::core_event_decls()).unwrap();
    assert_eq!(report.events, 2);
    assert_eq!(report.rewritten, 0);
}

/// A short prompt stays where a reader expects it. A document costs a file and
/// a jump to read, and neither is worth paying for a few hundred bytes.
#[test]
fn something_small_is_left_alone() {
    let (_dir, path) = ledger_with("be helpful");
    let report = compact(&path, &ce::core_event_decls()).unwrap();
    assert_eq!(report.documents, 0);
    let after = lines(&path);
    assert_eq!(after[1]["payload"]["system"], "be helpful");
}

/// A line this pass cannot parse is copied through untouched. The ledger is
/// the audit record: not understanding an event is never a reason to drop it.
#[test]
fn a_line_that_cannot_be_read_is_carried_through_rather_than_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.jsonl");
    let prompt: String = (1..=400)
        .map(|n| format!("Rule {n}: say what you checked.\n"))
        .collect();
    std::fs::write(
        &path,
        format!(
            "{{not json at all\n{}\n",
            json!({
                "v": 1, "id": "ev_2_bb", "seq": 2, "stream": "main", "time": "t",
                "type": ce::MODEL_CALL_STARTED, "source": "gate", "causes": [],
                "payload": {"model": "m", "system": prompt,
                            "input": {"parts": [], "fingerprint": "x"}},
            })
        ),
    )
    .unwrap();

    let report = compact(&path, &ce::core_event_decls()).unwrap();
    assert_eq!(report.events, 2);
    assert_eq!(report.rewritten, 1);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("{not json at all\n"),
        "the unreadable line is still there, first: {}",
        &text[..40.min(text.len())]
    );
}

/// An event type nobody declared documents for is left as it is — the pass
/// judges by what the type says, never by size alone.
#[test]
fn a_field_no_type_calls_a_document_stays_inline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.jsonl");
    let big: String = (1..=400)
        .map(|n| format!("line {n}: long enough to be worth a file of its own\n"))
        .collect();
    assert!(big.len() > 4096, "the fixture must exceed the threshold");
    let mut log = EventLog::open(
        ce::core_event_decls(),
        "main".to_string(),
        Some(path.clone()),
    )
    .unwrap();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": big.clone()})),
        "ui",
    )
    .unwrap();
    drop(log);

    let report = compact(&path, &ce::core_event_decls()).unwrap();
    assert_eq!(
        report.documents, 0,
        "`text` on a user message is not declared"
    );
    assert_eq!(lines(&path)[0]["payload"]["text"], big);
}

/// Compacting and exporting are inverses: what goes out comes back byte for
/// byte. That is what makes moving documents out safe to do at all — the
/// conversation can always be handed over as one file again.
#[test]
fn exporting_folds_the_documents_back_in_and_the_record_survives_the_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.jsonl");
    let prompt: String = (1..=400)
        .map(|n| format!("Rule {n}: say what you checked.\n"))
        .collect();
    let mut log = EventLog::open(
        ce::core_event_decls(),
        "main".to_string(),
        Some(path.clone()),
    )
    .unwrap();
    let said = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hello"})),
            "ui",
        )
        .unwrap();
    log.append(
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[&said.id],
            json!({
                "model": "m",
                "system": prompt,
                "tools": [{"name": "Read", "description": "x".repeat(5000)}],
                "input": {"parts": [{"event": said.id}], "fingerprint": "sha256:x"},
            }),
        ),
        "gate",
    )
    .unwrap();
    drop(log);

    // On the ledger both are references
    let stored = lines(&path);
    assert!(stored[1]["payload"]["system"]["file"].is_string());
    assert!(stored[1]["payload"]["tools"]["file"].is_string());

    let mut out = Vec::new();
    let report = lattice::export(&path, &mut out, &ce::core_event_decls()).unwrap();
    assert_eq!(report.events, 2);
    assert_eq!(report.inlined, 2, "the prompt and the tool list");
    assert!(report.missing.is_empty());

    let restored: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        restored[1]["payload"]["system"],
        json!(prompt),
        "byte for byte"
    );
    assert_eq!(
        restored[1]["payload"]["tools"][0]["name"], "Read",
        "and a field that was an array is an array again"
    );
    // Everything else is exactly as it was
    assert_eq!(restored[0], stored[0]);
    assert_eq!(restored[1]["id"], stored[1]["id"]);
    assert_eq!(
        restored[1]["payload"]["input"],
        stored[1]["payload"]["input"]
    );
}

/// A document that is not there is named, never silently dropped. An export
/// that quietly loses content is worse than one that says what is missing.
#[test]
fn a_document_that_cannot_be_read_is_reported_rather_than_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.jsonl");
    let prompt: String = (1..=400)
        .map(|n| format!("Rule {n}: say what you checked.\n"))
        .collect();
    let (_keep, made) = ledger_with(&prompt);
    std::fs::copy(&made, &path).unwrap();
    // The ledger arrives without its directory — the exact hazard of a
    // conversation being a folder rather than a file.

    let mut out = Vec::new();
    let report = lattice::export(&path, &mut out, &ce::core_event_decls()).unwrap();
    assert_eq!(report.inlined, 0);
    assert_eq!(report.missing, vec!["ev_2-system.txt".to_string()]);
    // And the reference is carried through, so nothing is invented either
    let restored: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(restored[1]["payload"]["system"]["file"], "ev_2-system.txt");
}
