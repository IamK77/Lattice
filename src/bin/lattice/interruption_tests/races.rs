use super::*;
use ratatui::crossterm::event::KeyEvent;

#[test]
fn review_interruption_status_queued_wake_does_not_acknowledge_local_input() {
    for queued_wake in [false, true] {
        let mut ui = Ui::replayed(&interrupted("deadline")[..2]);
        ui.domain.turns.submitted_input();
        ui.domain.accounting.seed_turn_total(lattice::Usage {
            prompt: 7,
            ..Default::default()
        });
        // These events were queued before Enter; the local input has not echoed.
        let third = if queued_wake {
            event(
                3,
                core_events::WAKE,
                None,
                json!({"summary":"background work finished"}),
            )
        } else {
            event(
                3,
                core_events::TOOL_EXEC_COMPLETED,
                None,
                json!({"call":"unrelated"}),
            )
        };
        for next in [
            third,
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
        ] {
            ui.absorb(&next, next.seq as usize);
        }
        assert!(ui.busy(), "queued_wake={queued_wake}: unrelated event acknowledged local input: status={}, done_at={:?}", ui.turn_status(), ui.done_at());
        assert!(ui.done_at().is_none());
        assert_eq!(
            ui.domain.accounting.turn_total().prompt,
            if queued_wake { 0 } else { 7 },
            "wake bookkeeping still follows the recorded boundary"
        );
        ui.domain.turns.quiescent(7);
        ui.absorb(
            &event(
                7,
                core_events::USER_MESSAGE,
                Some(1),
                json!({"text":"old forwarded input"}),
            ),
            7,
        );
        assert!(ui.busy() && ui.done_at().is_none());
        // The real echo releases local protection; its next round can finish.
        for next in [
            event(
                8,
                core_events::USER_MESSAGE,
                None,
                json!({"text":"new question"}),
            ),
            event(9, core_events::MODEL_CALL_STARTED, Some(8), json!({})),
            event(
                10,
                core_events::MODEL_CALL_COMPLETED,
                Some(9),
                json!({"status":"ok","text":"answer"}),
            ),
            event(
                11,
                core_events::OUTPUT_REPLY,
                Some(10),
                json!({"text":"answer"}),
            ),
            event(12, core_events::TURN_COMPLETED, Some(10), json!({})),
        ] {
            ui.absorb(&next, next.seq as usize);
        }
        assert!(!ui.busy());
        assert_eq!(ui.turn_status(), "Done");
        assert!(ui.done_at().is_some());
    }
}

#[test]
fn interruption_status_non_chat_operations_can_finish_on_quiet() {
    let mut ui = Ui::replayed(&[]);
    // Model switching and component removal have this local activity path;
    // their refusal/success is followed by quiet, not a user-message echo.
    ui.domain.turns.optimistic_activity();
    super::super::fold_render(&mut ui, RenderEvent::Quiescent).unwrap();
    assert!(
        !ui.busy(),
        "a non-chat operation must not wait for input to be recorded"
    );
    assert!(ui.done_at().is_some());
}

#[test]
fn interruption_status_does_not_settle_input_before_its_model_request() {
    for recorded in [false, true] {
        let mut ui = Ui::replayed(&interrupted("deadline")[..4]);
        ui.domain.turns.submitted_input();
        if recorded {
            ui.absorb(
                &event(
                    6,
                    core_events::USER_MESSAGE,
                    None,
                    json!({"text":"next question"}),
                ),
                6,
            );
        }
        ui.absorb(
            &event(7, core_events::TURN_COMPLETED, Some(3), json!({})),
            7,
        );
        ui.domain.turns.quiescent(8);
        assert!(
            ui.busy(),
            "recorded={recorded}: the old ending cannot settle new input"
        );
        assert!(ui.done_at().is_none());
    }
}

#[test]
fn interruption_status_keeps_partial_answer_when_input_is_submitted_mid_call() {
    for local in [false, true] {
        let mut ui = Ui::replayed(&interrupted("deadline")[..2]);
        ui.live_output.seed_reply("Partial before input".into());
        if local {
            ui.draft.edit().set("another question");
            super::super::on_key(
                &mut ui,
                None,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &Hit::default(),
            );
        }
        ui.absorb(
            &event(
                3,
                core_events::USER_MESSAGE,
                None,
                json!({"text":"another question"}),
            ),
            3,
        );
        assert_eq!(ui.streaming(), "Partial before input", "local={local}");
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
        ] {
            ui.absorb(&next, next.seq as usize);
        }
        assert_eq!(
            ui.entries
                .iter()
                .filter(
                    |entry| matches!(entry, Entry::Agent(text) if text == "Partial before input")
                )
                .count(),
            1
        );
    }
}

