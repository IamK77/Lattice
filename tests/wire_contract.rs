//! The cross-language wire contract, anchored to a golden file.
//!
//! The daemon protocol has consumers in other languages (clients/ink). Each
//! side previously verified itself against its own idea of the format — the
//! blind spot this file closes is DRIFT: a renamed field keeps both suites
//! green while the real integration breaks. The golden file
//! `clients/ink/test/golden/wire.jsonl` is the single reference: this test
//! pins the Rust side to it, `clients/ink/test/golden.test.js` pins the Node
//! side to the SAME file. Either side drifting reddens its suite.
//!
//! Regenerate deliberately (then review the diff — the golden is contract):
//!   cargo test --test wire_contract rewrite_golden -- --ignored

use lattice::daemon::{ClientMessage, ServerMessage};
use lattice::{EventEnvelope, StreamRef};
use serde_json::{json, Value};

fn golden_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("clients/ink/test/golden/wire.jsonl")
}

fn full_envelope() -> EventEnvelope {
    // Every field populated, so optional-field handling is part of the pin
    EventEnvelope {
        v: 1,
        id: "ev_7_a1b2c3d4".to_string(),
        seq: 7,
        stream: "main".to_string(),
        time: "2026-07-21T00:00:00Z".to_string(),
        event_type: "core.input.user_message".to_string(),
        source: "ui".to_string(),
        causes: vec!["ev_5_cafe0001".to_string(), "ev_6_cafe0002".to_string()],
        origin: Some(StreamRef {
            stream: "parent".to_string(),
            event: "ev_3_beef0003".to_string(),
        }),
        reason: Some("a decision needs a reason".to_string()),
        payload: json!({"text": "hello wire"}),
    }
}

/// Every message shape that crosses the socket, labelled.
fn samples() -> Vec<(&'static str, Value)> {
    let client = |m: &ClientMessage| serde_json::to_value(m).unwrap();
    let server = |m: &ServerMessage| serde_json::to_value(m).unwrap();
    let cursor = lattice::daemon::protocol::HistoryCursor {
        stream: "main".into(),
        generation: 1,
        through: 200,
        before: 73,
    };
    vec![
        (
            "history",
            client(&ClientMessage::History {
                stream: "main".into(),
                cursor: cursor.clone(),
            }),
        ),
        (
            "history-page",
            server(&ServerMessage::HistoryPage {
                stream: "main".into(),
                cursor: cursor.clone(),
                replay: vec![full_envelope()],
                older: None,
            }),
        ),
        (
            "history-error",
            server(&ServerMessage::HistoryError {
                stream: "main".into(),
                cursor: cursor.clone(),
                message: "stale cursor".into(),
            }),
        ),
        (
            "attached-paged",
            server(&ServerMessage::Attached {
                stream: "main".into(),
                replay: vec![full_envelope()],
                warnings: vec![],
                history: Some(lattice::daemon::protocol::AttachmentHistory {
                    through: 200,
                    older: Some(cursor),
                    state: Default::default(),
                }),
            }),
        ),
        (
            "attach-basic",
            client(&ClientMessage::Attach {
                stream: "main".into(),
                template: None,
                derive_from: None,
                capabilities: vec![],
            }),
        ),
        (
            "attach-derived",
            client(&ClientMessage::Attach {
                stream: "btw-1".into(),
                template: Some("chat".into()),
                derive_from: Some("main".into()),
                capabilities: vec![],
            }),
        ),
        (
            "attach-with-capabilities",
            client(&ClientMessage::Attach {
                stream: "main".into(),
                template: None,
                derive_from: None,
                capabilities: vec!["authorize".into()],
            }),
        ),
        (
            "send-text",
            client(&ClientMessage::SendText {
                stream: "main".into(),
                text: "add 4 and 7".into(),
            }),
        ),
        (
            "authorize",
            client(&ClientMessage::Authorize {
                stream: "main".into(),
                request: "ev_9_feed0004".into(),
                approve: true,
            }),
        ),
        (
            "detach",
            client(&ClientMessage::Detach {
                stream: "main".into(),
            }),
        ),
        (
            "interrupt",
            client(&ClientMessage::Interrupt {
                stream: "main".into(),
            }),
        ),
        (
            "attached-with-replay",
            server(&ServerMessage::Attached {
                history: None,
                stream: "main".into(),
                replay: vec![full_envelope()],
                warnings: vec![],
            }),
        ),
        (
            "attached-with-warnings",
            server(&ServerMessage::Attached {
                history: None,
                stream: "main".into(),
                replay: vec![],
                warnings: vec!["this client declared no \"authorize\" capability".into()],
            }),
        ),
        (
            "appended",
            server(&ServerMessage::Appended {
                stream: "main".into(),
                event: Box::new(full_envelope()),
            }),
        ),
        (
            "notice",
            server(&ServerMessage::Notice {
                stream: "main".into(),
                source: "model".into(),
                payload: json!({"chunk": "strea"}),
            }),
        ),
        (
            "quiescent",
            server(&ServerMessage::Quiescent {
                stream: "main".into(),
            }),
        ),
        (
            "error-with-stream",
            server(&ServerMessage::Error {
                stream: Some("main".into()),
                message: "unknown stream - attach first".into(),
            }),
        ),
        (
            "error-connection-level",
            server(&ServerMessage::Error {
                stream: None,
                message: "unparseable client message".into(),
            }),
        ),
    ]
}

fn golden_lines() -> Vec<(String, Value)> {
    let text = std::fs::read_to_string(golden_path())
        .expect("golden file missing — run the rewrite_golden test and review the diff");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("golden line is not JSON");
            (
                v["label"]
                    .as_str()
                    .expect("golden line has a label")
                    .to_string(),
                v["message"].clone(),
            )
        })
        .collect()
}

#[test]
fn the_wire_format_matches_the_golden_file() {
    let golden: std::collections::HashMap<String, Value> = golden_lines().into_iter().collect();
    let samples = samples();
    assert_eq!(
        samples.len(),
        golden.len(),
        "sample set and golden file disagree on message count — regenerate deliberately"
    );
    for (label, actual) in samples {
        let expected = golden
            .get(label)
            .unwrap_or_else(|| panic!("golden file has no entry for {label}"));
        assert_eq!(
            &actual, expected,
            "wire drift on {label}: Rust now serializes differently from the pinned contract"
        );
    }
}

#[test]
fn every_golden_message_deserializes_back() {
    // The reverse direction: whatever is pinned must still PARSE — a
    // removed field or variant breaks old clients even if serialization
    // "looks" fine
    for (label, message) in golden_lines() {
        let ok = serde_json::from_value::<ClientMessage>(message.clone()).is_ok()
            || serde_json::from_value::<ServerMessage>(message.clone()).is_ok();
        assert!(
            ok,
            "golden message {label} no longer parses on the Rust side"
        );
    }
}

#[test]
#[ignore = "writes the golden file; run deliberately and review the diff"]
fn rewrite_golden() {
    let mut out = String::new();
    for (label, message) in samples() {
        out.push_str(&serde_json::to_string(&json!({"label": label, "message": message})).unwrap());
        out.push('\n');
    }
    std::fs::create_dir_all(golden_path().parent().unwrap()).unwrap();
    std::fs::write(golden_path(), out).unwrap();
}
