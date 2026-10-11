use super::super::providers::{manual_models, PROVIDERS};
use super::*;

#[test]
fn presets_have_unique_https_targets_and_only_intended_protocols() {
    let mut names = std::collections::HashSet::new();
    let mut targets = std::collections::HashSet::new();
    assert_eq!(PROVIDERS.len(), 15);
    for provider in PROVIDERS {
        assert!(names.insert(provider.en));
        assert!(!provider.zh.is_empty());
        assert!(!provider.protocols.is_empty());
        for (index, service) in provider.services.iter().enumerate() {
            assert!(targets.insert(service.base));
            assert!(service.base.starts_with("https://"));
            assert!(!service.en.is_empty() && !service.zh.is_empty());
            for (adapter, _) in provider.protocols {
                let base = provider.base(index, adapter);
                validate_target(&json!({"adapter":adapter,"baseUrl":base,"model":"synthetic"}))
                    .unwrap();
                if service.discovery {
                    discovery::endpoint(&json!({"adapter":adapter,"baseUrl":base})).unwrap();
                }
            }
        }
    }
    assert_eq!(
        PROVIDERS[3].services[0].base,
        "https://generativelanguage.googleapis.com/v1beta/openai"
    );
    assert_eq!(
        PROVIDERS[4]
            .services
            .iter()
            .map(|s| s.base)
            .collect::<Vec<_>>(),
        [
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            "https://dashscope-us.aliyuncs.com/compatible-mode/v1",
        ]
    );
    assert_eq!(PROVIDERS[7].services[0].base, "https://api.minimax.cn/v1");
    assert_eq!(
        PROVIDERS[8].services[0].base,
        "https://ark.cn-beijing.volces.com/api/v3"
    );
    assert_eq!(
        PROVIDERS[13].services[0].base,
        "https://api.groq.com/openai/v1"
    );
    for provider in &PROVIDERS[3..] {
        assert_eq!(provider.protocols, &[("openai", "OpenAI Chat Completions")]);
    }
}

#[test]
fn new_discovery_presets_pin_exact_paths_and_bearer_authentication() {
    for (name, service, expected) in [
        (
            "Google Gemini",
            0,
            "https://generativelanguage.googleapis.com/v1beta/openai/models",
        ),
        ("Moonshot / Kimi", 0, "https://api.moonshot.cn/v1/models"),
        ("Moonshot / Kimi", 1, "https://api.moonshot.ai/v1/models"),
        ("SiliconFlow", 0, "https://api.siliconflow.cn/v1/models"),
        ("SiliconFlow", 1, "https://api.siliconflow.com/v1/models"),
        ("OpenRouter", 0, "https://openrouter.ai/api/v1/models"),
        ("xAI", 0, "https://api.x.ai/v1/models"),
        ("xAI", 1, "https://us.api.x.ai/v1/models"),
        ("Mistral", 0, "https://api.mistral.ai/v1/models"),
        ("Mistral", 1, "https://api.eu.mistral.ai/v1/models"),
        ("Mistral", 2, "https://api.us.mistral.ai/v1/models"),
        ("Groq", 0, "https://api.groq.com/openai/v1/models"),
        ("Together AI", 0, "https://api.together.ai/v1/models"),
    ] {
        let provider = PROVIDERS.iter().find(|p| p.en == name).unwrap();
        assert!(provider.services[service].discovery);
        let (url, anthropic) = discovery::endpoint(
            &json!({"adapter":provider.protocols[0].0,"baseUrl":provider.services[service].base}),
        )
        .unwrap();
        assert_eq!(url.as_str(), expected);
        assert!(!anthropic);
    }
}

#[test]
fn manual_discovery_is_exact_endpoint_scoped_not_a_hostname_substring() {
    assert!(manual_models("https://api.minimax.cn/v1/"));
    assert!(manual_models(
        "https://dashscope.aliyuncs.com/compatible-mode/v1"
    ));
    for base in [
        "https://example.invalid/v1",
        "https://api.minimax.cn/v1/proxy",
        "https://api.minimax.cn.evil.invalid/v1",
        "https://api.moonshot.cn/v1",
    ] {
        assert!(!manual_models(base));
    }
}

