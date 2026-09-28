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
    unseen.restore(vec!["restored".into()]);
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
        unseen.observe(&event(kind, &causes, payload));
    }
    assert_eq!(
        unseen.lines(),
        ["restored", "first", "", "timer ↯", "wake ↯"]
    );
}

#[test]
fn every_model_request_carries_the_list_regardless_of_purpose() {
    for payload in [
        json!({}),
        json!({"purpose":null}),
        json!({"purpose":"condense"}),
    ] {
        let mut unseen = Unseen::default();
        unseen.restore(vec!["waiting".into()]);
        unseen.observe(&event(ce::MODEL_CALL_STARTED, &[], payload));
        assert!(unseen.lines().is_empty());
        unseen.observe(&event(ce::USER_MESSAGE, &[], json!({"text":"next"})));
        assert_eq!(unseen.lines(), ["next"]);
    }
}
