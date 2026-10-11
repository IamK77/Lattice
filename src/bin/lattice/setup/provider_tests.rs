use super::super::providers::{manual_models, PROVIDERS};
use super::*;

#[test]
fn preset_default_label_does_not_claim_a_provider_recommendation() {
    for (language, expected) in [
        (Language::English, "Chat (Lattice default)"),
        (Language::Chinese, "Chat（Lattice 默认）"),
    ] {
        let mut ui = Script::new([]);
        ui.language = language;
        assert_eq!(ui.message(M::Recommended, &["Chat"]), expected);
    }
}

#[test]
fn documented_protocols_and_lattice_defaults_are_pinned_per_service() {
    // The first entry is the default. Regional support is not inferred from a sibling.
    let expected: &[&[&str]] = &[
        &["responses,openai"],
        &["anthropic,openai"],
        &["openai,anthropic,responses"],
        &["openai"],
        &[
            "openai,anthropic,responses",
            "openai,anthropic,responses",
            "openai,anthropic,responses",
        ],
        &["openai,anthropic,responses", "openai,anthropic,responses"],
        &["openai,responses", "openai"],
        &["anthropic,openai,responses", "anthropic,openai,responses"],
        &["responses,openai,anthropic"],
        &["openai,anthropic", "openai,anthropic"],
        &["openai,anthropic,responses"],
        &["responses,openai", "responses"],
        &["openai", "openai", "openai"],
        &["openai,responses"],
        &["openai"],
    ];
    assert_eq!(PROVIDERS.len(), expected.len());
    for (provider, formats) in PROVIDERS.iter().zip(expected) {
        let actual: Vec<_> = provider
            .services
            .iter()
            .map(|service| {
                service
                    .routes
                    .iter()
                    .map(|route| route.adapter)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect();
        assert_eq!(&actual, formats, "{}", provider.en);
    }
}

#[test]
fn alternative_generation_paths_are_independent_of_model_listing_paths() {
    let messages = [
        "https://api.anthropic.com/v1/messages",
        "https://api.deepseek.com/anthropic/v1/messages",
        "https://dashscope.aliyuncs.com/apps/anthropic/v1/messages",
        "https://dashscope-intl.aliyuncs.com/apps/anthropic/v1/messages",
        "https://dashscope-us.aliyuncs.com/apps/anthropic/v1/messages",
        "https://api.moonshot.cn/anthropic/v1/messages",
        "https://api.moonshot.ai/anthropic/v1/messages",
        "https://api.minimax.cn/anthropic/v1/messages",
        "https://api.minimax.io/anthropic/v1/messages",
        "https://ark.cn-beijing.volces.com/api/compatible/v1/messages",
        "https://api.siliconflow.cn/v1/messages",
        "https://api.siliconflow.com/v1/messages",
        "https://openrouter.ai/api/v1/messages",
    ];
    let responses = [
        "https://api.openai.com/v1/responses",
        "https://api.deepseek.com/responses",
        "https://dashscope.aliyuncs.com/compatible-mode/v1/responses",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/responses",
        "https://dashscope-us.aliyuncs.com/compatible-mode/v1/responses",
        "https://api.moonshot.cn/v1/responses",
        "https://api.moonshot.ai/v1/responses",
        "https://open.bigmodel.cn/api/v1/responses",
        "https://api.minimax.cn/v1/responses",
        "https://api.minimax.io/v1/responses",
        "https://ark.cn-beijing.volces.com/api/v3/responses",
        "https://openrouter.ai/api/v1/responses",
        "https://api.x.ai/v1/responses",
        "https://us.api.x.ai/v1/responses",
        "https://api.groq.com/openai/v1/responses",
    ];
    for (adapter, expected) in [
        ("anthropic", messages.as_slice()),
        ("responses", responses.as_slice()),
    ] {
        let actual: Vec<_> = PROVIDERS
            .iter()
            .flat_map(|p| p.services)
            .flat_map(|s| s.routes)
            .filter(|r| r.adapter == adapter)
            .map(|r| {
                if adapter == "anthropic" {
                    lattice::components::model_compat::messages_url(r.base)
                } else {
                    format!("{}/responses", r.base)
                }
            })
            .collect();
        assert_eq!(actual, expected, "{adapter}");
    }
}

#[test]
fn presets_have_unique_https_targets_and_only_intended_protocols() {
    let mut names = std::collections::HashSet::new();
    let mut targets = std::collections::HashSet::new();
    assert_eq!(PROVIDERS.len(), 15);
    for provider in PROVIDERS {
        assert!(names.insert(provider.en));
        assert!(!provider.zh.is_empty());
        for service in provider.services {
            assert!(!service.routes.is_empty());
            assert!(!service.en.is_empty() && !service.zh.is_empty());
            let mut adapters = std::collections::HashSet::new();
            for route in service.routes {
                assert!(targets.insert((route.adapter, route.base)));
                assert!(adapters.insert(route.adapter));
                assert!(route.base.starts_with("https://"));
                let spec =
                    json!({"adapter":route.adapter,"baseUrl":route.base,"model":"synthetic"});
                validate_target(&spec).unwrap();
                assert_eq!(manual_models(&spec), route.models.is_none());
                match route.models {
                    Some((expected, native)) => {
                        let (url, actual_native) = discovery::endpoint(&spec).unwrap();
                        assert_eq!(url.as_str(), expected);
                        assert_eq!(actual_native, native);
                        assert_eq!(
                            url.host_str(),
                            reqwest::Url::parse(route.base).unwrap().host_str()
                        );
                    }
                    None => assert!(discovery::endpoint(&spec).is_err()),
                }
            }
        }
    }
    for (provider, service, expected) in [
        (
            3,
            0,
            "https://generativelanguage.googleapis.com/v1beta/openai",
        ),
        (4, 0, "https://dashscope.aliyuncs.com/compatible-mode/v1"),
        (
            4,
            1,
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        ),
        (4, 2, "https://dashscope-us.aliyuncs.com/compatible-mode/v1"),
        (7, 0, "https://api.minimax.cn/v1"),
        (8, 0, "https://ark.cn-beijing.volces.com/api/v3"),
        (13, 0, "https://api.groq.com/openai/v1"),
    ] {
        let route = PROVIDERS[provider].services[service]
            .routes
            .iter()
            .find(|r| r.adapter == "openai")
            .unwrap();
        assert_eq!(route.base, expected);
    }
}

#[test]
fn absolute_dns_names_keep_catalog_auth_without_changing_saved_addresses() {
    let spec =
        json!({"adapter":"openai","baseUrl":"https://API.ANTHROPIC.COM./v1/","model":"synthetic"});
    let original = spec.clone();
    let (url, native) = discovery::endpoint(&spec).unwrap();
    assert_eq!(url.as_str(), "https://api.anthropic.com/v1/models");
    assert!(native);
    assert_eq!(spec, original);
    for base in [
        "https://api.anthropic.com.evil.invalid/v1",
        "https://api.anthropic.com/custom/v1",
    ] {
        assert!(super::super::providers::selected_route(
            &json!({"adapter":"openai","baseUrl":base})
        )
        .is_none());
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
        ("MiniMax", 0, "https://api.minimax.cn/v1/models"),
        ("MiniMax", 1, "https://api.minimax.io/v1/models"),
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
        for route in provider.services[service].routes {
            assert!(route.models.is_some());
            let (url, anthropic) =
                discovery::endpoint(&json!({"adapter":route.adapter,"baseUrl":route.base}))
                    .unwrap();
            assert_eq!(url.as_str(), expected);
            assert!(!anthropic);
        }
    }
}

#[test]
fn manual_discovery_is_exact_endpoint_scoped_not_a_hostname_substring() {
    for (adapter, base) in [
        ("responses", "https://ark.cn-beijing.volces.com/api/v3/"),
        (
            "anthropic",
            "https://ark.cn-beijing.volces.com/api/compatible",
        ),
        (
            "openai",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
        ),
    ] {
        assert!(manual_models(&json!({"adapter":adapter,"baseUrl":base})));
    }
    for base in [
        "https://example.invalid/v1",
        "https://api.minimax.cn/v1/proxy",
        "https://api.minimax.cn.evil.invalid/v1",
        "https://api.moonshot.cn/v1",
    ] {
        assert!(!manual_models(&json!({"adapter":"openai","baseUrl":base})));
    }
}

#[cfg(unix)]
#[test]
fn every_service_can_be_saved_in_both_languages_without_a_network_request() {
    use Answer::*;
    for language in [Language::English, Language::Chinese] {
        for (index, provider) in PROVIDERS.iter().enumerate() {
            for (region, service) in provider.services.iter().enumerate() {
                for (format, route) in service.routes.iter().enumerate() {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("models.json");
                    let prefs = dir.path().join("preferences.json");
                    let mut answers = vec![Select(0), Select(index)];
                    if provider.services.len() > 1 {
                        answers.push(Select(region));
                    }
                    if format > 0 {
                        answers.extend([Select(2), Select(format), Text(route.base)]);
                    }
                    answers.extend([
                        Select(0),
                        Secret,
                        Select(usize::from(route.models.is_some())),
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
                    assert_eq!(entry["baseUrl"], route.base);
                    assert_eq!(entry["adapter"], route.adapter);
                    if format > 0 {
                        let endpoint = ui.label(M::Endpoint);
                        let (_, default) = ui
                            .text_prompts
                            .iter()
                            .find(|(label, _)| *label == endpoint)
                            .unwrap();
                        assert_eq!(
                            default, route.base,
                            "switching formats must select its endpoint before user editing"
                        );
                        let (_, choices) = ui
                            .selections
                            .iter()
                            .find(|(label, _)| *label == ui.label(M::Protocol))
                            .unwrap();
                        assert_eq!(choices.len(), service.routes.len() + 1);
                    }
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