#[cfg(unix)]
#[test]
fn every_service_can_be_saved_in_both_languages_without_a_network_request() {
    use Answer::*;
    for language in [Language::English, Language::Chinese] {
        for (index, provider) in PROVIDERS.iter().enumerate() {
            for (region, service) in provider.services.iter().enumerate() {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("models.json");
                let prefs = dir.path().join("preferences.json");
                let mut answers = vec![Select(0), Select(index)];
                if provider.services.len() > 1 {
                    answers.push(Select(region));
                }
                answers.extend([
                    Select(0),
                    Secret,
                    Select(usize::from(service.discovery)),
                    Text("synthetic-provider-model"),
                    Text("32k"),
                    Text("4k"),
                    Select(0),
                    Select(0),
                ]);
                let mut ui = Script::new(answers);
                ui.language = language;
                let mut network = Probe {
                    calls: 0,
                    fail: false,
                };
                let result = guide(cfg(), &path, Some(&prefs), &mut ui, &mut network).unwrap();
                assert_eq!(
                    result.model, "synthetic-provider-model",
                    "{} / {}",
                    provider.en, service.en
                );
                let saved = Snapshot::read(&path).unwrap();
                let entry = saved.entry("synthetic-provider-model").unwrap();
                assert_eq!(entry["baseUrl"], service.base);
                assert_eq!(entry["adapter"], provider.protocols[0].0);
                assert_eq!(entry["profile"]["contextWindow"], 32_000);
                assert_eq!(entry["profile"]["maxOutputTokens"], 4_000);
                assert_eq!(network.calls, 0);
                assert!(ui.answers.is_empty());
                assert!(!ui
                    .messages
                    .join("\n")
                    .contains("FAKE_SETUP_KEY_NEVER_PRINT"));
                assert!(!dir.path().join("ledgers").exists());
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn switching_region_discards_credentials_and_model_but_back_to_same_region_keeps_them() {
    use Answer::*;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let mut ui = Script::new([
        Select(0),
        Select(5),
        Select(0),
        Select(0),
        Key("SYNTHETIC_CHINA_ONLY"),
        Back,
        Back,
        Select(0),
        Select(3), // Same region retains the credential.
        Back,
        Back,
        Select(1), // Another region must ask for its own credential.
        Select(0),
        Key("SYNTHETIC_GLOBAL_ONLY"),
        Select(1),
        Text("synthetic-region-model"),
        Text("32k"),
        Text("4k"),
        Select(0),
        Select(0),
    ]);
    let mut network = Probe {
        calls: 0,
        fail: false,
    };
    guide(
        cfg(),
        &path,
        Some(&dir.path().join("preferences.json")),
        &mut ui,
        &mut network,
    )
    .unwrap();
    let saved = Snapshot::read(&path).unwrap();
    let entry = saved.entry("synthetic-region-model").unwrap();
    assert_eq!(entry["baseUrl"], "https://api.moonshot.ai/v1");
    assert_eq!(entry["apiKey"], "SYNTHETIC_GLOBAL_ONLY");
    let menus = ui
        .selections
        .iter()
        .filter(|(prompt, _)| *prompt == ui.label(M::CredentialMenu))
        .map(|(_, options)| options)
        .collect::<Vec<_>>();
    assert!(menus
        .iter()
        .any(|options| options.contains(&ui.label(M::UseConnection))));
    assert!(
        !menus.last().unwrap().contains(&ui.label(M::UseConnection)),
        "the old region's credential must not be offered at the new endpoint"
    );
    assert!(!std::fs::read_to_string(path)
        .unwrap()
        .contains("SYNTHETIC_CHINA_ONLY"));
    assert_eq!(network.calls, 0);
    assert!(ui.answers.is_empty());
}
