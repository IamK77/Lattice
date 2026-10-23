use super::*;
use serde_json::{json, Value};

fn event(kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    EventEnvelope {
        v: 1,
        id: "input".into(),
        seq: 1,
        stream: "unseen".into(),
        time: "t".into(),
        event_type: kind.into(),
        source: "fixture".into(),
        causes: causes.iter().map(|s| (*s).into()).collect(),
        origin: None,
        reason: None,
        payload,
    }
}

#[test]
fn only_original_user_text_and_wakes_add_lines_in_recorded_order() {
    let mut unseen = Unseen::default();
    unseen.restore_at(
        vec!["restored".into()],
        lattice::view::activity::Activity {
            busy: true,
            ..Default::default()
        },
    );
    for (kind, causes, payload) in [
        (ce::USER_MESSAGE, vec![], json!({"text":"first"})),
        (ce::USER_MESSAGE, vec!["first"], json!({"text":"forwarded"})),
        (ce::USER_MESSAGE, vec![], json!({"text":7})),
        (ce::USER_MESSAGE, vec![], json!({"text":""})),
        (ce::WAKE, vec!["job"], json!({"source":"timer"})),
        (ce::WAKE, vec![], json!({"source":null})),
        (ce::TURN_COMPLETED, vec![], json!({})),
        (ce::INTERRUPTED, vec![], json!({})),
    ] {
        unseen.observe(&event(kind, &causes, payload), None);
    }
    assert_eq!(
        unseen.lines(),
        ["restored", "first", "", "timer ↯", "wake ↯"]
    );
}

#[test]
fn only_ordinary_requests_that_include_the_input_remove_it() {
    let mut unseen = Unseen::default();
    unseen.observe(&event(ce::MODEL_CALL_STARTED, &[], json!({})), None);
    unseen.observe(
        &event(ce::USER_MESSAGE, &[], json!({"text":"waiting"})),
        None,
    );
    for payload in [
        json!({}),
        json!({"input":{"parts":[{"event":"other"}]}}),
        json!({"purpose":null,"input":{"parts":[{"event":"input"}]}}),
        json!({"purpose":"condense","input":{"parts":[{"event":"input"}]}}),
    ] {
        unseen.observe(&event(ce::MODEL_CALL_STARTED, &[], payload), None);
        assert_eq!(unseen.lines(), ["waiting"]);
    }
    unseen.observe(
        &event(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({"input":{"parts":[{"event":"input"}]}}),
        ),
        None,
    );
    assert!(unseen.lines().is_empty());
}
