use super::*;
use ratatui::{backend::TestBackend, Terminal};
use serde_json::{json, Value};

#[path = "interruption_tests/indexed.rs"]
mod indexed;
#[path = "interruption_tests/races.rs"]
mod races;

fn event(seq: u64, kind: &str, cause: Option<u64>, payload: Value) -> EventEnvelope {
    EventEnvelope {
        v: 1,
        id: format!("e{seq}"),
        seq,
        stream: "test".into(),
        time: "t".into(),
        event_type: kind.into(),
        source: "test".into(),
        causes: cause.into_iter().map(|seq| format!("e{seq}")).collect(),
        origin: None,
        reason: None,
        payload,
    }
}

fn interrupted(by: &str) -> Vec<EventEnvelope> {
    vec![
        event(
            1,
            core_events::USER_MESSAGE,
            None,
            json!({"text":"question"}),
        ),
        event(
            2,
            core_events::MODEL_CALL_STARTED,
            Some(1),
            json!({"model":"fixture","input":{"parts":[],"fingerprint":"fixture"}}),
        ),
        event(3, core_events::INTERRUPTED, Some(2), json!({"by":by})),
        event(
            4,
            core_events::OUTPUT_REPLY,
            Some(3),
            json!({"cancelled":true}),
        ),
        event(5, core_events::TURN_COMPLETED, Some(3), json!({})),
    ]
}

fn frame(ui: &mut Ui) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    super::draw_ui(&mut terminal, ui).unwrap();
    let buffer = terminal.backend().buffer();
    (0..20)
        .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn interruption_status_uses_the_footer_without_a_synthetic_reply() {
    for (reason, expected) in [
        ("deadline", "Timed out"),
        ("cancelled", "Interrupted"),
        ("future-reason", "Interrupted"),
    ] {
        let mut ui = Ui::replayed(&interrupted(reason));
        let screen = frame(&mut ui);
        assert!(screen.contains(expected), "{reason}: {screen}");
        assert!(!screen.contains("Done"), "{reason}: {screen}");
        assert!(!screen.contains("[interrupted]"), "{reason}: {screen}");
        assert!(
            !ui.entries
                .iter()
                .any(|entry| matches!(entry, Entry::Agent(_))),
            "no text means no invented answer card"
        );
    }
}

#[test]
fn interruption_status_preserves_actual_live_text() {
    let events = interrupted("deadline");
    let mut ui = Ui::replayed(&[]);
    for event in &events[..2] {
        ui.absorb(event, event.seq as usize);
    }
    ui.live_output.seed_reply("Actual partial answer".into());
    for event in &events[2..] {
        ui.absorb(event, event.seq as usize);
    }
    let screen = frame(&mut ui);
    assert!(screen.contains("Actual partial answer"), "{screen}");
    assert!(screen.contains("Timed out"), "{screen}");
    assert!(!screen.contains("[interrupted]"), "{screen}");
    ui.domain.turns.quiescent(100);
    assert_eq!(ui.turn_status(), "Timed out");
    assert_eq!(
        ui.entries
            .iter()
            .filter(|entry| matches!(entry, Entry::Agent(text) if text == "Actual partial answer"))
            .count(),
        1
    );
}

