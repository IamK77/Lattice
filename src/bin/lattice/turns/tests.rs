use super::*;
use serde_json::json;

fn event(kind: &str) -> EventEnvelope {
    EventEnvelope {
        v: 1,
        id: "event".into(),
        seq: 1,
        stream: "turns".into(),
        time: "t".into(),
        event_type: kind.into(),
        source: "fixture".into(),
        causes: vec![],
        origin: None,
        reason: None,
        payload: json!({}),
    }
}

#[test]
fn local_activity_never_becomes_history_and_restore_settles_the_old_done() {
    let history = History {
        busy: false,
        waiting: true,
        done: true,
        number: 7,
    };
    let mut turns = Turns::default();
    turns.restore(history, 500);
    assert_eq!(turns.done_at(), Some(500));
    turns.activate();
    assert!(turns.busy());
    assert_eq!(
        turns.done_at(),
        Some(500),
        "side activation does not clear Done"
    );
    turns.optimistic_activity();
    assert!(turns.done_at().is_none());
    assert!(turns.waiting());
    assert_eq!(turns.history(), history);
    turns.quiescent(800);
    assert!(!turns.busy());
    assert_eq!(turns.done_at(), Some(800));
    assert_eq!(turns.history(), history);
    turns.quiescent(900);
    assert_eq!(
        turns.done_at(),
        Some(800),
        "idle notices do not restart Done"
    );
    turns.restore(history, 1_000);
    assert_eq!(turns.done_at(), Some(1_000));
    assert_eq!(turns.history(), history);
}

#[test]
fn only_recorded_boundaries_replace_live_activity_and_advance_turns() {
    let mut turns = Turns::default();
    turns.quiescent(1);
    assert!(turns.done_at().is_none());
    let mut forwarded = event(ce::USER_MESSAGE);
    forwarded.causes.push("original".into());
    assert!(!turns.observe(&forwarded, 2));
    assert_eq!(turns.history(), History::default());
    turns.seed_number(usize::MAX);
    assert!(turns.observe(&event(ce::WAKE), 3));
    assert_eq!(turns.number(), 0);
    assert!(turns.observe(&event(ce::USER_MESSAGE), 4));
    assert_eq!(turns.number(), 1);
    assert_eq!(
        turns.history(),
        History {
            busy: true,
            waiting: false,
            done: false,
            number: 1
        }
    );
    turns.quiescent(5);
    let before = turns.history();
    for kind in [ce::OUTPUT_REPLY, ce::INTERRUPTED, ce::MODEL_CALL_COMPLETED] {
        assert!(!turns.observe(&event(kind), 6));
        assert!(!turns.busy());
        assert_eq!(turns.done_at(), Some(5));
        assert_eq!(turns.history(), before);
    }
    for purpose in [serde_json::Value::Null, json!("condense")] {
        let mut background = event(ce::MODEL_CALL_STARTED);
        background.payload = json!({"purpose":purpose});
        assert!(!turns.observe(&background, 7));
        assert!(!turns.busy());
        assert_eq!(turns.done_at(), Some(5));
        assert_eq!(turns.history(), before);
    }
    assert!(!turns.observe(&event(ce::MODEL_CALL_STARTED), 8));
    assert!(turns.busy());
    assert!(turns.done_at().is_none());
    assert_eq!(turns.number(), 1);
    assert!(!turns.observe(&event(minimal_loop::WAITING), 9));
    assert_eq!(
        turns.history(),
        History {
            busy: false,
            waiting: true,
            done: false,
            number: 1
        }
    );
    assert!(!turns.busy() && turns.done_at().is_none());
    assert!(!turns.observe(&event(ce::TURN_COMPLETED), 10));
    assert_eq!(
        turns.history(),
        History {
            busy: false,
            waiting: false,
            done: true,
            number: 1
        }
    );
    assert_eq!(turns.done_at(), Some(10));
}

#[test]
fn pure_turn_history_reports_neutral_events_without_a_display_clock() {
    let mut history = History::default();
    assert_eq!(history.observe(&event(ce::OUTPUT_REPLY)), None);
    assert_eq!(history.observe(&event(ce::USER_MESSAGE)), Some(true));
    assert_eq!(history.number, 1);
    assert_eq!(history.observe(&event(ce::TURN_COMPLETED)), Some(false));
    assert!(history.done && !history.busy);
    let before = history;
    assert_eq!(history.observe(&event(ce::INTERRUPTED)), None);
    assert_eq!(history, before);
    assert_eq!(history.observe(&event(minimal_loop::WAITING)), Some(false));
    assert!(history.waiting && !history.done);
}
