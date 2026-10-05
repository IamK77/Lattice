use super::super::tests::{Answer::*, Script};
use super::*;
use std::collections::VecDeque;

struct Network {
    replies: VecDeque<std::result::Result<Vec<discovery::Model>, String>>,
    requests: Vec<Value>,
}
impl SetupNetwork for Network {
    fn test(&mut self, _: &lattice::models::Entry) -> std::result::Result<(), String> {
        panic!("draft editing must not send generation requests");
    }
    fn models(&mut self, spec: &Value) -> std::result::Result<Vec<discovery::Model>, String> {
        self.requests.push(spec.clone());
        self.replies
            .pop_front()
            .expect("unsolicited model discovery")
    }
}
fn model(id: &str) -> discovery::Model {
    discovery::Model {
        id: id.into(),
        profile: json!({"contextWindow":4096,"maxOutputTokens":1024,"acceptsImages":true}),
        input_limit: None,
    }
}

#[test]
fn effort_checkboxes_preselect_current_rungs_and_save_strength_order() {
    let mut caps = Capabilities::new(
        "synthetic",
        "responses",
        &json!({"contextWindow":1000,"maxOutputTokens":100,"effort":["low","high"]}),
        None,
        "https://example.invalid",
    );
    let mut ui = Script::new([Select(3), Multi(&[6, 2, 4]), Select(0)]);
    assert!(caps.edit(&mut ui, "responses").unwrap());
    assert_eq!(ui.multi_prompts[0].1, [2, 4]);
    assert_eq!(caps.value["effort"], json!(["low", "high", "max"]));
    assert!(ui
        .messages
        .join("\n")
        .contains("not the strength to use now"));
    let mut ui = Script::new([Select(3), Multi(&[]), Select(0)]);
    assert!(caps.edit(&mut ui, "responses").unwrap());
    assert_eq!(ui.multi_prompts[0].1, [2, 4, 6]);
    assert_eq!(caps.value["effort"], json!([]));
    caps.refresh(Capabilities::new(
        "synthetic",
        "responses",
        &json!({"effort":["low","high"]}),
        None,
        "https://example.invalid",
    ));
    assert_eq!(
        caps.value["effort"],
        json!([]),
        "explicitly clearing known rungs survives refresh"
    );
}

#[test]
fn custom_effort_names_keep_their_declared_order_and_remain_editable() {
    let mut caps = Capabilities::new(
        "synthetic",
        "openai",
        &json!({"contextWindow":1000,"maxOutputTokens":100,"effort":["fast","balanced","thorough"]}),
        None,
        "https://example.invalid",
    );
    let mut ui = Script::new([Select(3), Multi(&[2, 0]), Select(0)]);
    assert!(caps.edit(&mut ui, "openai").unwrap());
    assert_eq!(ui.multi_prompts[0].0, ["fast", "balanced", "thorough"]);
    assert_eq!(ui.multi_prompts[0].1, [0, 1, 2]);
    assert_eq!(caps.value["effort"], json!(["fast", "thorough"]));
    let mut ui = Script::new([
        Select(4),
        Select(1),
        Text("quick, quick"),
        Text("quick, balanced, deep"),
        Select(3),
        Multi(&[2, 0]),
        Select(0),
    ]);
    assert!(caps.edit(&mut ui, "openai").unwrap());
    assert_eq!(ui.multi_prompts[0].0, ["quick", "balanced", "deep"]);
    assert_eq!(caps.value["effort"], json!(["quick", "deep"]));
    assert!(ui
        .messages
        .join("\n")
        .contains("distinct provider effort names"));
    assert!(ui.answers.is_empty());
}

