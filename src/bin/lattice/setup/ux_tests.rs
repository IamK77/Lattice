//! Interaction regressions: count required answers, exercise Back, and retain drafts.
use super::*;
use Answer::*;

#[cfg(unix)]
#[test]
fn default_path_has_eight_answers_and_only_one_review_in_either_language() {
    for language in [Language::English, Language::Chinese] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        let preferences = dir.path().join("preferences.json");
        let mut ui = Script::new([
            Select(0),
            Select(2),
            Select(0),
            Secret,
            Select(1),
            Text("deepseek-flash"),
            Select(0),
            Select(0),
        ]);
        ui.language = language;
        let mut network = Probe {
            calls: 0,
            fail: false,
        };
        let result = guide(cfg(), &path, Some(&preferences), &mut ui, &mut network).unwrap();
        assert_eq!(result.model, "deepseek-flash");
        assert_eq!(
            ui.prompts,
            [
                M::Home,
                M::Provider,
                M::CredentialMenu,
                M::ApiKey,
                M::ModelMenu,
                M::ModelIdentifier,
                M::Review,
                M::TestMenu
            ]
            .map(|id| ui.label(id))
        );
        assert_eq!(network.calls, 0);
        assert!(ui.answers.is_empty());
        let prefs = lattice::preferences::load_from(&preferences);
        assert_eq!(prefs["model"], "deepseek-flash");
        assert!(
            prefs.get("setupLanguage").is_none(),
            "automatic language must not become a standing preference"
        );
    }
}

#[cfg(unix)]
#[test]
fn switching_language_on_review_keeps_the_entire_draft_and_network_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let preferences = dir.path().join("preferences.json");
    std::fs::write(
        &preferences,
        json!({"thinking":"high","unknown":"keep"}).to_string(),
    )
    .unwrap();
    let mut answers = first_steps(false);
    answers.pop();
    answers.extend([
        Select(8),
        Select(1),
        Select(4),
        Text("cn-saved"),
        Select(0),
        Select(0),
    ]);
    let mut ui = Script::new(answers);
    let mut network = Probe {
        calls: 0,
        fail: false,
    };
    let result = guide(cfg(), &path, Some(&preferences), &mut ui, &mut network).unwrap();
    assert_eq!(result.model, "deepseek-flash");
    assert_eq!(ui.language, Language::Chinese);
    assert_eq!(network.calls, 0);
    assert!(ui.answers.is_empty());
    assert_eq!(
        ui.prompts
            .iter()
            .filter(|s| **s == i18n::text(Language::English, M::ApiKey, &[]))
            .count(),
        1
    );
    assert!(ui
        .prompts
        .contains(&i18n::text(Language::Chinese, M::LocalName, &[])));
    let saved = Snapshot::read(&path).unwrap();
    assert_eq!(
        saved.entry("cn-saved").unwrap()["apiKey"],
        "FAKE_SETUP_KEY_NEVER_PRINT"
    );
    assert_eq!(
        lattice::preferences::load_from(&preferences),
        json!({"thinking":"high","unknown":"keep","setupLanguage":"zh-CN"})
    );
    assert!(!ui
        .messages
        .join("\n")
        .contains("FAKE_SETUP_KEY_NEVER_PRINT"));
}

#[test]
fn explicit_language_survives_cancel_but_malformed_preferences_are_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let preferences = dir.path().join("preferences.json");
    let mut ui = Script::new([Select(2), Select(1), Cancel]);
    let mut network = Probe {
        calls: 0,
        fail: false,
    };
    assert!(matches!(
        guide(cfg(), &path, Some(&preferences), &mut ui, &mut network),
        Err(Error::Cancelled)
    ));
    assert_eq!(
        lattice::preferences::load_from(&preferences)["setupLanguage"],
        "zh-CN"
    );
    assert!(!path.exists());
    assert!(!dir.path().join("ledgers").exists());
    std::fs::write(&preferences, "not valid JSON").unwrap();
    let mut ui = Script::new([Select(1)]);
    change_language(&mut ui, Some(&preferences)).unwrap();
    assert_eq!(ui.language, Language::Chinese);
    assert_eq!(
        std::fs::read_to_string(&preferences).unwrap(),
        "not valid JSON"
    );
    assert!(ui
        .messages
        .join("\n")
        .contains(ui.message(M::LanguageNotSaved, &[""]).trim_end()));
}

