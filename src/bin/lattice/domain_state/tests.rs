use super::*;
use lattice::view::{EffortView, ModelRow, ModelView};
use serde_json::{json, Value};

fn event(seq: u64, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    serde_json::from_value(json!({"v":1,"id":format!("event-{seq}"),"seq":seq,
        "stream":"domain-test","time":"2026-09-19T00:00:00Z","type":kind,
        "source":"fixture","causes":causes,"payload":payload}))
    .unwrap()
}
fn model() -> ModelState {
    ModelState::new(
        ModelView {
            rows: vec![
                ModelRow {
                    id: "first".into(),
                    model: "first".into(),
                    dialect: "openai".into(),
                    ..ModelRow::default()
                },
                ModelRow {
                    id: "second".into(),
                    model: "second".into(),
                    dialect: "anthropic".into(),
                    ..ModelRow::default()
                },
            ],
            now: Some(0),
        },
        lattice::models::Entry {
            id: "first".into(),
            model: "first".into(),
            adapter: "openai".into(),
            base_url: String::new(),
            key_env: String::new(),
            profile: None,
        },
        EffortView::default(),
        Some(999),
    )
}
fn same<T: TurnState, B: BackgroundState<T::Clock>>(
    domain: &Domain<T, B>,
    reference: &crate::terminal_host::Ui,
) {
    let reference = &reference.domain;
    assert_eq!(domain.title, reference.title);
    assert_eq!(domain.skills, reference.skills);
    assert_eq!(domain.turns.history(), reference.turns.history());
    assert_eq!(
        domain.authorizations.history(),
        reference.authorizations.history()
    );
    assert_eq!(domain.unseen.lines(), reference.unseen.lines());
    assert_eq!(domain.model.running(), reference.model.running());
    assert_eq!(
        serde_json::to_value(domain.model.catalog()).unwrap(),
        serde_json::to_value(reference.model.catalog()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(domain.model.effort()).unwrap(),
        serde_json::to_value(reference.model.effort()).unwrap()
    );
    assert_eq!(
        domain.model.effective_window(),
        reference.model.effective_window()
    );
    assert_eq!(
        domain.accounting.last_call(),
        reference.accounting.last_call()
    );
    assert_eq!(
        domain.accounting.turn_total(),
        reference.accounting.turn_total()
    );
    assert_eq!(
        domain.accounting.session_total(),
        reference.accounting.session_total()
    );
    assert_eq!(domain.accounting.parts(), reference.accounting.parts());
    assert_eq!(
        domain.accounting.previous(),
        reference.accounting.previous()
    );
    assert_eq!(
        domain.accounting.turn_growth(),
        reference.accounting.turn_growth()
    );
    assert_eq!(
        domain.accounting.session_growth(),
        reference.accounting.session_growth()
    );
    assert_eq!(
        domain.background.history().rows(),
        reference.background.history().rows()
    );
    assert_eq!(domain.event_inputs, reference.event_inputs);
}

#[test]
fn live_coordinator_retires_receipts_only_for_new_recorded_turns() {
    let mut ui = crate::terminal_host::Ui::replayed(&[]);
    ui.flash = Some("local receipt".into());
    ui.domain.turns.optimistic_activity();
    ui.absorb_state(&event(1, ce::OUTPUT_REPLY, &[], json!({})), 7, None);
    assert_eq!(ui.flash.as_deref(), Some("local receipt"));
    assert!(ui.domain.turns.busy());
    ui.absorb_state(
        &event(
            2,
            ce::USER_MESSAGE,
            &["forwarded"],
            json!({"text":"expanded"}),
        ),
        8,
        None,
    );
    assert_eq!(ui.flash.as_deref(), Some("local receipt"));
    assert_eq!(ui.domain.turns.number(), 0);
    ui.absorb_state(
        &event(3, ce::WAKE, &[], json!({"source":"fixture","body":{}})),
        9,
        None,
    );
    assert!(ui.flash.is_none());
    assert_eq!(ui.domain.turns.number(), 1);
}

#[test]
fn single_stage_test_adapters_do_not_change_unrelated_preparation_state() {
    let mut ui = crate::terminal_host::Ui::replayed(&[]);
    ui.flash = Some("kept".into());
    ui.domain.unseen.restore(vec!["not carried".into()]);
    ui.note_usage(&event(
        1,
        ce::MODEL_CALL_STARTED,
        &[],
        json!({"model":"first"}),
    ));
    assert!(!ui.domain.turns.busy());
    assert_eq!(ui.domain.unseen.lines(), ["not carried"]);
    assert!(ui.domain.event_inputs.size("event-1").is_none());
    ui.note_background(
        &event(2, ce::WAKE, &[], json!({"source":"fixture","body":{}})),
        10,
    );
    assert_eq!(ui.domain.turns.number(), 0);
    assert_eq!(ui.flash.as_deref(), Some("kept"));
    ui.note_turn_boundary(
        &event(3, ce::USER_MESSAGE, &[], json!({"text":"question"})),
        11,
    );
    assert_eq!(ui.domain.turns.number(), 1);
    assert!(ui.flash.is_none());
    assert_eq!(ui.domain.unseen.lines(), ["not carried"]);
    assert!(ui.domain.event_inputs.size("event-3").is_none());
    ui.note_model_swap(&event(
        4,
        ce::COMPONENT_REPLACED,
        &[],
        json!({"instance":"model","to":"scripted-model","config":{"model":"second"}}),
    ));
    assert_eq!(ui.domain.model.running().model, "second");
    assert!(ui.domain.event_inputs.size("event-4").is_none());
}

#[test]
fn both_domain_modes_match_the_original_ui_fold_after_every_event() {
    let mut reference = crate::terminal_host::Ui::replayed(&[]);
    reference.domain.title = "first · fixture · suffix".into();
    reference.domain.model = model();
    let mut live = Live {
        title: reference.domain.title.clone(),
        model: model(),
        ..Live::default()
    };
    let mut historical = Historical {
        title: reference.domain.title.clone(),
        model: model(),
        ..Historical::default()
    };
    let events = [
        event(1, ce::USER_MESSAGE, &[], json!({"text":"question"})),
        event(
            2,
            skill_library::SKILL_LISTING,
            &[],
            json!({"skills":[{"name":"alpha","description":"A"},{"name":5},{"name":"beta"}]}),
        ),
        event(
            3,
            ce::MODEL_CALL_STARTED,
            &[],
            json!({"model":"first","system":"test","tools":[],"input":{"parts":[{"event":"event-1"}]}}),
        ),
        event(
            4,
            ce::MODEL_CALL_COMPLETED,
            &["event-3"],
            json!({"usage":{"prompt_tokens":10,"completion_tokens":3}}),
        ),
        event(
            5,
            ce::COMPONENT_REPLACED,
            &[],
            json!({"instance":"model","to":"anthropic-model","config":{"model":"second","profile":null}}),
        ),
        event(
            6,
            ce::MODEL_CALL_STARTED,
            &[],
            json!({"model":"second","input":{"parts":[{"event":"event-1"}]}}),
        ),
        event(
            7,
            ce::MODEL_CALL_COMPLETED,
            &["event-6"],
            json!({"usage":{"input_tokens":100,"cache_read_input_tokens":20,"cache_creation_input_tokens":3,"output_tokens":4}}),
        ),
        event(
            8,
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel":lattice::components::context_gate::MODEL_CHANNEL,"contextWindow":12345}),
        ),
        event(
            9,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"timer-call","tool":"Schedule","arguments":{"interval_ms":0}}),
        ),
        event(
            10,
            ce::TOOL_EXEC_COMPLETED,
            &[],
            json!({"call":"timer-call","status":"ok","result":{"timer":1}}),
        ),
        event(11, ce::WAKE, &[], json!({"source":"timer:1","body":{}})),
        event(
            12,
            lattice::components::trust_policy::AUTH_REQUESTED,
            &[],
            json!({"held":"held-tool"}),
        ),
        event(13, ce::MODEL_CALL_STARTED, &[], json!({"purpose":null})),
        event(
            14,
            ce::MODEL_CALL_COMPLETED,
            &["event-13"],
            json!({"purpose":null}),
        ),
        event(
            15,
            ce::TOOL_EXEC_COMPLETED,
            &["held-tool"],
            json!({"status":"error"}),
        ),
        event(
            16,
            skill_library::SKILL_LISTING,
            &[],
            json!({"skills":null}),
        ),
    ];
    let expected: Vec<Value> = serde_json::from_slice(include_bytes!(
        "../../../../tests/fixtures/terminal-domain-fold/states.json"
    ))
    .unwrap();
    assert_eq!(expected.len(), events.len());
    for (at, event) in events.iter().enumerate() {
        assert_eq!(
            live.completed_usage(event),
            reference.completed_usage(event)
        );
        assert_eq!(
            historical.completed_usage(event),
            reference.completed_usage(event)
        );
        reference.absorb_state(event, 20, None);
        assert_eq!(
            crate::terminal_host::recovery::reference_snapshot(&reference),
            expected[at],
            "pre-coordinator snapshot after event {}",
            event.seq
        );
        live.absorb(event, 20, None);
        historical.absorb(event, (), None);
        same(&live, &reference);
        same(&historical, &reference);
        if event.seq == 4 {
            assert_eq!(historical.accounting.last_call().unwrap().prompt, 10);
        }
        if event.seq == 5 {
            assert_eq!(historical.title, "second · fixture · suffix");
            assert!(historical.accounting.last_call().is_none());
            assert_eq!(historical.accounting.session_total().calls, 1);
        }
        if event.seq == 7 {
            assert_eq!(historical.accounting.last_call().unwrap().prompt, 123);
        }
    }
    assert_eq!(historical.background.rows()[0].fires, 1);
    assert!(historical.skills.is_empty());
}