#[test]
fn token_suffixes_are_decimal_exact_and_checked() {
    use super::capabilities::parse_tokens;
    for (text, expected) in [
        ("1000000", 1_000_000),
        ("1M", 1_000_000),
        ("1000k", 1_000_000),
        ("1.5m", 1_500_000),
        ("32K", 32_000),
        (".5M", 500_000),
        ("0.000001M", 1),
        ("1048576", 1_048_576),
        (" 1 M ", 1_000_000),
        ("18446744073709551615", u64::MAX),
    ] {
        assert_eq!(parse_tokens(text), Some(expected), "{text}");
    }
    for text in [
        "",
        "0",
        "0M",
        "-1",
        "1.1",
        "0.0000001M",
        "1.2.3M",
        "1Mi",
        "NaN",
        "1e6",
        "1.",
        "18446744073709551616",
        "18446744073709551615M",
    ] {
        assert_eq!(parse_tokens(text), None, "{text}");
    }
    assert_eq!(parse_tokens(&"9".repeat(65)), None);
}

#[test]
fn capability_checkboxes_preserve_defaults_and_update_only_changed_fields() {
    let remote = json!({"contextWindow":1000,"maxOutputTokens":100,"acceptsImages":true});
    let mut caps = Capabilities::new(
        "synthetic",
        "responses",
        &remote,
        None,
        "https://example.invalid",
    );
    let mut ui = Script::new([
        Select(2),
        Multi(&[1, 2]),
        Select(2),
        Multi(&[1, 2]),
        Select(0),
    ]);
    assert!(caps.edit(&mut ui, "responses").unwrap());
    assert_eq!(ui.multi_prompts[0].1, [0]);
    assert_eq!(ui.multi_prompts[1].1, [1, 2]);
    assert_eq!(ui.multi_prompts[0].0.len(), 3);
    assert_eq!(caps.value["acceptsImages"], false);
    assert_eq!(caps.value["nativeWebSearch"], true);
    assert_eq!(caps.value["nativeImageGeneration"], true);
    caps.refresh(Capabilities::new(
        "synthetic",
        "responses",
        &remote,
        None,
        "https://example.invalid",
    ));
    assert_eq!(caps.value["acceptsImages"], false);
    assert_eq!(caps.value["nativeImageGeneration"], true);
    let mut unknown = Capabilities::new(
        "synthetic",
        "openai",
        &json!({"contextWindow":1000,"maxOutputTokens":100}),
        None,
        "https://example.invalid",
    );
    let mut ui = Script::new([Select(2), Multi(&[]), Select(0)]);
    assert!(unknown.edit(&mut ui, "openai").unwrap());
    assert!(ui.multi_prompts[0].1.is_empty());
    assert_eq!(ui.multi_prompts[0].0.len(), 1);
    unknown.refresh(Capabilities::new(
        "synthetic",
        "openai",
        &remote,
        None,
        "https://example.invalid",
    ));
    assert_eq!(
        unknown.value["acceptsImages"], true,
        "confirming an unchanged checkbox must not freeze unknown metadata"
    );
}

#[test]
fn provider_and_protocol_are_distinct_and_only_documented_combinations_are_offered() {
    assert_eq!(
        Provider::OpenAI
            .protocols()
            .iter()
            .map(|p| p.0)
            .collect::<Vec<_>>(),
        ["responses", "openai"]
    );
    assert_eq!(
        Provider::Anthropic
            .protocols()
            .iter()
            .map(|p| p.0)
            .collect::<Vec<_>>(),
        ["anthropic", "openai"]
    );
    assert_eq!(
        Provider::DeepSeek
            .protocols()
            .iter()
            .map(|p| p.0)
            .collect::<Vec<_>>(),
        ["openai", "anthropic", "responses"]
    );
    assert_eq!(
        Provider::DeepSeek.base("anthropic"),
        "https://api.deepseek.com/anthropic"
    );
    assert_eq!(
        Provider::Anthropic.base("openai"),
        "https://api.anthropic.com/v1"
    );
}

