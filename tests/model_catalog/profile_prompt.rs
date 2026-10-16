use super::*;
use lattice::{models::Entry, preset};

fn entry(profile: serde_json::Value) -> Entry {
    Entry {
        id: "local-entry".into(),
        adapter: "openai".into(),
        model: "deepseek-v4-flash".into(),
        base_url: "http://127.0.0.1:1".into(),
        key_env: "PROFILE_PROMPT_TEST_KEY".into(),
        profile: Some(profile),
    }
}

#[test]
fn effective_profile_preserves_authored_text_and_inherits_unrelated_defaults() {
    let text = " \nSYNTHETIC {model}\n ";
    let local = entry(json!({"systemPromptSupplement":text}));
    assert_eq!(
        local.system_prompt_supplement().unwrap().as_deref(),
        Some(text)
    );
    assert_eq!(local.profile().unwrap()["contextWindow"], 1_000_000);
    assert_eq!(
        local.profile().unwrap()["usageFields"]["input"],
        "prompt_tokens"
    );
    for profile in [json!({}), json!({"systemPromptSupplement":""})] {
        assert_eq!(entry(profile).system_prompt_supplement().unwrap(), None);
    }
    for bad in [json!(null), json!(false), json!(12), json!([]), json!({})] {
        let problem = entry(json!({"systemPromptSupplement":bad}))
            .system_prompt_supplement()
            .unwrap_err();
        assert!(problem.contains("local-entry"));
        assert!(problem.contains("profile.systemPromptSupplement must be a string"));
    }
}

#[test]
fn saved_entry_identity_selects_the_supplement_even_with_identical_connections() {
    let _serial = alone();
    std::env::set_var("PROFILE_PROMPT_TEST_KEY", "synthetic-not-a-secret");
    let row = |text: &str| {
        json!({"adapter":"openai", "model":"deepseek-v4-flash",
        "baseUrl":"http://127.0.0.1:1", "apiKeyEnv":"PROFILE_PROMPT_TEST_KEY",
        "profile":{"systemPromptSupplement":text}})
    };
    let _dir = staged(
        json!({"models":{"entry-a":row("SYNTHETIC_A"), "entry-b":row("SYNTHETIC_B")}}),
        Some("entry-b"),
    );
    let cfg = PresetConfig::from_env();
    std::env::remove_var("PROFILE_PROMPT_TEST_KEY");
    assert_eq!(cfg.entry_id.as_deref(), Some("entry-b"));
    assert_eq!(preset::running_entry(&cfg).id, "entry-b");
    let serialized = serde_json::to_value(&cfg).unwrap();
    let roundtrip: PresetConfig = serde_json::from_value(serialized.clone()).unwrap();
    assert_eq!(roundtrip.entry_id.as_deref(), Some("entry-b"));
    let mut legacy = serialized;
    legacy.as_object_mut().unwrap().remove("entry_id");
    let legacy: PresetConfig = serde_json::from_value(legacy).unwrap();
    assert!(
        legacy.entry_id.is_none(),
        "older serialized configurations still load"
    );
    assert_eq!(preset::running_entry(&legacy).id, legacy.model);
    let (_, _, assembly) = preset::standard(&cfg).unwrap();
    assert_eq!(
        assembly.instances["ctx"].config.as_ref().unwrap()["profile"]["systemPromptSupplement"],
        "SYNTHETIC_B"
    );
}

#[test]
fn malformed_selected_supplements_fail_assembly_instead_of_silently_disappearing() {
    let _serial = alone();
    for bad in [json!(null), json!(false), json!(12), json!([]), json!({})] {
        std::env::set_var("PROFILE_PROMPT_TEST_KEY", "synthetic-not-a-secret");
        let _dir = staged(
            json!({"models":{"invalid-entry":{
                "adapter":"openai", "model":"deepseek-v4-flash", "baseUrl":"http://127.0.0.1:1",
                "apiKeyEnv":"PROFILE_PROMPT_TEST_KEY", "profile":{"systemPromptSupplement":bad}
            }}}),
            Some("invalid-entry"),
        );
        let cfg = PresetConfig::from_env();
        std::env::remove_var("PROFILE_PROMPT_TEST_KEY");
        let problem = preset::standard(&cfg)
            .err()
            .expect("invalid local text must be rejected");
        assert!(problem.contains("invalid-entry"));
        assert!(problem.contains("profile.systemPromptSupplement must be a string"));
    }
}
