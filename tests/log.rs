//! Behavior-contract tests for the event log (ported from the TypeScript skeleton, same coverage).

use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::core_events as ce;
use lattice::{AuditViolation, EventDraft, EventLog, EventTypeDecl};

fn test_types() -> Vec<EventTypeDecl> {
    let mut types = ce::core_event_decls();
    types.push(
        EventTypeDecl::decision("sched.task.split", "decision-class event for tests")
            // Conversation-class for these tests: a custom type can opt in,
            // which is the whole point of the flag living on the declaration.
            .redacting(),
    );
    types
}

fn new_log() -> EventLog {
    EventLog::in_memory(test_types(), "main")
}

// ── Sequence numbers & envelope completion ─────────────

#[test]
fn append_assigns_seq_unique_id_and_time() {
    let mut log = new_log();
    let a = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "你好"})),
            "界面",
        )
        .unwrap();
    let b = log
        .append(
            EventDraft::new(ce::TURN_STARTED, &[&a.id], json!({})),
            "主循环",
        )
        .unwrap();
    assert_eq!(a.seq, 1);
    assert_eq!(b.seq, 2);
    assert_ne!(a.id, b.id);
    assert!(
        a.time.starts_with("20"),
        "time should be ISO 8601: {}",
        a.time
    );
    assert_eq!(a.stream, "main", "the log stamps its own stream");
    assert_eq!(b.source, "主循环");
}

// ── Entry-point enforcement ─────────────────────────────

#[test]
fn rejects_unregistered_event_type() {
    let mut log = new_log();
    let err = log
        .append(EventDraft::new("core.model.没这种事", &[], json!({})), "x")
        .unwrap_err();
    assert!(matches!(err, AuditViolation::UnregisteredType(_)));
}

#[test]
fn rejects_cause_pointing_to_nonexistent_event() {
    let mut log = new_log();
    let err = log
        .append(
            EventDraft::new(ce::TURN_STARTED, &["ev_不存在"], json!({})),
            "主循环",
        )
        .unwrap_err();
    assert!(
        matches!(err, AuditViolation::UnknownCause(_)),
        "the chain can never break"
    );
}

#[test]
fn decision_event_requires_reason() {
    let mut log = new_log();
    let err = log
        .append(
            EventDraft::new("sched.task.split", &[], json!({})),
            "调度器",
        )
        .unwrap_err();
    assert!(matches!(err, AuditViolation::MissingReason(_)));

    let ok = log
        .append(
            EventDraft::new("sched.task.split", &[], json!({}))
                .with_reason("任务过大，拆成三份并行"),
            "调度器",
        )
        .unwrap();
    assert!(ok.reason.unwrap().contains("拆成三份"));
}

// ── Replay & subscription ───────────────────────────────

#[test]
fn replay_returns_all_in_order_and_supports_from_seq() {
    let mut log = new_log();
    let a = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "1"})),
            "界面",
        )
        .unwrap();
    log.append(
        EventDraft::new(ce::TURN_STARTED, &[&a.id], json!({})),
        "主循环",
    )
    .unwrap();
    log.append(
        EventDraft::new(ce::TURN_COMPLETED, &[&a.id], json!({})),
        "主循环",
    )
    .unwrap();

    let seqs: Vec<u64> = log.replay(1).unwrap().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    let tail = log.replay(3).unwrap();
    let types: Vec<&str> = tail.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(types, vec![ce::TURN_COMPLETED]);
}

#[test]
fn subscriber_receives_events_until_unsubscribed() {
    let mut log = new_log();
    let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_in_handler = Arc::clone(&seen);
    let subscription = log.subscribe(move |e| seen_in_handler.lock().unwrap().push(e.seq));

    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "a"})),
        "界面",
    )
    .unwrap();
    log.unsubscribe(subscription);
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "b"})),
        "界面",
    )
    .unwrap();

    assert_eq!(*seen.lock().unwrap(), vec![1]);
}