#[test]
fn changing_identity_invalidates_capabilities_lists_and_endpoint_credentials() {
    let mut draft = Draft::new();
    draft.connection("responses", "https://example.invalid/v1");
    draft.spec["apiKey"] = json!("SYNTHETIC");
    draft.listed = vec![model("first")];
    draft.model(&model("first"));
    let mut ui = Script::new([Select(2), Multi(&[]), Select(0)]);
    assert!(draft
        .capabilities
        .as_mut()
        .unwrap()
        .edit(&mut ui, "responses")
        .unwrap());
    draft.connection("responses", "https://example.invalid/v1");
    let mut updated = model("first");
    updated.profile["contextWindow"] = json!(8192);
    draft.model(&updated);
    assert_eq!(
        draft.capabilities.as_ref().unwrap().value["contextWindow"],
        8192
    );
    assert_eq!(draft.spec["apiKey"], "SYNTHETIC");
    assert_eq!(
        draft.capabilities.as_ref().unwrap().value["acceptsImages"],
        false
    );
    draft.model(&discovery::Model {
        id: "second".into(),
        profile: json!({}),
        input_limit: None,
    });
    assert_eq!(
        draft.capabilities.as_ref().unwrap().value["acceptsImages"],
        false
    );
    assert!(draft
        .capabilities
        .as_ref()
        .unwrap()
        .value
        .get("contextWindow")
        .is_none());
    draft.connection("anthropic", "https://example.invalid/v1");
    assert!(draft.listed.is_empty());
    assert!(draft.capabilities.is_none());
    assert!(draft.spec.get("model").is_none());
    assert_eq!(draft.spec["apiKey"], "SYNTHETIC");
    draft.connection("anthropic", "https://other.invalid");
    assert!(draft.spec.get("apiKey").is_none());
}

#[cfg(unix)]
#[test]
fn fetched_model_and_user_corrections_survive_review_and_connection_navigation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut network = Network {
        replies: [Ok(vec![model("listed")])].into(),
        requests: vec![],
    };
    let mut ui = Script::new([
        Select(0),
        Select(0),
        Secret,
        Select(0),
        Select(0),
        Select(4),
        Text("chosen"),
        Select(5),
        Select(3),
        Select(2),
        Multi(&[]),
        Select(0),
        Select(1),
        Select(3),
        Select(0),
    ]);
    let result = configure(
        &mut ui,
        &mut network,
        &path,
        &snapshot,
        &mut Session::new(false),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(ui.answers.is_empty());
    assert_eq!(result.spec["adapter"], "responses");
    assert_eq!(result.spec["model"], "listed");
    assert_eq!(result.spec["profile"]["acceptsImages"], false);
    assert_eq!(result.spec["profile"]["contextWindow"], 4096);
    assert_eq!(
        result.spec["profile"]["usageFields"]["input"],
        "input_tokens"
    );
    assert_eq!(network.requests.len(), 1);
    assert_eq!(result.spec["apiKey"], "FAKE_SETUP_KEY_NEVER_PRINT");
    assert!(!ui
        .messages
        .join("\n")
        .contains("FAKE_SETUP_KEY_NEVER_PRINT"));
    assert!(!path.exists(), "draft editing must not write the catalog");
}

#[cfg(unix)]
#[test]
fn failed_and_empty_discovery_allow_manual_input_without_guessed_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut network = Network {
        replies: [Err("synthetic failure".into()), Ok(vec![])].into(),
        requests: vec![],
    };
    let mut ui = Script::new([
        Select(0),
        Text("https://example.invalid/v1"),
        Select(0),
        Secret,
        Select(0),
        Select(0),
        Select(1),
        Text("unlisted"),
        Text("4096"),
        Text("1024"),
        Select(3),
        Select(2),
        Multi(&[]),
        Select(0),
        Select(4),
        Text("manual"),
        Select(5),
        Select(0),
    ]);
    let result = configure(
        &mut ui,
        &mut network,
        &path,
        &snapshot,
        &mut Session::new(true),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(ui.answers.is_empty());
    assert_eq!(network.requests.len(), 2);
    assert_eq!(result.spec["profile"]["acceptsImages"], false);
    assert_eq!(result.spec["model"], "unlisted");
    let messages = ui.messages.join("\n");
    assert!(messages.contains("synthetic failure"));
    assert!(messages.contains("No models are available"));
    assert!(messages.contains("No guessed limits"));
}