#[test]
fn interruption_status_keeps_committed_cancelled_text_once_and_literal_text_intact() {
    for text in ["Actual answer", "[interrupted]", ""] {
        let mut events = interrupted("user");
        events[2] = event(
            3,
            core_events::MODEL_CALL_COMPLETED,
            Some(2),
            json!({"status":"cancelled","text":text}),
        );
        events[3].payload = json!({"cancelled":true,"text":text});
        let mut ui = Ui::replayed(&events[..2]);
        if !text.is_empty() {
            ui.live_output.seed_reply(text.into());
        }
        for event in &events[2..] {
            ui.absorb(event, event.seq as usize);
        }
        assert_eq!(ui.turn_status(), "Interrupted");
        let answers: Vec<_> = ui
            .entries
            .iter()
            .filter_map(|entry| {
                if let Entry::Agent(text) = entry {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(answers, if text.is_empty() { vec![] } else { vec![text] });
    }
}

#[test]
fn interruption_status_ignores_tool_and_auxiliary_interruptions() {
    let mut ui = Ui::replayed(&interrupted("deadline")[..2]);
    ui.absorb(
        &event(
            3,
            core_events::TOOL_EXEC_STARTED,
            Some(2),
            json!({"call":"tool","tool":"Read"}),
        ),
        3,
    );
    ui.live_output.seed_reply("Current answer".into());
    ui.absorb(
        &event(
            4,
            core_events::INTERRUPTED,
            Some(3),
            json!({"by":"deadline"}),
        ),
        4,
    );
    ui.absorb(
        &event(
            5,
            core_events::MODEL_CALL_STARTED,
            Some(2),
            json!({"purpose":"condense"}),
        ),
        5,
    );
    ui.absorb(
        &event(
            6,
            core_events::INTERRUPTED,
            Some(5),
            json!({"by":"deadline"}),
        ),
        6,
    );
    assert!(ui.busy());
    assert_eq!(ui.streaming(), "Current answer");
    ui.absorb(
        &event(
            7,
            core_events::MODEL_CALL_COMPLETED,
            Some(2),
            json!({"status":"ok","text":"Final answer"}),
        ),
        7,
    );
    ui.absorb(
        &event(
            8,
            core_events::OUTPUT_REPLY,
            Some(7),
            json!({"text":"Final answer"}),
        ),
        8,
    );
    ui.absorb(
        &event(9, core_events::TURN_COMPLETED, Some(7), json!({})),
        9,
    );
    assert!(!ui.busy());
    assert_eq!(ui.turn_status(), "Done");
    assert!(frame(&mut ui).contains("Done"));
}

#[test]
fn interruption_status_rejects_old_endings_and_quiet_after_new_activity() {
    let mut ui = Ui::replayed(&interrupted("deadline"));
    ui.domain.turns.submitted_input();
    ui.domain.turns.quiescent(20);
    assert!(ui.busy());
    ui.absorb(
        &event(6, core_events::USER_MESSAGE, None, json!({"text":"next"})),
        21,
    );
    ui.absorb(
        &event(7, core_events::MODEL_CALL_STARTED, Some(6), json!({})),
        22,
    );
    ui.live_output.seed_reply("New answer".into());
    ui.live_output.seed_thinking("New thought".into());
    for old in [
        event(
            8,
            core_events::INTERRUPTED,
            Some(2),
            json!({"by":"deadline"}),
        ),
        event(
            9,
            core_events::MODEL_CALL_COMPLETED,
            Some(2),
            json!({"status":"cancelled","reasoning":[{"kind":"text","text":"Old thought"}]}),
        ),
        event(10, core_events::TURN_COMPLETED, Some(3), json!({})),
    ] {
        ui.absorb(&old, old.seq as usize);
    }
    ui.domain.turns.quiescent(30);
    assert!(ui.busy());
    assert!(ui.done_at().is_none());
    assert_eq!(ui.streaming(), "New answer");
    assert_eq!(ui.thinking(), "New thought");
}

#[test]
fn interruption_status_follows_forwarded_calls_and_keeps_first_outcome() {
    let mut ui = Ui::replayed(&interrupted("deadline")[..2]);
    ui.absorb(
        &event(3, core_events::MODEL_CALL_STARTED, Some(2), json!({})),
        3,
    );
    ui.live_output.seed_reply("Partial".into());
    for next in [
        event(
            4,
            core_events::INTERRUPTED,
            Some(2),
            json!({"by":"deadline"}),
        ),
        event(
            5,
            core_events::OUTPUT_REPLY,
            Some(4),
            json!({"cancelled":true}),
        ),
        event(6, core_events::TURN_COMPLETED, Some(4), json!({})),
        event(
            7,
            core_events::MODEL_CALL_COMPLETED,
            Some(3),
            json!({"status":"cancelled"}),
        ),
        event(8, core_events::TURN_COMPLETED, Some(7), json!({})),
    ] {
        ui.absorb(&next, next.seq as usize);
    }
    assert_eq!(ui.turn_status(), "Timed out");
    assert_eq!(
        ui.entries
            .iter()
            .filter(|entry| matches!(entry, Entry::Agent(text) if text == "Partial"))
            .count(),
        1
    );
}
