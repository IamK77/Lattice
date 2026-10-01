//! Redraw eligibility must agree with the state used to draw authorization waits.
use super::*;
use lattice::components::{browser_tools, expert_definitions};
use ratatui::backend::TestBackend;

const SOURCES: [(&str, &str); 3] = [
    (trust_policy::AUTH_REQUESTED, trust_policy::DECISION),
    (browser_tools::AUTH_REQUESTED, browser_tools::DECISION),
    (
        expert_definitions::AUTH_REQUESTED,
        expert_definitions::DECISION,
    ),
];

fn event(kind: &str, id: &str, held: &str) -> EventEnvelope {
    EventEnvelope {
        v: 1,
        id: id.into(),
        seq: 1,
        stream: "animation-test".into(),
        time: "t".into(),
        event_type: kind.into(),
        source: "fixture".into(),
        causes: vec![held.into()],
        origin: None,
        reason: None,
        payload: json!({"held": held, "tool": "Run", "summary": "synthetic approval"}),
    }
}

fn authorization_wait(kind: &str, tick: usize) -> Ui {
    let mut ui = Ui::replayed(&[]);
    ui.tick = tick;
    ui.domain.turns.seed_busy(true);
    ui.push_local_card(Entry::Tool(ToolCard {
        call: Some("call-a".into()),
        name: "run".into(),
        args: json!({"command": "echo synthetic"}),
        status: ToolStatus::Running,
        output: Vec::new(),
        changed: None,
        edit_diff: None,
    }));
    fold_render(
        &mut ui,
        RenderEvent::Appended(Box::new(event(kind, "question-a", "call-a"))),
    )
    .unwrap();
    assert!(animating(&ui));
    fold_render(&mut ui, RenderEvent::Quiescent).unwrap();
    ui.tick = ui.tick.wrapping_add(DONE_SETTLE + 1);
    ui
}

fn tool_spinner(ui: &Ui) -> char {
    let mut terminal = Terminal::new(TestBackend::new(84, 28)).unwrap();
    draw(&mut terminal, ui).unwrap();
    let tool_row = terminal
        .backend()
        .buffer()
        .content()
        .chunks(84)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .find(|row| row.contains("Run"))
        .expect("the running tool has a visible row");
    tool_row
        .chars()
        .find(|glyph| activity::SPINNER.contains(glyph))
        .expect("the tool row contains its spinner")
}

fn phrase_styles(ui: &Ui) -> Vec<Style> {
    // The leading activity glyph can rotate independently of the phrase sweep.
    activity::line(ui)
        .spans
        .iter()
        .skip(1)
        .map(|span| span.style)
        .collect()
}

#[test]
fn authorization_animation_survives_quiescence_for_each_source_and_tick_wrap() {
    for (requested, _) in SOURCES {
        for tick in [0, usize::MAX - 10] {
            let mut ui = authorization_wait(requested, tick);
            assert!(!ui.domain.turns.busy());
            assert!(ui.busy());
            assert_eq!(ui.pending_auth(), Some("question-a"));

            let spinner = tool_spinner(&ui);
            ui.tick = ui.tick.wrapping_add(6);
            assert_ne!(spinner, tool_spinner(&ui), "tool animation: {requested}");
            let phrase = phrase_styles(&ui);
            assert!(!phrase.is_empty());
            let mut sweep_changed = false;
            // Observe two sweep cycles: the phrase intentionally rests between
            // sweeps, so adjacent frames need not have different text styles.
            for _ in 0..120 {
                ui.tick = ui.tick.wrapping_add(1);
                sweep_changed |= phrase_styles(&ui) != phrase;
                assert!(
                    animating(&ui),
                    "authorization animations must request frames without input: {requested}"
                );
            }
            assert!(sweep_changed, "phrase styles must animate: {requested}");
        }
    }
}

#[test]
fn restored_authorization_animates_without_an_active_turn_or_settling_window() {
    for (requested, decided) in SOURCES {
        let mut ui = Ui::replayed(&[event(requested, "question-a", "call-a")]);
        assert!(!ui.domain.turns.busy());
        assert!(ui.domain.turns.done_at().is_none());
        assert!(ui.busy());
        assert!(animating(&ui));

        let unsettled = ui.domain.authorizations.history();
        ui.domain.authorizations.answer_selected().unwrap();
        assert!(!animating(&ui), "the local answer ends this idle wait");
        ui.domain.authorizations.restore(unsettled);
        assert!(animating(&ui), "restored unanswered history animates again");
        let mut outcome = event(decided, "outcome-a", "call-a");
        outcome.seq = 2; // New live events must follow the replayed prefix.
        fold_render(&mut ui, RenderEvent::Appended(Box::new(outcome))).unwrap();
        assert!(ui.domain.authorizations.history().is_empty());
        assert!(
            !animating(&ui),
            "the recorded outcome settles the restored wait"
        );
    }
}

#[test]
fn local_allow_and_refuse_keep_remaining_questions_animated_then_stop() {
    for allow in [false, true] {
        let mut ui = authorization_wait(SOURCES[0].0, 0);
        ui.note_authorization(&event(SOURCES[1].0, "question-b", "call-b"));
        ui.note_authorization(&event(SOURCES[2].0, "question-c", "call-c"));
        for id in ["question-a", "question-b", "question-c"] {
            assert!(animating(&ui), "an unanswered question still remains: {id}");
            ui.domain.authorizations.select_allow(allow);
            assert_eq!(
                ui.domain.authorizations.answer_selected(),
                Some((id.into(), allow))
            );
        }
        // Local answers leave questions in history until their outcomes arrive,
        // but that history must not keep an otherwise idle screen redrawing.
        assert_eq!(ui.domain.authorizations.history().len(), 3);
        assert!(ui.pending_auth().is_none());
        assert!(!ui.busy());
        assert!(!animating(&ui));
    }
}

#[test]
fn decisions_and_interruptions_stop_only_after_the_last_question_is_removed() {
    for (requested, decided) in SOURCES {
        for outcome in [decided, core_events::INTERRUPTED] {
            let mut ui = authorization_wait(requested, 0);
            ui.note_authorization(&event(requested, "question-b", "call-b"));
            ui.note_authorization(&event(outcome, "outcome-b", "call-b"));
            assert_eq!(ui.pending_auth(), Some("question-a"));
            assert!(
                animating(&ui),
                "the remaining question must animate: {outcome}"
            );
            ui.note_authorization(&event(outcome, "outcome-a", "call-a"));
            assert!(ui.pending_auth().is_none());
            assert!(!ui.busy());
            assert!(
                !animating(&ui),
                "a settled idle screen must stop: {outcome}"
            );
        }
    }
}

#[test]
fn answering_authorization_preserves_active_turn_and_done_settling_animation() {
    for tick in [0, usize::MAX - 10] {
        let mut ui = authorization_wait(SOURCES[0].0, tick);
        ui.domain.turns.activate();
        ui.domain.authorizations.answer_selected().unwrap();
        assert!(ui.pending_auth().is_none());
        assert!(animating(&ui), "answering must not suppress a running turn");
        fold_render(&mut ui, RenderEvent::Quiescent).unwrap();
        assert!(!ui.busy());
        assert!(
            animating(&ui),
            "the Done line still needs its settling frames"
        );
        ui.tick = ui.tick.wrapping_add(DONE_SETTLE);
        assert!(animating(&ui), "the settling boundary is inclusive");
        ui.tick = ui.tick.wrapping_add(1);
        assert!(!animating(&ui), "the settled screen is idle again");
    }
}