// ── JSONL persistence ───────────────────────────────────

#[test]
fn persists_and_reopens_intact() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.jsonl");

    let mut log = EventLog::open(test_types(), "main", Some(file.clone())).unwrap();
    let a = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "存我"})),
            "界面",
        )
        .unwrap();
    log.append(
        EventDraft::new(ce::TURN_STARTED, &[&a.id], json!({})),
        "主循环",
    )
    .unwrap();

    let mut reopened = EventLog::open(test_types(), "main", Some(file)).unwrap();
    assert_eq!(reopened.len(), 2);
    assert_eq!(
        reopened.replay(1).unwrap()[0].payload,
        json!({"text": "存我"})
    );
    // Keep appending after reopen: seq continues, causes still validated
    let c = reopened
        .append(
            EventDraft::new(ce::TURN_COMPLETED, &[&a.id], json!({})),
            "主循环",
        )
        .unwrap();
    assert_eq!(c.seq, 3);
}

/// A write that never finished costs its own event, not the conversation.
///
/// Every event is fsynced, which is precisely why a process killed at the
/// wrong instant leaves half a line at the end of the file — it is the
/// expected way to die, not an exotic one. Refusing the whole ledger over it
/// meant that conversation could never be opened again by anything, with no
/// repair path and nothing said about why. The half line is dropped, the file
/// is cut back to the last good event, and the next append lands cleanly
/// instead of being glued to the wreckage.
#[test]
fn a_ledger_cut_off_mid_write_opens_without_its_last_half_event() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.jsonl");

    let mut log = EventLog::open(test_types(), "main", Some(file.clone())).unwrap();
    let a = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "whole"})),
            "ui",
        )
        .unwrap();
    drop(log);

    // The power goes out halfway through writing the second event.
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("{\"v\":1,\"id\":\"ev_2\",\"seq\":2,\"stream\":\"ma");
    std::fs::write(&file, &text).unwrap();

    let mut reopened = EventLog::open(test_types(), "main", Some(file.clone())).unwrap();
    assert_eq!(reopened.len(), 1, "the finished event survives");
    assert_eq!(
        reopened.replay(1).unwrap()[0].payload,
        json!({"text": "whole"})
    );

    // And the file is usable again — not merely readable this once.
    let next = reopened
        .append(
            EventDraft::new(ce::TURN_COMPLETED, &[&a.id], json!({})),
            "loop",
        )
        .unwrap();
    assert_eq!(next.seq, 2);
    let third = EventLog::open(test_types(), "main", Some(file)).unwrap();
    assert_eq!(third.len(), 2, "the repair held across another open");
}

/// Damage anywhere else is not a torn write, and guessing about it would be
/// worse than refusing.
#[test]
fn a_broken_line_in_the_middle_is_still_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.jsonl");

    let mut log = EventLog::open(test_types(), "main", Some(file.clone())).unwrap();
    let a = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "one"})),
            "ui",
        )
        .unwrap();
    log.append(
        EventDraft::new(ce::TURN_STARTED, &[&a.id], json!({})),
        "loop",
    )
    .unwrap();
    drop(log);

    let text = std::fs::read_to_string(&file).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines[0] = "{not json at all";
    std::fs::write(&file, lines.join("\n") + "\n").unwrap();

    assert!(
        EventLog::open(test_types(), "main", Some(file)).is_err(),
        "a ledger damaged in the middle must refuse to open"
    );
}

// ── Audit scenario: find slowest test, pytest missing, recover ──

