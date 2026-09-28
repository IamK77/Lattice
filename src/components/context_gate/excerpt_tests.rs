use super::*;
use crate::EventLog;
use std::sync::atomic::Ordering;

fn log(path: Option<std::path::PathBuf>) -> EventLog {
    EventLog::open(
        [ce::USER_MESSAGE, ce::OUTPUT_REPLY, ce::TOOL_EXEC_COMPLETED]
            .into_iter()
            .map(|name| EventTypeDecl::new(name, "test"))
            .collect(),
        "parent",
        path,
    )
    .unwrap()
}

fn append(log: &mut EventLog, kind: &str, text: &str, causes: &[&str]) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, json!({"text": text})), "test")
        .unwrap()
}

fn excerpt(log: &EventLog) -> String {
    foreign_transcript_digest("parent", &log.reader())
        .unwrap()
        .unwrap()["digest"]["text"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn excerpts_identify_their_sources_and_keep_repeated_real_inputs_but_not_forwards() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parent.jsonl");
    let mut log = log(Some(path.clone()));
    let first = append(&mut log, ce::USER_MESSAGE, "same question", &[]);
    let forwarded = append(&mut log, ce::USER_MESSAGE, "expanded secret", &[&first.id]);
    let second = append(&mut log, ce::USER_MESSAGE, "same question", &[]);
    let reply = append(
        &mut log,
        ce::OUTPUT_REPLY,
        &format!("{}important conclusion", "body ".repeat(60)),
        &[],
    );
    let tool = append(&mut log, ce::TOOL_EXEC_COMPLETED, "tool secret", &[]);
    let text = excerpt(&log);
    assert!(text.contains("NOT a generated summary"));
    assert!(text.contains(path.to_str().unwrap()));
    assert!(text.contains(&format!("snapshot through {}", tool.id)));
    assert!(text.contains(&first.id) && text.contains(&second.id) && text.contains(&reply.id));
    assert!(
        !text.contains(&forwarded.id)
            && !text.contains("expanded secret")
            && !text.contains("tool secret")
    );
    assert_eq!(text.matches("same question").count(), 2);
    assert!(text.contains("important conclusion"));
    assert_eq!(log.reader().cost().events.load(Ordering::Relaxed), 0);
    assert_eq!(log.reader().cost().bytes.load(Ordering::Relaxed), 0);
    append(&mut log, ce::USER_MESSAGE, "new progress", &[]);
    assert!(excerpt(&log).contains("new progress"));
}

#[test]
fn a_huge_latest_message_keeps_both_ends_and_bounds_copies_with_unicode() {
    let mut log = log(None);
    append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &"x".repeat(1_000_000),
        &[],
    );
    append(
        &mut log,
        ce::USER_MESSAGE,
        &format!("BEGIN{}END", "界".repeat(20_000)),
        &[],
    );
    let text = excerpt(&log);
    assert!(text.contains("BEGIN") && text.contains("END") && text.contains("middle omitted"));
    assert!(text.contains("unavailable (in-memory ledger)"));
    assert!(text.chars().count() < FOREIGN_EXCERPT_CHARS + 1_000);
    assert_eq!(log.reader().cost().events.load(Ordering::Relaxed), 0);
    assert_eq!(log.reader().cost().bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn total_budget_keeps_recent_messages_whole_and_stops_before_older_content() {
    let mut log = log(None);
    append(&mut log, ce::USER_MESSAGE, &"old".repeat(4_000), &[]);
    let recent = format!("{}RECENT-END", "recent ".repeat(80));
    for _ in 0..8 {
        append(&mut log, ce::USER_MESSAGE, &recent, &[]);
    }
    let text = excerpt(&log);
    assert_eq!(text.matches(&recent).count(), 8);
    assert!(!text.contains("oldold"));
    assert!(text.contains("omitted by the excerpt budget"));
    assert!(text.chars().count() < FOREIGN_EXCERPT_CHARS + 1_000);
}