#[test]
fn input_limit_is_not_added_to_output_and_unknown_images_stay_disabled() {
    let mut capabilities = Capabilities::new(
        "synthetic-unlisted",
        "responses",
        &json!({"maxOutputTokens":100}),
        Some(1000),
        "https://example.invalid",
    );
    assert_eq!(capabilities.value["contextWindow"], 1000);
    assert_eq!(capabilities.value["acceptsImages"], false);
    assert_eq!(capabilities.value["nativeWebSearch"], false);
    let mut ui = Script::new([Select(2), Multi(&[1]), Select(0)]);
    assert!(capabilities.edit(&mut ui, "responses").unwrap());
    assert_eq!(capabilities.value["nativeWebSearch"], true);
    assert_eq!(capabilities.value["nativeImageGeneration"], false);
    assert!(ui
        .messages
        .join("\n")
        .contains("service input ceiling 1000"));
}

#[test]
fn discovering_a_previously_manual_model_fills_unknown_capabilities() {
    let mut draft = Draft::new();
    draft.connection("responses", "https://example.invalid");
    draft.model(&discovery::Model {
        id: "same".into(),
        profile: json!({}),
        input_limit: None,
    });
    draft.model(&model("same"));
    assert_eq!(
        draft.capabilities.as_ref().unwrap().value["contextWindow"],
        4096
    );
    assert_eq!(
        draft.capabilities.as_ref().unwrap().value["acceptsImages"],
        true
    );
}

#[cfg(unix)]
#[test]
fn replacing_an_account_does_not_keep_the_previous_accounts_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut second = model("shared");
    second.profile["contextWindow"] = json!(8192);
    second.profile["acceptsImages"] = json!(false);
    let mut network = Network {
        replies: [Ok(vec![model("shared")]), Ok(vec![second])].into(),
        requests: vec![],
    };
    let mut ui = Script::new([
        Select(0),
        Select(0),
        Secret,
        Select(0),
        Select(0),
        Select(1),
        Select(0),
        Key("OTHER_SYNTHETIC_KEY"),
        Select(0),
        Select(0),
        Select(0),
    ]);
    let result = configure(
        &mut ui,
        &mut network,
        &path,
        &snapshot,
        &mut Session::new(false),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(network.requests.len(), 2);
    assert_eq!(result.spec["profile"]["contextWindow"], 8192);
    assert_eq!(result.spec["profile"]["acceptsImages"], false);
    assert_eq!(result.spec["apiKey"], "OTHER_SYNTHETIC_KEY");
    assert!(ui.answers.is_empty());
}

#[test]
fn backing_out_of_an_unsubmitted_custom_endpoint_returns_home_safely() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut session = Session::new(true);
    let mut ui = Script::new([Select(0), Back, Back]);
    let mut network = Network {
        replies: VecDeque::new(),
        requests: vec![],
    };
    assert!(
        configure(&mut ui, &mut network, &path, &snapshot, &mut session, None)
            .unwrap()
            .is_none()
    );
    assert!(network.requests.is_empty());
    assert!(ui.answers.is_empty());
}

#[cfg(unix)]
#[test]
fn cancelling_a_review_edit_returns_directly_to_review() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut network = Network {
        replies: [Ok(vec![model("listed")])].into(),
        requests: vec![],
    };
    let mut ui = Script::new([
        Select(0),
        Select(0),
        Secret,
        Select(0),
        Select(0),
        Select(2),
        Back,
        Select(0),
    ]);
    let result = configure(
        &mut ui,
        &mut network,
        &path,
        &snapshot,
        &mut Session::new(false),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.spec["model"], "listed");
    assert_eq!(network.requests.len(), 1);
    assert!(ui.answers.is_empty());
}

#[cfg(unix)]
#[test]
fn repeated_back_from_review_reaches_home_instead_of_cycling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut network = Network {
        replies: [Ok(vec![model("listed")])].into(),
        requests: vec![],
    };
    let mut ui = Script::new([
        Select(0),
        Select(0),
        Secret,
        Select(0),
        Select(0),
        Back,
        Back,
        Back,
        Back,
    ]);
    let result = configure(
        &mut ui,
        &mut network,
        &path,
        &snapshot,
        &mut Session::new(false),
        None,
    )
    .unwrap();
    assert!(result.is_none());
    assert_eq!(network.requests.len(), 1);
    assert!(ui.answers.is_empty());
}