#[test]
fn trace_back_surfaces_root_cause_mechanically() {
    let mut log = new_log();
    let user_input = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "看看哪个测试最慢"})),
            "界面",
        )
        .unwrap();
    let ask_model = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[&user_input.id],
                json!({"model": "m", "input": {"parts": [], "fingerprint": "sha256:0"}}),
            ),
            "主循环",
        )
        .unwrap();
    let model_reply = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED, &[&ask_model.id],
                json!({"status": "ok", "toolCalls": [{"tool": "shell", "arguments": {"cmd": "pytest --durations=10"}}]}),
            ),
            "模型适配",
        )
        .unwrap();
    let exec = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&model_reply.id],
                json!({"tool": "shell", "arguments": {}}),
            ),
            "主循环",
        )
        .unwrap();
    let failure = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&exec.id],
                json!({"status": "error", "error": {"code": "tool.exec_failed", "message": "pytest: command not found (exit 127)", "blame": "environment"}}),
            ),
            "工具执行器",
        )
        .unwrap();
    let ask_again = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[&failure.id],
                json!({"model": "m", "input": {"parts": [], "fingerprint": "sha256:0"}}),
            ),
            "主循环",
        )
        .unwrap();
    let reply_again = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[&ask_again.id],
                json!({"status": "ok", "text": "改用 uv run pytest"}),
            ),
            "模型适配",
        )
        .unwrap();
    let output = log
        .append(
            EventDraft::new(ce::TURN_COMPLETED, &[&reply_again.id], json!({})),
            "主循环",
        )
        .unwrap();

    let chain = log.trace_back(&output.id).unwrap();
    let seqs: Vec<u64> = chain.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(chain.last().unwrap().event_type, ce::USER_MESSAGE);

    // The only failure event on the chain is the root cause — no human narration needed
    let root_cause = chain
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["status"] == "error")
        .expect("the chain should contain a failed tool execution");
    assert!(root_cause.payload["error"]["message"]
        .as_str()
        .unwrap()
        .contains("exit 127"));
}

// ── Streams (the top-level container) ───────────────────

#[test]
fn origin_is_a_weak_unvalidated_cross_stream_reference() {
    let mut log = new_log();
    let root = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "spawned work"}))
                .with_origin("st_elsewhere", "ev_never_checked"),
            "界面",
        )
        .unwrap();
    // The kernel does not look inside other streams: the reference is
    // recorded as-is, the audit hop is followable but unverified
    let origin = root.origin.expect("origin recorded");
    assert_eq!(origin.stream, "st_elsewhere");
    assert_eq!(origin.event, "ev_never_checked");
}

#[test]
fn reopening_a_ledger_under_a_different_stream_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.jsonl");
    let mut log = EventLog::open(test_types(), "main", Some(file.clone())).unwrap();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hi"})),
        "界面",
    )
    .unwrap();

    assert!(EventLog::open(test_types(), "other", Some(file)).is_err());
}

// ── Multi-cause: the ancestry is a graph ────────────────

#[test]
fn trace_back_walks_the_ancestor_graph_without_duplicates() {
    let mut log = new_log();
    // Diamond: one root, two branches, one join
    let a = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "root"})),
            "界面",
        )
        .unwrap();
    let b = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&a.id],
                json!({"tool": "x", "arguments": {}}),
            ),
            "主循环",
        )
        .unwrap();
    let c = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&a.id],
                json!({"tool": "y", "arguments": {}}),
            ),
            "主循环",
        )
        .unwrap();
    let d = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[&b.id, &c.id],
                json!({"model": "m", "input": {"parts": [], "fingerprint": "sha256:0"}}),
            ),
            "主循环",
        )
        .unwrap();

    let ancestors = log.trace_back(&d.id).unwrap();
    let seqs: Vec<u64> = ancestors.iter().map(|e| e.seq).collect();
    // Each ancestor once, in topological (seq-descending) order — the shared
    // root is not visited twice
    assert_eq!(seqs, vec![4, 3, 2, 1]);
}

// ── Enforcement layer four: the letter itself ───────────