#[test]
fn interruption_status_does_not_duplicate_thinking_after_a_two_channel_completion() {
    let mut ui = Ui::replayed(&interrupted("deadline")[..2]);
    ui.live_output.seed_thinking("Old thought".into());
    ui.live_output.seed_reply("Prelude".into());
    ui.absorb(
        &event(
            3,
            core_events::MODEL_CALL_COMPLETED,
            Some(2),
            json!({
                "status":"ok", "text":"Prelude", "reasoning":[{"kind":"text","text":"Old thought"}],
                "toolCalls":[{"id":"tool","tool":"Read","arguments":{}}]
            }),
        ),
        3,
    );
    assert!(
        ui.thinking().is_empty(),
        "both committed channels must retire"
    );
    assert!(ui.streaming().is_empty());
    ui.absorb(
        &event(4, core_events::MODEL_CALL_STARTED, Some(3), json!({})),
        4,
    );
    ui.live_output.seed_thinking("New thought".into());
    for next in [
        event(
            5,
            core_events::INTERRUPTED,
            Some(4),
            json!({"by":"deadline"}),
        ),
        event(
            6,
            core_events::OUTPUT_REPLY,
            Some(5),
            json!({"cancelled":true}),
        ),
        event(7, core_events::TURN_COMPLETED, Some(5), json!({})),
    ] {
        ui.absorb(&next, next.seq as usize);
    }
    let thoughts: Vec<_> = ui
        .entries
        .iter()
        .filter_map(|entry| {
            if let Entry::Thinking(card) = entry {
                Some(card.lines.join("\n"))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(thoughts, ["Old thought", "New thought"]);
}

#[test]
fn interruption_status_mid_call_input_ends_with_its_existing_round() {
    let mut ui = Ui::replayed(&interrupted("deadline")[..2]);
    for next in [
        event(
            3,
            core_events::USER_MESSAGE,
            None,
            json!({"text":"next question"}),
        ),
        event(4, core_events::MODEL_CALL_STARTED, Some(2), json!({})),
        event(
            5,
            core_events::INTERRUPTED,
            Some(4),
            json!({"by":"deadline"}),
        ),
        event(
            6,
            core_events::OUTPUT_REPLY,
            Some(5),
            json!({"cancelled":true}),
        ),
        event(7, core_events::TURN_COMPLETED, Some(5), json!({})),
    ] {
        ui.absorb(&next, next.seq as usize);
    }
    assert!(!ui.busy(), "mid-call input does not create a second round");
    assert_eq!(ui.turn_status(), "Timed out");
    assert!(ui.done_at().is_some());
}

#[test]
fn interruption_status_tool_timeout_then_normal_model_reply_is_done() {
    let mut ui = Ui::replayed(&interrupted("deadline")[..2]);
    for next in [
        event(
            3,
            core_events::MODEL_CALL_COMPLETED,
            Some(2),
            json!({"status":"ok","toolCalls":[{"id":"tool","tool":"Read","arguments":{}}]}),
        ),
        event(
            4,
            core_events::TOOL_EXEC_STARTED,
            Some(3),
            json!({"call":"tool","tool":"Read"}),
        ),
        event(
            5,
            core_events::INTERRUPTED,
            Some(4),
            json!({"by":"deadline"}),
        ),
        event(6, core_events::MODEL_CALL_STARTED, Some(5), json!({})),
        event(
            7,
            core_events::MODEL_CALL_COMPLETED,
            Some(6),
            json!({"status":"ok","text":"Recovered answer"}),
        ),
        event(
            8,
            core_events::OUTPUT_REPLY,
            Some(7),
            json!({"text":"Recovered answer"}),
        ),
        event(9, core_events::TURN_COMPLETED, Some(7), json!({})),
    ] {
        ui.absorb(&next, next.seq as usize);
    }
    assert!(!ui.busy());
    assert_eq!(ui.turn_status(), "Done");
    assert!(frame(&mut ui).contains("Recovered answer"));
}