#[cfg(unix)]
#[test]
fn back_and_unchanged_connection_keep_completed_fields_without_reentering_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let mut ui = Script::new([
        Select(0),
        Select(2),
        Select(0),
        Secret,
        Back,
        Select(3),
        Select(1),
        Text("deepseek-flash"),
        Select(3),
        Select(1),
        Text("1M"),
        Back,
        Select(0),
        Select(1),
        Select(2),
        Select(0),
        Text("https://api.deepseek.com"),
        Select(3),
        Select(5),
        Select(0),
        Select(0),
    ]);
    let mut network = Probe {
        calls: 0,
        fail: false,
    };
    let result = guide(cfg(), &path, None, &mut ui, &mut network).unwrap();
    assert_eq!(result.profile.unwrap()["contextWindow"], 1_000_000);
    assert_eq!(
        ui.prompts
            .iter()
            .filter(|s| **s == ui.label(M::ApiKey))
            .count(),
        1
    );
    assert!(ui.answers.is_empty());
    assert_eq!(network.calls, 0);
}

#[cfg(unix)]
#[test]
fn credential_repair_keeps_its_unsaved_key_across_home_and_language_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let preferences = dir.path().join("preferences.json");
    let original = json!({"models":{"repair-me":{"adapter":"openai","model":"synthetic","baseUrl":"https://example.invalid","apiKey":"[redacted]","profile":{"custom":"preserve"},"note":"keep"}},"unknown":"keep"});
    std::fs::write(&path, original.to_string()).unwrap();
    std::fs::write(&preferences, json!({"model":"repair-me"}).to_string()).unwrap();
    let mut ui = Script::new([
        Select(2),
        Select(0),
        Secret,
        Select(4),
        Select(2),
        Select(3),
        Select(1),
        Select(2),
        Select(0),
        Select(0),
    ]);
    let mut network = Probe {
        calls: 0,
        fail: false,
    };
    guide(cfg(), &path, Some(&preferences), &mut ui, &mut network).unwrap();
    let mut expected = original;
    expected["models"]["repair-me"]["apiKey"] = json!("FAKE_SETUP_KEY_NEVER_PRINT");
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved, expected);
    assert_eq!(
        lattice::preferences::load_from(&preferences)["model"],
        "repair-me"
    );
    assert!(ui.messages.join("\n").contains(&ui.label(M::KeepDefault)));
    assert_eq!(
        ui.prompts
            .iter()
            .filter(|s| **s == i18n::text(Language::English, M::ApiKey, &[]))
            .count(),
        1
    );
    assert_eq!(ui.language, Language::Chinese);
    assert!(ui.answers.is_empty());
    assert_eq!(network.calls, 0);
}

#[test]
fn input_validation_preserves_the_invalid_answer_for_correction() {
    let mut ui = Script::new([Text("1Mi"), Text("1M")]);
    assert_eq!(ui.input("tokens", "", Field::Tokens, &[]).unwrap(), "1M");
    assert_eq!(
        ui.text_prompts,
        [
            ("tokens".into(), "".into()),
            ("tokens".into(), "1Mi".into())
        ]
    );
}

#[cfg(unix)]
#[test]
fn successful_generation_probe_is_still_explicit_and_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let mut answers = first_steps(false);
    answers.extend([Select(1), Confirm(true)]);
    let mut ui = Script::new(answers);
    let mut network = Probe {
        calls: 0,
        fail: false,
    };
    guide(
        cfg(),
        &dir.path().join("models.json"),
        None,
        &mut ui,
        &mut network,
    )
    .unwrap();
    assert_eq!(network.calls, 1);
    assert!(ui.answers.is_empty());
}