#[test]
fn malformed_payload_is_rejected_by_the_type_schema() {
    let mut log = new_log();
    // user_message requires a "text" string — an empty letter is a violation
    let err = log
        .append(EventDraft::new(ce::USER_MESSAGE, &[], json!({})), "界面")
        .unwrap_err();
    assert!(matches!(err, AuditViolation::InvalidPayload { .. }));

    // and a wrong-typed field is caught too
    let err = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": 42})),
            "界面",
        )
        .unwrap_err();
    assert!(matches!(err, AuditViolation::InvalidPayload { .. }));
}

#[test]
fn every_event_carries_the_envelope_version() {
    let mut log = new_log();
    let event = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hi"})),
            "界面",
        )
        .unwrap();
    assert_eq!(event.v, lattice::ENVELOPE_VERSION);
}

/// A secret told to the ledger is never written down.
///
/// The ledger is permanent and, worse for a secret, it is re-sent: every part
/// of it goes to the provider again on the next turn. So a key that lands on
/// it once is on it forever and leaves repeatedly. And the ways it lands are
/// ordinary — the agent reads the config file that holds it, runs `env`,
/// pastes an error quoting a URL — none of which anyone would think to
/// forbid.
#[test]
fn a_secret_never_reaches_the_ledger_however_it_arrives() {
    const KEY: &str = "test-only-redaction-token-not-a-real-credential";
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.jsonl");
    let mut log = EventLog::open(test_types(), "main", Some(file.clone())).unwrap();
    log.redact(lattice::Redactor::new([KEY.to_string()]));

    // The shapes it really arrives in, none of them a field called "key".
    let said = log
        .append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text": format!("here is my key {KEY}, use it")}),
            ),
            "ui",
        )
        .unwrap();
    log.append(
        EventDraft::new(
            ce::MODEL_CALL_COMPLETED,
            &[&said.id],
            // Buried: inside a nested structure, in a list, and as a map KEY.
            json!({
                "status": "ok",
                "text": format!("DEEPSEEK_API_KEY={KEY}\nPATH=/usr/bin"),
                "argv": ["curl", "-H", format!("Authorization: Bearer {KEY}")],
                "by_key": {KEY: "something indexed by it"},
            }),
        ),
        "model",
    )
    .unwrap();

    // Nowhere in memory…
    let whole = serde_json::to_string(&log.replay(1).unwrap()).unwrap();
    assert!(
        !whole.contains(KEY),
        "the secret is on the ledger: {}",
        &whole[..whole.len().min(400)]
    );
    assert!(
        whole.contains("[redacted]"),
        "and it was replaced, not dropped"
    );
    // …and nowhere on disk, because it was never written in the first place.
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(!written.contains(KEY), "the secret is in the file");
    // What surrounded it survives — this is redaction, not deletion.
    assert!(written.contains("PATH=/usr/bin"));
    assert!(written.contains("use it"));
}

/// A reason line is prose a person wrote, and prose is where a secret gets
/// quoted.
#[test]
fn a_secret_quoted_in_a_reason_is_redacted_too() {
    const KEY: &str = "sk-0123456789abcdef0123456789abcdef";
    let mut log = EventLog::in_memory(test_types(), "main");
    log.redact(lattice::Redactor::new([KEY.to_string()]));
    let said = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
            "ui",
        )
        .unwrap();
    let decided = log
        .append(
            EventDraft::new("sched.task.split", &[&said.id], json!({}))
                .with_reason(&format!("the user pasted {KEY} and asked for this")),
            "core",
        )
        .unwrap();
    let reason = decided.reason.unwrap();
    assert!(!reason.contains(KEY), "the reason still has it: {reason}");
    assert!(reason.contains("[redacted]"));
}

/// A short string would match everywhere and turn the ledger into
/// `[redacted]`; a real key is long.
#[test]
fn something_too_short_to_be_a_secret_is_not_treated_as_one() {
    let mut log = EventLog::in_memory(test_types(), "main");
    log.redact(lattice::Redactor::new(["a".to_string(), "go".to_string()]));
    let said = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go and about"})),
            "ui",
        )
        .unwrap();
    assert_eq!(said.payload["text"], "go and about");
}
