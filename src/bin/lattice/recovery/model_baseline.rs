use super::*;
use serde_json::json;

fn initial_ui() -> Ui {
    let mut ui = Ui::replayed(&[]);
    ui.domain.title = "fixture-model · /fixture-workspace".into();
    ui.domain.stream_id = "fixture-model-stream".into();
    ui.domain.expert_dir = Some(std::path::PathBuf::from("/fixture-experts"));
    *ui.domain.model.fixture_running() = lattice::models::Entry {
        id: "active".into(),
        adapter: "openai".into(),
        model: "fixture-model".into(),
        base_url: "https://active.example.invalid/v1".into(),
        key_env: "FIXTURE_ACTIVE_KEY".into(),
        profile: Some(json!({"contextWindow":111111})),
    };
    *ui.domain.model.fixture_catalog() = view::ModelView {
        rows: vec![
            view::ModelRow {
                id: "other".into(),
                model: "other-model".into(),
                endpoint: "other.example.invalid".into(),
                dialect: "anthropic".into(),
                window: Some(222222),
                rungs: vec!["other-rung".into()],
                accepts_images: false,
                key_env: "FIXTURE_OTHER_KEY".into(),
                key_present: false,
            },
            view::ModelRow {
                id: "active".into(),
                model: "fixture-model".into(),
                endpoint: "active.example.invalid".into(),
                dialect: "openai".into(),
                window: Some(111111),
                rungs: vec!["quiet".into(), "deep".into()],
                accepts_images: true,
                key_env: "FIXTURE_ACTIVE_KEY".into(),
                key_present: true,
            },
        ],
        now: Some(1),
    };
    *ui.domain.model.fixture_effort() = view::EffortView {
        rungs: vec!["quiet".into(), "deep".into()],
        now: Some("deep".into()),
    };
    // Deliberately not the profile window: the effective value is independent.
    *ui.domain.model.fixture_window() = Some(99999);
    ui
}

#[test]
fn initial_model_identity_preserves_the_pre_refactor_serialized_bytes() {
    let initial = Initial::of(&initial_ui());
    let old = include_bytes!("../../../../tests/fixtures/terminal-model-state-v4/initial.json");
    assert_eq!(
        serde_json::to_string(&initial).unwrap(),
        std::str::from_utf8(old).unwrap()
    );
}

#[test]
fn initial_projection_keeps_all_model_and_path_interpretation_fields() {
    let ui = initial_ui();
    let log = lattice::EventLog::open(
        lattice::core_events::core_event_decls(),
        "fixture-model-stream",
        None,
    )
    .unwrap();
    let pure = Initial::of(&ui).projection(&log.reader()).unwrap();
    assert_eq!(pure.expert_dir, ui.domain.expert_dir);
    assert_eq!(pure.stream_id, ui.domain.stream_id);
    assert_eq!(
        serde_json::to_value(State::capture(&pure)).unwrap(),
        serde_json::to_value(State::reference_capture(&ui.domain)).unwrap()
    );
}

#[test]
fn v4_model_fields_restore_from_the_pre_refactor_document() {
    let old = include_bytes!("../../../../tests/fixtures/terminal-model-state-v4/state.json");
    let saved: State = serde_json::from_slice(old).unwrap();
    let mut restored = Ui::replayed(&[]);
    saved.apply(&mut restored.domain, SETTLED_TICK).unwrap();
    let expected = initial_ui();
    assert_eq!(restored.domain.title, expected.domain.title);
    assert_eq!(
        restored.domain.model.running(),
        expected.domain.model.running()
    );
    assert_eq!(
        serde_json::to_value(restored.domain.model.catalog()).unwrap(),
        serde_json::to_value(expected.domain.model.catalog()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(restored.domain.model.effort()).unwrap(),
        serde_json::to_value(expected.domain.model.effort()).unwrap()
    );
    assert_eq!(restored.domain.model.effective_window(), Some(99999));
    assert_eq!(restored.domain.model.catalog().now, Some(1));
    assert_eq!(VERSION, 5);
}

#[test]
fn restoring_model_facts_preserves_local_controls_but_does_not_recreate_them() {
    let old = include_bytes!("../../../../tests/fixtures/terminal-model-state-v4/state.json");
    let mut live = initial_ui();
    live.controls.open_dial(live.domain.model.effort());
    live.controls.open_form();
    live.controls.paste_form("local-only");
    live.controls.form_problem("keep this problem".into());
    live.controls.ask_delete("local-confirmation".into());
    let dial = live.controls.dial();
    serde_json::from_slice::<State>(old)
        .unwrap()
        .apply(&mut live.domain, SETTLED_TICK)
        .unwrap();
    assert_eq!(live.controls.dial(), dial);
    assert_eq!(live.controls.form().unwrap().values[0], "local-only");
    assert_eq!(
        live.controls.form().unwrap().problem.as_deref(),
        Some("keep this problem")
    );
    assert_eq!(live.controls.deletion(), Some("local-confirmation"));
    let mut fresh = Ui::replayed(&[]);
    serde_json::from_slice::<State>(old)
        .unwrap()
        .apply(&mut fresh.domain, SETTLED_TICK)
        .unwrap();
    assert_eq!(fresh.controls.dial(), None);
    assert!(fresh.controls.form().is_none());
    assert_eq!(fresh.controls.deletion(), None);
}
