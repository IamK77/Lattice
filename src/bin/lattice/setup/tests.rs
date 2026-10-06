use super::*;
use std::collections::VecDeque;

#[path = "ux_tests.rs"]
mod ux;

#[test]
fn custom_endpoint_uses_explicit_limits_and_an_environment_reference() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    std::env::set_var("LATTICE_SETUP_CUSTOM_SYNTHETIC", "synthetic-custom-key");
    use Answer::*;
    let env_choice = usize::from(cfg!(unix));
    let mut ui = Script::new([
        Select(1),
        Select(0),
        Text("https://example.invalid/v1"),
        Select(env_choice),
        Text("LATTICE_SETUP_CUSTOM_SYNTHETIC"),
        Select(1),
        Text("custom-model"),
        Text("1M"),
        Text("32k"),
        Select(3),
        Select(2),
        Multi(&[0]),
        Select(0),
        More(1),
        Text("custom"),
        More(2),
        Select(0),
        Select(0),
    ]);
    let mut probe = Probe {
        calls: 0,
        fail: false,
    };
    let result = guide(cfg(), &path, None, &mut ui, &mut probe).unwrap();
    assert_eq!(result.model, "custom-model");
    assert_eq!(result.key_env, "LATTICE_SETUP_CUSTOM_SYNTHETIC");
    let stored = std::fs::read_to_string(path).unwrap();
    assert!(!stored.contains("synthetic-custom-key"));
    let value: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        value["models"]["custom"]["profile"]["contextWindow"],
        1_000_000
    );
    assert_eq!(
        value["models"]["custom"]["profile"]["maxOutputTokens"],
        32_000
    );
    assert_eq!(value["models"]["custom"]["profile"]["acceptsImages"], true);
    assert_eq!(probe.calls, 0);
}

#[cfg(unix)]
#[test]
fn each_failed_probe_requires_a_new_explicit_action_and_exit_keeps_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let mut answers = first_steps(false);
    answers.extend([Answer::Select(1), Answer::Select(1), Answer::Select(3)]);
    let mut ui = Script::new(answers);
    let mut probe = Probe {
        calls: 0,
        fail: true,
    };
    assert!(matches!(
        guide(cfg(), &path, None, &mut ui, &mut probe),
        Err(Error::Cancelled)
    ));
    assert_eq!(probe.calls, 2);
    assert!(path.exists());
}

#[cfg(unix)]
#[test]
fn redaction_placeholder_can_be_repaired_without_recreating_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let original = json!({"models":{"broken-key":{"adapter":"openai","model":"synthetic","baseUrl":"https://example.invalid","apiKey":"[redacted]","note":"keep","profile":{"contextWindow":4096}}},"unknown":"keep"});
    std::fs::write(&path, original.to_string()).unwrap();
    use Answer::*;
    let mut ui = Script::new([
        Select(2),
        Select(0),
        Secret,
        Select(2),
        Select(0),
        Select(0),
    ]);
    let mut probe = Probe {
        calls: 0,
        fail: false,
    };
    let result = guide(cfg(), &path, None, &mut ui, &mut probe).unwrap();
    assert_eq!(result.model, "synthetic");
    let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut expected = original;
    expected["models"]["broken-key"]["apiKey"] = json!("FAKE_SETUP_KEY_NEVER_PRINT");
    assert_eq!(saved, expected);
    assert_eq!(probe.calls, 0);
}

