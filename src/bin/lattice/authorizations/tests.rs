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
fn selection_is_per_question_and_never_survives_cancellation_or_restore() {
    let mut state = Authorizations::default();
    state.set_scoped(true);
    state.observe(&event(trust_policy::AUTH_REQUESTED, "first", &["a"]));
    assert_eq!(state.prompt().unwrap().unwrap().selected, Choice::Refuse);
    state.move_selection(isize::MIN).unwrap();
    state.observe(&event(browser_tools::AUTH_REQUESTED, "second", &["b"]));
    assert_eq!(state.prompt().unwrap().unwrap().selected, Choice::Once);
    state.observe(&event(ce::INTERRUPTED, "cancel-first", &["a"]));
    assert_eq!(state.next(), Some("second"));
    assert_eq!(state.prompt().unwrap().unwrap().selected, Choice::Refuse);
    state.move_selection(isize::MIN).unwrap();
    state.restore(state.history());
    assert_eq!(state.answer_selected(), Some(("second".into(), false)));
    assert_eq!(state.answer_selected(), None);
    state.observe(&event(trust_policy::AUTH_REQUESTED, "third", &["c"]));
    state.move_selection(isize::MIN).unwrap();
    assert_eq!(state.answer_selected(), Some(("third".into(), true)));
}

#[test]
fn restored_prompt_resolves_its_own_ledger_event_and_rejects_missing_data() {
    use lattice::{EventDraft, EventLog, EventTypeDecl};
    let mut log = EventLog::in_memory(
        vec![EventTypeDecl::new(trust_policy::AUTH_REQUESTED, "fixture")],
        "test",
    );
    let first = log
        .append(
            EventDraft::new(
                trust_policy::AUTH_REQUESTED,
                &[],
                json!({"tool":"Browser", "summary":"click the first button"}),
            ),
            "fixture",
        )
        .unwrap();
    let second = log
        .append(
            EventDraft::new(
                trust_policy::AUTH_REQUESTED,
                &[],
                json!({"tool":"Browser", "summary":"click the second button"}),
            ),
            "fixture",
        )
        .unwrap();
    let mut state = Authorizations::default();
    state.bind_reader(log.reader());
    state.restore(vec![
        (first.id.clone(), "a".into()),
        (second.id.clone(), "b".into()),
    ]);
    let prompt = state.prompt().unwrap().unwrap();
    assert_eq!(prompt.request, first.id);
    assert!(prompt.description.contains("first button"));
    assert!(!prompt.description.contains("second button"));
    assert!(!prompt.description.contains("y = allow"));
    state.answer_selected();
    assert!(state
        .prompt()
        .unwrap()
        .unwrap()
        .description
        .contains("second button"));
    state.restore(vec![("missing".into(), "c".into())]);
    assert!(
        state.prompt().is_err(),
        "a missing request is not an empty prompt"
    );
}

#[test]
fn scope_choices_follow_service_capability_and_the_actual_question() {
    for (kind, grants, expected) in [
        (
            operation_policy::AUTH_REQUESTED,
            json!([{"kind":"fixture"}]),
            vec![Choice::Once, Choice::Flow, Choice::Refuse],
        ),
        (
            operation_policy::AUTH_REQUESTED,
            json!([]),
            vec![Choice::Once, Choice::Refuse],
        ),
        (
            trust_policy::AUTH_REQUESTED,
            json!(null),
            vec![
                Choice::Once,
                Choice::Flow,
                Choice::Permanent,
                Choice::Refuse,
            ],
        ),
        (
            browser_tools::AUTH_REQUESTED,
            json!(null),
            vec![Choice::Once, Choice::Flow, Choice::Refuse],
        ),
        (
            lattice::components::expert_definitions::AUTH_REQUESTED,
            json!(null),
            vec![Choice::Once, Choice::Flow, Choice::Refuse],
        ),
    ] {
        let mut state = Authorizations::default();
        state.set_scoped(true);
        let mut question = event(kind, "question", &["call"]);
        question.payload = json!({"grants":grants});
        state.observe(&question);
        assert_eq!(state.prompt().unwrap().unwrap().choices, expected, "{kind}");
        assert_eq!(state.prompt().unwrap().unwrap().selected, Choice::Refuse);
        state.move_selection(-1).unwrap();
        assert_eq!(
            state.prompt().unwrap().unwrap().selected,
            expected[expected.len() - 2]
        );
        state.scroll_to(10);
        state.observe(&event(
            browser_tools::AUTH_REQUESTED,
            "next",
            &["next-call"],
        ));
        state.answer_oldest();
        let next = state.prompt().unwrap().unwrap();
        assert_eq!(next.selected, Choice::Refuse);
        assert_eq!(next.detail_line, 0);
    }
    let mut legacy = Authorizations::default();
    legacy.observe(&event(trust_policy::AUTH_REQUESTED, "admit", &["call"]));
    assert_eq!(
        legacy.prompt().unwrap().unwrap().choices,
        vec![Choice::Permanent, Choice::Refuse]
    );
    legacy.move_selection(-1).unwrap();
    assert_eq!(
        legacy.prompt().unwrap().unwrap().selected,
        Choice::Permanent
    );
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
