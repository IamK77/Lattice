use super::*;
use crate::{EventDraft, EventLog};
use serde_json::{json, Value};

fn declarations() -> Vec<crate::EventTypeDecl> {
    // Imported histories can contain older, schema-less payloads. Exercise the
    // same permissive display behavior as ingest, not today's admission rules.
    ce::core_event_decls()
        .into_iter()
        .map(|mut declaration| {
            declaration.schema = None;
            declaration
        })
        .collect()
}

fn append(log: &mut EventLog, kind: &str, payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, &[], payload), "fixture")
        .unwrap()
}

fn start(call: Value, tool: &str) -> Value {
    json!({"call":call,"tool":tool,"arguments":{"command":"fixture"}})
}

fn wake(call: &str, body: Value) -> Value {
    let mut body = body;
    body["job"] = json!(call);
    json!({"source":format!("background:{call}"),"summary":"receipt","body":body})
}

#[test]
fn card_recipes_match_full_ingest_at_every_saved_prefix_and_across_pages() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("cards.ledger");
    let mut log = EventLog::open_segmented(declarations(), "cards", root, 4096).unwrap();
    let mut history = History::recover(log.reader(), 0).unwrap();
    let mut full = Vec::new();
    let first = append(&mut log, ce::TOOL_EXEC_STARTED, start(json!("old"), "Run"));
    assert_eq!(
        history.catch_up(first.seq).unwrap(),
        ingest(&mut full, &first)
    );
    for index in 0..140 {
        let event = append(
            &mut log,
            ce::USER_MESSAGE,
            json!({"text":format!("message {index}")}),
        );
        ingest(&mut full, &event);
        history.catch_up(event.seq).unwrap();
    }
    let late = json!({"call":"old","status":"ok","result":{
        "background":true,"changed":"must-not-appear",
        "editDiff":{"hunks":[],"truncated":false}
    }});
    let events = vec![
        (ce::WAKE, wake("old", json!({"error":"unknown one"}))),
        (ce::WAKE, wake("old", json!({"error":"unknown two"}))),
        (ce::TOOL_EXEC_COMPLETED, late),
        (
            ce::WAKE,
            wake("old", json!({"exit_code":0,"stdout":"first"})),
        ),
        (
            ce::WAKE,
            wake(
                "old",
                json!({"exit_code":7,"stdout":"must-not-replace-first"}),
            ),
        ),
        (ce::TOOL_EXEC_STARTED, start(json!("old"), "Run")),
        (ce::TOOL_EXEC_STARTED, start(json!("old"), "Other")),
        (
            ce::TOOL_EXEC_COMPLETED,
            json!({"call":"old","status":"ok","result":{"background":true,"changed":"kept","editDiff":{"hunks":[],"truncated":false}}}),
        ),
        (ce::TOOL_EXEC_STARTED, start(json!("old"), "Other")),
        (ce::WAKE, wake("old", json!({"interrupted":"restart"}))),
        (
            ce::WAKE,
            wake("old", json!({"exit_code":0,"stdout":"later"})),
        ),
        (ce::TOOL_EXEC_STARTED, start(Value::Null, "Anonymous")),
        (
            ce::TOOL_EXEC_COMPLETED,
            json!({"call":"old","status":"ok","result":"anonymous wins before exact"}),
        ),
        (
            ce::TOOL_EXEC_COMPLETED,
            json!({"call":"old","status":"ok","result":"exact then settles"}),
        ),
        (ce::TOOL_EXEC_STARTED, start(Value::Null, "Run")),
        (ce::TOOL_EXEC_STARTED, start(Value::Null, "Run")),
        (ce::TOOL_EXEC_COMPLETED, json!({"status":"cancelled"})),
        (ce::WAKE, wake("unmatched", json!({"exit_code":1}))),
        (ce::TOOL_EXEC_STARTED, start(json!("unmatched"), "Run")),
        (
            ce::USER_MESSAGE,
            json!({"text":"images","images":[{"name":"one"},{"file":"two"}]}),
        ),
        (
            ce::MODEL_CALL_COMPLETED,
            json!({"reasoning":[{"kind":"text","text":"thought"}],"text":"between tools","toolCalls":[{"id":"next"}]}),
        ),
        (ce::OUTPUT_REPLY, json!({"text":"reply"})),
    ];
    for (kind, payload) in events {
        let event = append(&mut log, kind, payload);
        let expected_handled = ingest(&mut full, &event);
        assert_eq!(
            history.catch_up(event.seq).unwrap(),
            expected_handled,
            "{}",
            event.id
        );
        assert_eq!(
            history.load(0..history.len()).unwrap(),
            full,
            "{}",
            event.id
        );
        history.save().unwrap();
        history = History::recover(log.reader(), event.seq).unwrap();
        assert!(history.cold_reason().is_none());
        assert_eq!(
            history.load(0..history.len()).unwrap(),
            full,
            "restored {}",
            event.id
        );
    }
    let Entry::Tool(first) = &full[0] else {
        panic!("first command missing")
    };
    assert_eq!(first.status, ToolStatus::Ok);
    assert!(first.changed.is_none() && first.edit_diff.is_none());
    assert!(first.output.iter().any(|line| line.contains("first")));
    assert!(!first.output.iter().any(|line| line.contains("must-not")));
}

#[test]
fn warm_card_recovery_does_not_load_old_recipe_pages_and_reads_only_selected_cards() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("warm.ledger");
    let mut log = EventLog::open_segmented(declarations(), "cards", root.clone(), 4096).unwrap();
    for index in 0..1200 {
        append(
            &mut log,
            ce::OUTPUT_REPLY,
            json!({"text":format!("reply {index}")}),
        );
    }
    let end = log.reader().snapshot_end();
    let cold = History::recover(log.reader(), end).unwrap();
    assert!(cold.cold_reason().is_some());
    drop(cold);
    drop(log);
    let log = EventLog::open_segmented(declarations(), "cards", root, 4096).unwrap();
    let warm = History::recover(log.reader(), end).unwrap();
    assert!(warm.cold_reason().is_none());
    assert_eq!(warm.pages.read_count(), 0);
    assert_eq!(warm.get(1199).unwrap(), Entry::Agent("reply 1199".into()));
    assert_eq!(warm.pages.read_count(), 1);
    assert_eq!(warm.get(1198).unwrap(), Entry::Agent("reply 1198".into()));
    assert_eq!(warm.pages.read_count(), 1);
    assert_eq!(warm.group(1199).unwrap(), 1199..1200);
}

#[test]
fn invalid_recipe_sources_and_updates_are_errors_not_missing_cards() {
    let mut log = EventLog::in_memory(declarations(), "cards");
    let source = append(&mut log, ce::TOOL_EXEC_STARTED, start(json!("one"), "Run"));
    let other = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        json!({"call":"other","status":"ok"}),
    );
    let mut entries = Vec::new();
    ingest(&mut entries, &source);
    let mut record = Record::new(&source.id, 0, &entries[0]);
    let get = |id: &str| {
        log.reader()
            .get(id)?
            .ok_or_else(|| invalid("absent source"))
    };
    record.completion = Some(other.id);
    assert!(record
        .materialize(get)
        .unwrap_err()
        .to_string()
        .contains("does not address"));
    record.completion = None;
    record.ordinal = 1;
    assert!(record
        .materialize(get)
        .unwrap_err()
        .to_string()
        .contains("ordinal"));
    record.ordinal = 0;
    record.source = "absent".into();
    assert!(record
        .materialize(get)
        .unwrap_err()
        .to_string()
        .contains("absent"));
}