#[derive(Debug)]
pub(super) enum Answer {
    Select(usize),
    More(usize),
    Multi(&'static [usize]),
    Text(&'static str),
    Secret,
    Key(&'static str),
    Confirm(bool),
    Back,
    Cancel,
}
pub(super) struct Script {
    pub(super) answers: VecDeque<Answer>,
    pub(super) messages: Vec<String>,
    pub(super) multi_prompts: Vec<(Vec<String>, Vec<usize>)>,
    pub(super) language: Language,
    pub(super) prompts: Vec<String>,
    pub(super) text_prompts: Vec<(String, String)>,
}
impl Script {
    pub(super) fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: answers
                .into_iter()
                .flat_map(|answer| match answer {
                    Answer::More(index) => vec![Answer::Select(4), Answer::Select(index)],
                    other => vec![other],
                })
                .collect(),
            messages: vec![],
            multi_prompts: vec![],
            language: Language::English,
            prompts: vec![],
            text_prompts: vec![],
        }
    }
    fn next(&mut self) -> Result<Answer> {
        match self.answers.pop_front().expect("unexpected extra prompt") {
            Answer::Cancel => Err(Error::Cancelled),
            Answer::Back => Err(Error::Back),
            value => Ok(value),
        }
    }
}
impl Questions for Script {
    fn language(&self) -> Language {
        self.language
    }
    fn set_language(&mut self, language: Language) {
        self.language = language;
    }
    fn tell(&mut self, text: &str) {
        self.messages.push(text.into());
    }
    fn select(&mut self, message: &str, options: &[String]) -> Result<usize> {
        self.prompts.push(message.into());
        let Answer::Select(i) = self.next()? else {
            panic!("expected selection at {message}")
        };
        assert!(i < options.len());
        Ok(i)
    }
    fn multi_select(
        &mut self,
        _: &str,
        options: &[String],
        selected: &[usize],
    ) -> Result<Vec<usize>> {
        self.multi_prompts
            .push((options.to_vec(), selected.to_vec()));
        let Answer::Multi(indices) = self.next()? else {
            panic!("expected multiple selection");
        };
        assert!(indices.iter().all(|i| *i < options.len()));
        Ok(indices.to_vec())
    }
    fn text(&mut self, message: &str, default: &str) -> Result<String> {
        self.prompts.push(message.into());
        self.text_prompts.push((message.into(), default.into()));
        let Answer::Text(text) = self.next()? else {
            panic!("expected text at {message}")
        };
        Ok(text.into())
    }
    fn secret(&mut self, message: &str) -> Result<String> {
        self.prompts.push(message.into());
        match self.next()? {
            Answer::Secret => Ok("FAKE_SETUP_KEY_NEVER_PRINT".into()),
            Answer::Key(key) => Ok(key.into()),
            _ => panic!("expected secret"),
        }
    }
    fn confirm(&mut self, _: &str, _: bool) -> Result<bool> {
        let Answer::Confirm(value) = self.next()? else {
            panic!("expected confirmation")
        };
        Ok(value)
    }
}
struct Probe {
    calls: usize,
    fail: bool,
}
impl SetupNetwork for Probe {
    fn models(&mut self, _: &Value) -> std::result::Result<Vec<discovery::Model>, String> {
        panic!("model discovery must require an explicit selection");
    }
    fn test(&mut self, _: &Entry) -> std::result::Result<(), String> {
        self.calls += 1;
        if self.fail {
            Err("synthetic refusal".into())
        } else {
            Ok(())
        }
    }
}
fn cfg() -> PresetConfig {
    PresetConfig {
        adapter: "openai".into(),
        model: "original".into(),
        base_url: "https://example.invalid".into(),
        key_env: "LATTICE_SETUP_TEST_MISSING".into(),
        workspace: Some("do-not-create-workspace".into()),
        context_window: 1000,
        usage_input_field: "prompt_tokens".into(),
        profile: None,
        catalog_problems: vec![],
        system: "preserve system".into(),
        thinking: None,
        scripted: None,
        overlay: None,
        assembly: None,
    }
}
fn first_steps(preferred: bool) -> Vec<Answer> {
    use Answer::*;
    let mut answers = vec![
        Select(0),
        Select(2),
        Select(0),
        Secret,
        Select(1),
        Text("deepseek-flash"),
        More(1),
        Text("setup-test"),
    ];
    if !preferred {
        answers.push(More(2));
    }
    answers.push(Select(0));
    answers
}

#[test]
fn cancel_before_saving_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-created/models.json");
    let mut ui = Script::new([Answer::Select(0), Answer::Select(2), Answer::Cancel]);
    let mut probe = Probe {
        calls: 0,
        fail: false,
    };
    assert!(matches!(
        guide(cfg(), &path, None, &mut ui, &mut probe),
        Err(Error::Cancelled)
    ));
    assert!(!path.parent().unwrap().exists());
    assert_eq!(probe.calls, 0);
}

#[cfg(unix)]
#[test]
fn save_and_skip_makes_no_request_and_preserves_launch_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let preferences = dir.path().join("preferences.json");
    let mut answers = first_steps(true);
    answers.push(Answer::Select(0));
    let mut ui = Script::new(answers);
    let mut probe = Probe {
        calls: 0,
        fail: false,
    };
    let result = guide(cfg(), &path, Some(&preferences), &mut ui, &mut probe).unwrap();
    assert_eq!(result.model, "deepseek-flash");
    assert_eq!(result.workspace, cfg().workspace);
    assert_eq!(result.system, cfg().system);
    assert_eq!(probe.calls, 0);
    assert!(!ui
        .messages
        .join("\n")
        .contains("FAKE_SETUP_KEY_NEVER_PRINT"));
    assert!(ui.answers.is_empty());
    assert_eq!(
        lattice::preferences::load_from(&preferences)["model"],
        "setup-test"
    );
    assert!(!dir.path().join("ledgers").exists());
}

