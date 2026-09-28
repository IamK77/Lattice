//! Behavioral boundaries fixed before moving background state ownership.
use super::*;

fn event(seq: u64, kind: &str, payload: Value) -> lattice::EventEnvelope {
    serde_json::from_value(json!({
        "v":1,"id":format!("ev_{seq}_background"),"seq":seq,"stream":"background-test",
        "time":"2026-09-19T00:00:00Z","type":kind,"source":"test","causes":[],"payload":payload
    }))
    .unwrap()
}

fn complete(ui: &mut Ui, seq: u64, tool: &str, args: Value, status: &str, result: Value) {
    let call = format!("call-{seq}");
    ui.note_background(
        &event(
            seq,
            core_events::TOOL_EXEC_STARTED,
            json!({"call":call,"tool":tool,"arguments":args}),
        ),
        4,
    );
    ui.note_background(
        &event(
            seq + 1,
            core_events::TOOL_EXEC_COMPLETED,
            json!({"call":call,"status":status,"result":result}),
        ),
        9,
    );
}

#[test]
fn cancellation_receipts_remove_rows_before_status_is_checked() {
    let mut ui = Ui::replayed(&[]);
    complete(
        &mut ui,
        1,
        "Schedule",
        json!({"interval_ms":1000}),
        "ok",
        json!({"timer":7}),
    );
    complete(
        &mut ui,
        3,
        "Watch",
        json!({"path":"fixture"}),
        "ok",
        json!({"watch":8}),
    );
    assert_eq!(ui.domain.background.rows().len(), 2);
    // Preserve the existing reducer, not a new interpretation of tool success.
    complete(
        &mut ui,
        5,
        "Unschedule",
        json!({"timer":7}),
        "error",
        json!({"timer":7}),
    );
    assert_eq!(ui.domain.background.rows().len(), 1);
    assert_eq!(ui.domain.background.rows()[0].key, "watch:8");
    complete(
        &mut ui,
        7,
        "Unwatch",
        json!({"watch":8}),
        "error",
        json!({"watch":8}),
    );
    assert!(ui.domain.background.rows().is_empty());
    complete(
        &mut ui,
        9,
        "Run",
        json!({"command":"ignored"}),
        "error",
        json!({"job":9,"background":true}),
    );
    assert!(ui.domain.background.rows().is_empty());
}

#[test]
fn replacing_a_background_identity_moves_it_to_the_end_and_resets_progress() {
    let mut ui = Ui::replayed(&[]);
    complete(
        &mut ui,
        1,
        "Schedule",
        json!({"interval_ms":1000}),
        "ok",
        json!({"timer":7}),
    );
    complete(
        &mut ui,
        3,
        "Watch",
        json!({"path":"old"}),
        "ok",
        json!({"watch":8}),
    );
    ui.domain.background.fixture_edit(0, |row| {
        row.tools = 99;
        row.tokens = 88;
        row.read_len = 777;
        row.fires = 6;
    });
    complete(
        &mut ui,
        5,
        "Schedule",
        json!({"delay_ms":5000}),
        "ok",
        json!({"timer":7}),
    );
    assert_eq!(
        ui.domain
            .background
            .rows()
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>(),
        ["watch:8", "timer:7"]
    );
    let row = &ui.domain.background.rows()[1];
    assert_eq!(
        (row.tools, row.tokens, row.fires, row.since, row.read_len),
        (0, 0, 0, 9, 0)
    );
    assert!(!row.standing);
    ui.note_background(
        &event(
            7,
            core_events::WAKE,
            json!({"source":"timer:7","body":{"watch":8}}),
        ),
        10,
    );
    assert_eq!(
        ui.domain.background.rows()[0].fires,
        1,
        "watch id takes precedence over source"
    );
    assert_eq!(
        ui.domain.background.rows().len(),
        2,
        "standing watch remains; unrelated timer remains"
    );
    ui.note_background(
        &event(8, core_events::WAKE, json!({"source":"timer:7","body":{}})),
        10,
    );
    assert_eq!(ui.domain.background.rows().len(), 1);
}

#[test]
fn a_failed_expert_refresh_keeps_both_progress_and_cursor_for_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("expert.jsonl");
    let first =
        serde_json::to_string(&event(1, core_events::TOOL_EXEC_STARTED, json!({}))).unwrap();
    std::fs::write(&path, format!("{first}\n")).unwrap();
    let mut ui = Ui::replayed(&[]);
    let mut row = started_live("ask", &json!({}), &json!({"job":1}), 0).unwrap();
    row.ledger = Some(path.display().to_string());
    ui.domain.background.fixture_push(row);
    ui.refresh_experts();
    assert_eq!(ui.domain.background.rows()[0].tools, 1);
    let cursor = ui.domain.background.rows()[0].read_len;
    let second =
        serde_json::to_string(&event(2, core_events::TOOL_EXEC_STARTED, json!({}))).unwrap();
    std::fs::write(&path, format!("{first}\n{second}\nnot-json\n")).unwrap();
    ui.refresh_experts();
    assert_eq!(
        ui.domain.background.rows()[0].tools,
        1,
        "partial reads must not commit counts"
    );
    assert_eq!(
        ui.domain.background.rows()[0].read_len,
        cursor,
        "or their cursor"
    );
    std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
    ui.refresh_experts();
    assert_eq!(
        ui.domain.background.rows()[0].tools,
        2,
        "retry counts the valid prefix once"
    );
    assert!(ui.domain.background.rows()[0].read_len > cursor);
    ui.refresh_experts();
    assert_eq!(ui.domain.background.rows()[0].tools, 2);
    assert_eq!(
        ui.domain.accounting.session_total().calls,
        0,
        "expert progress is not main usage"
    );
}

#[test]
fn expert_paths_are_resolved_once_at_creation_with_the_legacy_job_suffix() {
    for (legacy, segmented, extension) in [
        (false, false, "ledger"),
        (true, false, "jsonl"),
        (true, true, "ledger"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        if legacy {
            std::fs::write(directory.path().join("parent-sub-7.jsonl"), "").unwrap();
        }
        if segmented {
            std::fs::create_dir(directory.path().join("parent-sub-7.ledger")).unwrap();
        }
        let mut ui = Ui::replayed(&[]);
        ui.domain.expert_dir = Some(directory.path().to_path_buf());
        ui.domain.stream_id = "parent".into();
        complete(
            &mut ui,
            1,
            "ask",
            json!({"expert":"fixture","prompt":"work"}),
            "ok",
            json!({"job":"left:7"}),
        );
        assert_eq!(ui.domain.background.rows()[0].key, "expert:left:7");
        let expected = directory
            .path()
            .join(format!("parent-sub-7.{extension}"))
            .display()
            .to_string();
        assert_eq!(
            ui.domain.background.rows()[0].ledger.as_deref(),
            Some(expected.as_str())
        );
        if !segmented {
            std::fs::create_dir(directory.path().join("parent-sub-7.ledger")).unwrap();
        }
        ui.refresh_experts();
        assert_eq!(
            ui.domain.background.rows()[0].ledger.as_deref(),
            Some(expected.as_str()),
            "refresh never resolves the path again"
        );
    }
}