#[cfg(unix)]
#[test]
fn changing_format_keeps_a_custom_endpoint_and_key_but_refreshes_its_model_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut network = Network {
        replies: [Ok(vec![model("listed")]), Ok(vec![model("listed")])].into(),
        requests: vec![],
    };
    let mut ui = Script::new([
        Select(0),
        Text("https://custom.invalid/v1"),
        Select(0),
        Secret,
        Select(0),
        Select(0),
        Select(6),
        Select(1),
        Text("https://custom.invalid/v1"),
        Select(3),
        Select(0),
        Select(0),
        Select(0),
    ]);
    let result = configure(
        &mut ui,
        &mut network,
        &path,
        &snapshot,
        &mut Session::new(true),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ui.text_prompts[1].1, "https://custom.invalid/v1");
    assert_eq!(result.spec["adapter"], "responses");
    assert_eq!(result.spec["apiKey"], "FAKE_SETUP_KEY_NEVER_PRINT");
    assert_eq!(network.requests.len(), 2);
    assert!(ui.answers.is_empty());
}

#[test]
fn final_review_refuses_invalid_limits_and_only_asks_for_the_invalid_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let snapshot = Snapshot::read(&path).unwrap();
    let mut session = Session::new(false);
    session
        .draft
        .connection("responses", "https://example.invalid");
    session.draft.credential("apiKey", "SYNTHETIC");
    session.draft.model(&model("review-model"));
    session.draft.capabilities.as_mut().unwrap().value["maxOutputTokens"] = json!(8192);
    session.step = Step::Review;
    let mut ui = Script::new([Select(0), Text("4096"), Text("1024"), Select(0)]);
    let mut network = Network {
        replies: VecDeque::new(),
        requests: vec![],
    };
    let result = configure(&mut ui, &mut network, &path, &snapshot, &mut session, None)
        .unwrap()
        .unwrap();
    assert_eq!(result.spec["profile"]["contextWindow"], 4096);
    assert_eq!(result.spec["profile"]["maxOutputTokens"], 1024);
    assert_eq!(ui.text_prompts.len(), 2);
    assert!(ui
        .text_prompts
        .iter()
        .all(|(label, _)| *label == ui.label(M::OutputInput)));
    assert!(network.requests.is_empty());
    assert!(ui.answers.is_empty());
}

#[test]
fn automatic_local_names_are_valid_and_do_not_replace_existing_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    std::fs::write(
        &path,
        json!({"models":{"model-v1":{},"model-v1-2":{}}}).to_string(),
    )
    .unwrap();
    let snapshot = Snapshot::read(&path).unwrap();
    assert_eq!(suggested_name(&snapshot, "Model/V1"), "model-v1-3");
    assert_eq!(suggested_name(&snapshot, "/special"), "my-model");
}

#[test]
fn usage_defaults_follow_endpoint_and_wire_not_model_brand() {
    let remote = json!({});
    let anthropic = Capabilities::new(
        "deepseek-flash",
        "anthropic",
        &remote,
        None,
        "https://api.deepseek.com/anthropic",
    );
    assert_eq!(anthropic.value["usageFields"]["input"], "input_tokens");
    let openai = Capabilities::new(
        "unknown",
        "openai",
        &remote,
        None,
        "https://api.openai.com/v1",
    );
    assert_eq!(
        openai.value["usageFields"]["cacheRead"],
        "prompt_tokens_details.cached_tokens"
    );
    let deepseek = Capabilities::new(
        "deepseek-flash",
        "openai",
        &remote,
        None,
        "https://api.deepseek.com",
    );
    assert_eq!(
        deepseek.value["usageFields"]["cacheRead"],
        "prompt_cache_hit_tokens"
    );
}