#[cfg(unix)]
#[test]
fn failed_probe_is_not_retried_and_user_can_skip() {
    let dir = tempfile::tempdir().unwrap();
    let mut answers = first_steps(false);
    answers.extend([Answer::Select(1), Answer::Select(0)]);
    let mut ui = Script::new(answers);
    let mut probe = Probe {
        calls: 0,
        fail: true,
    };
    assert!(guide(
        cfg(),
        &dir.path().join("models.json"),
        None,
        &mut ui,
        &mut probe
    )
    .is_ok());
    assert_eq!(probe.calls, 1);
    assert!(ui.messages.join("\n").contains("synthetic refusal"));
}

#[cfg(unix)]
#[test]
fn preference_failure_does_not_rollback_the_saved_model() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let pref = dir.path().join("preferences.json");
    std::fs::write(&pref, "broken").unwrap();
    let mut answers = first_steps(true);
    answers.extend([Answer::Select(0), Answer::Select(0)]);
    let mut ui = Script::new(answers);
    let mut probe = Probe {
        calls: 0,
        fail: false,
    };
    assert!(guide(cfg(), &path, Some(&pref), &mut ui, &mut probe).is_ok());
    assert_eq!(std::fs::read_to_string(pref).unwrap(), "broken");
    assert!(path.exists());
    assert!(ui
        .messages
        .join("\n")
        .contains("default selection NOT saved"));
}

#[test]
fn invalid_catalog_is_not_treated_as_first_installation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let original = r#"{"models": ["do not destroy"]}"#;
    std::fs::write(&path, original).unwrap();
    let mut ui = Script::new([Answer::Select(2)]);
    let mut probe = Probe {
        calls: 0,
        fail: false,
    };
    assert!(matches!(
        guide(cfg(), &path, None, &mut ui, &mut probe),
        Err(Error::Cancelled)
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn endpoint_validation_rejects_hidden_credentials_and_non_http_schemes() {
    for url in [
        "file:///tmp/model",
        "https://key@example.invalid",
        "https://example.invalid?key=secret",
        "https://example.invalid/#secret",
        "",
    ] {
        assert!(
            validate_target(&json!({"adapter":"openai","model":"test","baseUrl":url})).is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn returning_to_setup_choices_retains_the_complete_unsaved_draft() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let mut answers = first_steps(false);
    answers.pop();
    answers.extend([
        Answer::More(5),
        Answer::Select(0),
        Answer::Select(0),
        Answer::Select(0),
    ]);
    let mut ui = Script::new(answers);
    let mut network = Probe {
        calls: 0,
        fail: false,
    };
    let result = guide(cfg(), &path, None, &mut ui, &mut network).unwrap();
    assert_eq!(result.model, "deepseek-flash");
    assert!(ui.answers.is_empty());
    assert_eq!(network.calls, 0);
}

#[test]
fn confirmed_capabilities_reach_runtime_configuration_not_only_the_catalog() {
    let entry = Entry {
        id: "chosen".into(),
        adapter: "responses".into(),
        model: "synthetic".into(),
        base_url: "https://example.invalid".into(),
        key_env: "SYNTHETIC_KEY_ENV".into(),
        profile: Some(
            json!({"contextWindow":8192,"maxOutputTokens":2048,"acceptsImages":true,
            "effort":["low","high"],"nativeWebSearch":true,"nativeImageGeneration":true,
            "usageFields":{"input":"custom.input","output":"custom.output"}}),
        ),
    };
    let mut launch = cfg();
    apply(&mut launch, &entry);
    assert_eq!(launch.usage_input_field, "custom.input");
    let running = lattice::preset::running_entry(&launch);
    assert!(running.accepts_images());
    let gate = lattice::preset::model_profile(&launch);
    assert_eq!(gate["contextWindow"], 8192);
    assert_eq!(gate["usageFields"]["input"], "custom.input");
    let model = lattice::preset::main_model_config(&running, launch.thinking.as_ref());
    assert_eq!(model["maxTokens"], 2048);
    assert_eq!(model["effort"], json!(["low", "high"]));
    assert_eq!(model["nativeWebSearch"], true);
    assert_eq!(model["nativeImageGeneration"], true);
    assert_eq!(launch.workspace, cfg().workspace);
}

#[test]
fn guided_template_is_complete_but_does_not_seed_a_catalog() {
    let template: Value = serde_json::from_str(include_str!("deepseek.json")).unwrap();
    let entry = &template["entry"];
    validate_target(entry).unwrap();
    assert_eq!(entry["profile"]["contextWindow"], 1048576);
    assert_eq!(entry["profile"]["maxOutputTokens"], 393216);
    assert!(entry.get("apiKey").is_none());
    assert!(entry.get("apiKeyEnv").is_none());
    assert!(!template["sources"].as_array().unwrap().is_empty());
}
