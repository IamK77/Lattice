use super::*;
use serde_json::json;

fn event(kind: &str, id: &str, causes: &[&str]) -> EventEnvelope {
    EventEnvelope {
        v: 1,
        id: id.into(),
        seq: 1,
        stream: "test".into(),
        time: "t".into(),
        event_type: kind.into(),
        source: "fixture".into(),
        causes: causes.iter().map(|s| (*s).into()).collect(),
        origin: None,
        reason: None,
        payload: json!({}),
    }
}

#[test]
fn local_answers_advance_immediately_but_only_outcomes_retire_history() {
    for outcome in [
        trust_policy::DECISION,
        browser_tools::DECISION,
        ce::TOOL_EXEC_COMPLETED,
        ce::INTERRUPTED,
    ] {
        let mut state = Authorizations::default();
        let first = event(trust_policy::AUTH_REQUESTED, "first", &["a"]);
        let mut second = event(browser_tools::AUTH_REQUESTED, "second", &["forwarded"]);
        second.payload = json!({"held":"b"});
        state.observe(&first);
        state.observe(&second);
        assert_eq!(state.answer_oldest().as_deref(), Some("first"));
        assert_eq!(state.next(), Some("second"));
        assert_eq!(state.answer_oldest().as_deref(), Some("second"));
        assert!(state.answer_oldest().is_none());
        assert_eq!(state.answerable_count(), 0);
        assert_eq!(
            state.history(),
            vec![("first".into(), "a".into()), ("second".into(), "b".into())]
        );
        state.observe(&event(trust_policy::AUTH_REQUESTED, "third", &["c"]));
        let mut decision = event(outcome, "settled", &[]);
        decision.payload = json!({"held":"b"});
        state.observe(&decision);
        assert_eq!(
            state.history(),
            vec![("first".into(), "a".into()), ("third".into(), "c".into())]
        );
        assert_eq!(state.next(), Some("third"));
        state.observe(&event(outcome, "settled-a", &["a"]));
        assert_eq!(state.history(), vec![("third".into(), "c".into())]);
        assert_eq!(state.answerable_count(), 1);
    }
}

#[test]
fn restoring_unsettled_history_drops_only_the_local_answer_marks() {
    let mut state = Authorizations::default();
    state.observe(&event(trust_policy::AUTH_REQUESTED, "first", &["a"]));
    state.answer_oldest();
    let history = state.history();
    assert!(state.next().is_none());
    state.restore(history);
    assert_eq!(state.next(), Some("first"));
    assert_eq!(state.answerable_count(), 1);
}

#[test]
fn folding_and_answering_do_not_deduplicate_questions_or_held_calls() {
    let mut state = Authorizations::default();
    let ask = event(trust_policy::AUTH_REQUESTED, "same", &["call"]);
    state.observe(&ask);
    state.observe(&ask);
    assert_eq!(state.answerable_count(), 2);
    assert_eq!(state.answer_oldest().as_deref(), Some("same"));
    assert_eq!(state.answerable_count(), 1);
    assert_eq!(state.answer_oldest().as_deref(), Some("same"));
    assert_eq!(state.history().len(), 2);
    state.observe(&event(ce::INTERRUPTED, "stop", &["call"]));
    assert!(state.history().is_empty());
}
