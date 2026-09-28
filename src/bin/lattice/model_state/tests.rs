use super::*;
use lattice::view::ModelRow;
use serde_json::{json, Value};

fn event(kind: &str, payload: Value) -> EventEnvelope {
    serde_json::from_value(json!({
        "v":1, "id":"ev_1_model", "seq":1, "stream":"model-test",
        "time":"2026-09-19T00:00:00Z", "type":kind,
        "source":"test", "causes":[], "payload":payload
    }))
    .unwrap()
}

fn swap(config: Value) -> EventEnvelope {
    event(
        ce::COMPONENT_REPLACED,
        json!({
            "instance":preset::MAIN_MODEL, "to":"unknown-model", "config":config
        }),
    )
}

fn seeded() -> ModelState {
    let rows = [("a.invalid/v1", "A"), ("b.invalid/v1", "B")]
        .into_iter()
        .map(|(endpoint, key_env)| ModelRow {
            id: key_env.into(),
            model: "shared".into(),
            endpoint: endpoint.into(),
            key_env: key_env.into(),
            rungs: vec![format!("catalog-{key_env}")],
            ..ModelRow::default()
        })
        .collect();
    ModelState::new(
        ModelView { rows, now: Some(1) },
        Entry {
            id: "old-id".into(),
            adapter: "scripted".into(),
            model: "old".into(),
            base_url: "https://old.invalid/v1".into(),
            key_env: "OLD".into(),
            profile: Some(json!({"contextWindow":1000})),
        },
        EffortView {
            rungs: vec!["old-rung".into()],
            now: Some("local-effort".into()),
        },
        Some(999),
    )
}

#[test]
fn swap_matching_keeps_legacy_wildcards_and_endpoint_paths() {
    for (config, expected) in [
        (json!({"model":"shared"}), Some(0)),
        (
            json!({"model":"shared","baseUrl":"","apiKeyEnv":""}),
            Some(0),
        ),
        (
            json!({"model":"shared","baseUrl":"https://b.invalid/v1/"}),
            Some(1),
        ),
        (json!({"model":"shared","apiKeyEnv":"B"}), Some(1)),
        (
            json!({"model":"shared","baseUrl":"https://b.invalid/v2"}),
            None,
        ),
        (
            json!({"model":"shared","baseUrl":"https://a.invalid/v1","apiKeyEnv":"B"}),
            None,
        ),
        (json!({"model":"outside-catalog"}), None),
    ] {
        let mut state = seeded();
        let event = swap(config.clone());
        assert_eq!(state.observe_swap(&event), config["model"].as_str());
        assert_eq!(state.catalog().now, expected, "{config}");
        assert_eq!(
            state.running().adapter,
            "scripted",
            "unknown component only preserves adapter"
        );
        assert_eq!(state.running().model, config["model"].as_str().unwrap());
        assert_eq!(state.effort().now.as_deref(), Some("local-effort"));
        assert_eq!(state.effective_window(), Some(999));
    }
    assert_eq!(endpoint_host("https://b.invalid/v1///"), "b.invalid/v1");
}

#[test]
fn absent_and_explicit_fields_do_not_share_a_fallback() {
    let mut absent = seeded();
    absent.observe_swap(&swap(json!({"model":"shared"})));
    assert_eq!(absent.running().id, "shared");
    assert!(absent.running().profile.is_none());
    assert_eq!(absent.running().base_url, "https://old.invalid/v1");
    assert_eq!(absent.running().key_env, "OLD");
    assert_eq!(absent.effort().rungs, ["catalog-A"]);

    let mut explicit = seeded();
    explicit.observe_swap(&swap(json!({"model":"shared", "entryId":"selected",
        "profile":null, "baseUrl":"", "apiKeyEnv":""})));
    assert_eq!(explicit.running().id, "selected");
    assert!(explicit.running().profile.is_none());
    assert_eq!(explicit.running().base_url, "");
    assert_eq!(explicit.running().key_env, "");
    assert_eq!(explicit.effort().rungs, explicit.running().effort_rungs());
    assert_ne!(explicit.effort().rungs, absent.effort().rungs);
    assert_eq!(explicit.effective_window(), Some(999));

    explicit.observe_swap(&swap(
        json!({"model":"new", "profile":{"contextWindow":1234}}),
    ));
    assert_eq!(explicit.effective_window(), Some(1234));
}

#[test]
fn window_channel_and_swap_filters_leave_other_state_alone() {
    let mut state = seeded();
    for event in [
        event(
            ce::EXTERNAL_INPUT,
            json!({"instance":preset::MAIN_MODEL,"config":{"model":"bad"}}),
        ),
        event(
            ce::COMPONENT_REPLACED,
            json!({"instance":"other","config":{"model":"bad"}}),
        ),
        swap(json!({"model":null})),
        swap(json!({})),
    ] {
        assert!(state.observe_swap(&event).is_none());
        assert_eq!(state.running().model, "old");
        assert_eq!(state.catalog().now, Some(1));
        assert_eq!(state.effort().rungs, ["old-rung"]);
        assert_eq!(state.effective_window(), Some(999));
    }
    for (kind, channel, value, expected) in [
        (
            ce::EXTERNAL_INPUT,
            context_gate::MODEL_CHANNEL,
            json!(4321),
            4321,
        ),
        (
            ce::EXTERNAL_INPUT,
            context_gate::MODEL_CHANNEL,
            json!(null),
            4321,
        ),
        (
            ce::EXTERNAL_INPUT,
            context_gate::MODEL_CHANNEL,
            json!(-1),
            4321,
        ),
        (
            ce::EXTERNAL_INPUT,
            context_gate::MODEL_CHANNEL,
            json!("50"),
            4321,
        ),
        (ce::EXTERNAL_INPUT, "other", json!(50), 4321),
        (
            ce::USER_MESSAGE,
            context_gate::MODEL_CHANNEL,
            json!(50),
            4321,
        ),
        (ce::EXTERNAL_INPUT, context_gate::MODEL_CHANNEL, json!(0), 0),
    ] {
        state.observe_window(&event(
            kind,
            json!({"channel":channel,"contextWindow":value}),
        ));
        assert_eq!(state.effective_window(), Some(expected));
        assert_eq!(state.running().model, "old");
        assert_eq!(state.effort().now.as_deref(), Some("local-effort"));
    }
}
