//! Frontend-owned bootstrap: ordinary prompts before any conversation or TUI.
use lattice::{
    models::{self, catalog::Snapshot, Entry},
    preset::PresetConfig,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::IsTerminal, path::Path};

#[path = "setup/discovery.rs"]
mod discovery;
#[path = "setup/i18n.rs"]
mod i18n;
#[path = "setup/input.rs"]
mod input;
#[path = "setup/probe.rs"]
mod probe;
#[path = "setup/prompts.rs"]
mod prompts;
#[path = "setup/repair.rs"]
mod repair;
#[path = "setup/screen.rs"]
mod screen;
#[cfg(test)]
#[path = "setup/tests.rs"]
mod tests;
#[path = "setup/wizard.rs"]
mod wizard;
use i18n::{Id as M, Language};
use input::Field;

#[derive(Debug)]
enum Error {
    Back,
    Cancelled,
    Failed(String),
}
type Result<T> = std::result::Result<T, Error>;
impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::Failed(value)
    }
}

trait Questions {
    fn page(&mut self, _title: M, _context: &[String]) {}
    fn busy(&mut self, message: M) -> Result<()> {
        self.say(message, &[]);
        Ok(())
    }
    fn details(&mut self, title: M, lines: &[String]) -> Result<()>
    where
        Self: Sized,
    {
        self.page(title, &[]);
        for line in lines {
            self.tell(line);
        }
        match choose(self, title, &[M::Back]) {
            Ok(_) | Err(Error::Back) => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn language(&self) -> Language {
        Language::English
    }
    fn set_language(&mut self, _language: Language) {}
    fn message(&self, id: M, args: &[&str]) -> String {
        i18n::text(self.language(), id, args)
    }
    fn label(&self, id: M) -> String {
        self.message(id, &[])
    }
    fn say(&mut self, id: M, args: &[&str]) {
        self.tell(&self.message(id, args));
    }
    fn input(
        &mut self,
        message: &str,
        default: &str,
        field: Field,
        _suggestions: &[String],
    ) -> Result<String> {
        let mut draft = default.to_owned();
        loop {
            draft = self.text(message, &draft)?;
            match field.validate(&draft) {
                Ok(()) => return Ok(draft.trim().into()),
                Err(id) => self.say(id, &[]),
            }
        }
    }
    fn tell(&mut self, message: &str);
    fn select(&mut self, message: &str, options: &[String]) -> Result<usize>;
    fn multi_select(
        &mut self,
        message: &str,
        options: &[String],
        selected: &[usize],
    ) -> Result<Vec<usize>>;
    fn text(&mut self, message: &str, default: &str) -> Result<String>;
    fn secret(&mut self, message: &str) -> Result<String>;
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool>;
}
trait SetupNetwork {
    fn test(&mut self, entry: &Entry) -> std::result::Result<(), String>;
    fn models(&mut self, spec: &Value) -> std::result::Result<Vec<discovery::Model>, String>;
}
fn choose(ui: &mut impl Questions, message: M, options: &[M]) -> Result<usize> {
    ui.select(
        &ui.label(message),
        &options.iter().map(|id| ui.label(*id)).collect::<Vec<_>>(),
    )
}
fn api_name(adapter: &str) -> &str {
    match adapter {
        "openai" => "OpenAI Chat Completions",
        "responses" => "OpenAI Responses",
        "anthropic" => "Anthropic Messages",
        other => other,
    }
}
fn connection_context(spec: &Value) -> Vec<String> {
    match (spec["adapter"].as_str(), spec["baseUrl"].as_str()) {
        (Some(adapter), Some(base)) if !base.is_empty() => {
            vec![format!("{} · {base}", api_name(adapter))]
        }
        _ => vec![],
    }
}
fn review_action(ui: &mut impl Questions) -> Result<usize> {
    match choose(
        ui,
        M::Review,
        &[
            M::SaveContinue,
            M::EditConnection,
            M::EditModel,
            M::EditCapabilities,
            M::MoreSettings,
        ],
    )? {
        action @ 0..=3 => Ok(action),
        _ => match choose(
            ui,
            M::MoreSettings,
            &[
                M::ShowDetails,
                M::EditName,
                M::ToggleDefault,
                M::EditProvider,
                M::LanguageMenu,
                M::HomeBack,
                M::Exit,
                M::Back,
            ],
        ) {
            Ok(index) => Ok([10, 4, 5, 6, 8, 7, 9, 11][index]),
            Err(Error::Back) => Ok(11),
            Err(error) => Err(error),
        },
    }
}
fn change_language(ui: &mut impl Questions, preferences: Option<&Path>) -> Result<()> {
    let choice = ui.select(
        &ui.label(M::LanguageChoice),
        &["English".into(), "简体中文".into()],
    )?;
    let language = if choice == 1 {
        Language::Chinese
    } else {
        Language::English
    };
    ui.set_language(language);
    match preferences
        .ok_or_else(|| "preferences have no configured path".to_owned())
        .and_then(|path| {
            lattice::preferences::set_in(path, "setupLanguage", json!(language.code())).map(|_| ())
        }) {
        Ok(()) => ui.say(M::LanguageSaved, &[]),
        Err(error) => ui.say(M::LanguageNotSaved, &[&error]),
    }
    Ok(())
}

pub(crate) fn prepare() -> std::io::Result<Option<PresetConfig>> {
    let cfg = PresetConfig::from_env();
    if ready(&cfg) {
        return Ok(Some(cfg));
    }
    if !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        return Err(std::io::Error::other("model configuration is incomplete; run lattice in an interactive terminal to configure it, or supply a valid catalog and credential"));
    }
    let preferences = lattice::preferences::path();
    let saved = preferences
        .as_deref()
        .map(lattice::preferences::load_from)
        .unwrap_or_else(|| json!({}));
    let language = Language::detect(
        saved["setupLanguage"].as_str(),
        Language::environment().as_deref(),
    );
    let Some(path) = models::path() else {
        eprintln!("{}", i18n::text(language, M::NoCatalog, &[]));
        return Ok(None);
    };
    let mut ui = prompts::Terminal::new(language)?;
    let mut connection = probe::HttpTest::new(path.with_file_name("setup-tests"));
    let result = guide(cfg, &path, preferences.as_deref(), &mut ui, &mut connection);
    // The main TUI gets its own terminal lifetime only after setup has restored
    // the primary buffer, including when configuration was already saved.
    ui.close()?;
    match result {
        Ok(cfg) => Ok(Some(cfg)),
        Err(Error::Back | Error::Cancelled) => {
            eprintln!("{}", ui.label(M::Exited));
            Ok(None)
        }
        Err(Error::Failed(message)) => {
            Err(std::io::Error::other(ui.message(M::Failure, &[&message])))
        }
    }
}
fn ready(cfg: &PresetConfig) -> bool {
    cfg.adapter == "scripted"
        || (crate::startup::has_key(cfg)
            && validate_target(
                &json!({"adapter":cfg.adapter,"model":cfg.model,"baseUrl":cfg.base_url}),
            )
            .is_ok())
}
fn validate_target(spec: &Value) -> std::result::Result<(), String> {
    let field = |key| spec.get(key).and_then(Value::as_str).unwrap_or("");
    if !["openai", "anthropic", "responses"].contains(&field("adapter")) {
        return Err("choose a supported API protocol".into());
    }
    if field("model").trim().is_empty() || field("model").chars().any(char::is_control) {
        return Err("enter a nonempty model identifier without control characters".into());
    }
    let url = reqwest::Url::parse(field("baseUrl"))
        .map_err(|_| "enter an absolute HTTP(S) base URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "use an HTTP(S) base URL without embedded credentials, query or fragment".into(),
        );
    }
    Ok(())
}
fn target(cfg: &PresetConfig) -> Value {
    let mut spec = json!({"adapter":cfg.adapter,"model":cfg.model,"baseUrl":cfg.base_url});
    if let Some(profile) = &cfg.profile {
        spec["profile"] = profile.clone();
    }
    spec
}
fn apply(cfg: &mut PresetConfig, entry: &Entry) {
    cfg.adapter = entry.adapter.clone();
    cfg.model = entry.model.clone();
    cfg.base_url = entry.base_url.clone();
    cfg.key_env = entry.key_env.clone();
    cfg.profile = entry.profile.clone();
    cfg.usage_input_field = entry
        .usage_fields()
        .get("input")
        .and_then(Value::as_str)
        .unwrap_or("prompt_tokens")
        .into();
    cfg.catalog_problems.clear();
}

fn guide(
    mut cfg: PresetConfig,
    path: &Path,
    preferences: Option<&Path>,
    ui: &mut impl Questions,
    connection: &mut impl SetupNetwork,
) -> Result<PresetConfig> {
    let mut drafts = [wizard::Session::new(false), wizard::Session::new(true)];
    let mut repairs: BTreeMap<String, repair::Draft> = BTreeMap::new();
    'setup: loop {
        ui.page(M::PageTitle, &[]);
        let mut snapshot = match Snapshot::read(path) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                ui.say(M::RawError, &[&error]);
                match choose(
                    ui,
                    M::UnsafeCatalog,
                    &[M::CheckAgain, M::LanguageMenu, M::Exit],
                )? {
                    0 => {}
                    1 => match change_language(ui, preferences) {
                        Ok(()) | Err(Error::Back) => {}
                        Err(e) => return Err(e),
                    },
                    _ => return Err(Error::Cancelled),
                }
                continue;
            }
        };
        let (loaded, _) = models::load_from(path);
        // Include repairable raw entries whose keys the runtime loader rejects.
        let entries: Vec<Entry> = snapshot
            .entries()
            .filter(|(_, spec)| validate_target(spec).is_ok())
            .map(|(id, spec)| Entry {
                id: id.into(),
                adapter: spec["adapter"].as_str().unwrap().into(),
                model: spec["model"].as_str().unwrap().into(),
                base_url: spec["baseUrl"].as_str().unwrap().into(),
                key_env: loaded
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.key_env.clone())
                    .unwrap_or_default(),
                profile: spec.get("profile").cloned(),
            })
            .collect();
        let mut options = vec![ui.label(M::Provider), ui.label(M::Custom)];
        options.extend(
            entries
                .iter()
                .map(|e| ui.message(M::RepairEntry, &[&e.id, &e.model])),
        );
        let explicit = [
            "LATTICE_ADAPTER",
            "LATTICE_MODEL",
            "LATTICE_BASE_URL",
            "LATTICE_API_KEY_ENV",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some());
        if explicit && validate_target(&target(&cfg)).is_ok() {
            options.push(ui.message(M::RepairLaunch, &[&cfg.model]));
        }
        let language_index = options.len();
        options.extend([ui.label(M::LanguageMenu), ui.label(M::Exit)]);
        let selected = ui.select(&ui.label(M::Home), &options)?;
        if selected == options.len() - 1 {
            return Err(Error::Cancelled);
        }
        if selected == language_index {
            match change_language(ui, preferences) {
                Ok(()) | Err(Error::Back) => {}
                Err(e) => return Err(e),
            }
            continue;
        }
        if explicit {
            ui.say(M::LaunchOverride, &[]);
        }
        let existing = selected.checked_sub(2).and_then(|i| entries.get(i));
        let configured = if selected < 2 {
            let Some(configured) = wizard::configure(
                ui,
                connection,
                path,
                &snapshot,
                &mut drafts[selected],
                preferences,
            )?
            else {
                continue;
            };
            configured
        } else {
            let original = if let Some(entry) = existing {
                snapshot
                    .entry(&entry.id)
                    .cloned()
                    .ok_or_else(|| Error::Failed("catalog entry disappeared".into()))?
            } else {
                target(&cfg)
            };
            let repair_id = existing
                .map(|e| e.id.clone())
                .unwrap_or_else(|| "_launch".into());
            if repairs
                .get(&repair_id)
                .is_none_or(|draft| draft.original != original)
            {
                let id = existing
                    .map(|e| e.id.clone())
                    .unwrap_or_else(|| wizard::suggested_name(&snapshot, &cfg.model));
                repairs.insert(
                    repair_id.clone(),
                    repair::Draft::new(
                        original,
                        id,
                        existing.is_none_or(|e| !e.key_present()),
                        existing.is_some(),
                    ),
                );
            }
            let draft = repairs.get_mut(&repair_id).unwrap();
            if !draft.configure(ui, preferences, path, &snapshot)? {
                continue;
            }
            wizard::Configured {
                id: draft.id.clone(),
                spec: draft.spec.clone(),
                preferred: draft.preferred,
            }
        };
        let wizard::Configured {
            id,
            spec,
            preferred,
        } = configured;
        validate_target(&spec)?;
        let change_key = existing.is_some_and(|entry| snapshot.entry(&entry.id) != Some(&spec));
        if let Some(entry) = existing {
            if change_key {
                let field = if spec.get("apiKey").is_some() {
                    "apiKey"
                } else {
                    "apiKeyEnv"
                };
                snapshot.credential(
                    &entry.id,
                    field,
                    spec[field].as_str().expect("credential string"),
                )?;
            }
        } else {
            snapshot.insert(&id, spec.clone())?;
        }
        if existing.is_none() || change_key {
            if let Err(error) = snapshot.save() {
                ui.say(M::NotSaved, &[&error]);
                continue;
            }
        }
        ui.say(M::Saved, &[]);
        let saved = Snapshot::read(path)?;
        if saved.entry(&id) != Some(&spec) {
            ui.say(M::ChangedAfterSave, &[]);
            continue;
        }
        if selected < 2 {
            drafts[selected] = wizard::Session::new(selected == 1);
        }
        let entry = models::load_from(path)
            .0
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| {
                Error::Failed("saved model could not be loaded; configuration was retained".into())
            })?;
        if !entry.key_present() {
            ui.say(M::MissingKey, &[]);
            continue;
        }
        if preferred {
            loop {
                let result = preferences
                    .ok_or_else(|| "preferences have no configured path".to_owned())
                    .and_then(|p| lattice::preferences::set_in(p, "model", json!(id)).map(|_| ()));
                match result {
                    Ok(()) => break,
                    Err(error) => {
                        ui.say(M::DefaultNotSaved, &[&error]);
                        match choose(
                            ui,
                            M::RecoveryMenu,
                            &[M::ContinueOnce, M::RetryDefault, M::Exit],
                        ) {
                            Ok(0) => break,
                            Ok(1) => continue,
                            Err(Error::Back) => continue 'setup,
                            Ok(_) => return Err(Error::Cancelled),
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        }
        loop {
            ui.page(M::PageTitle, &connection_context(&spec));
            ui.say(M::TestNotice, &[]);
            match choose(
                ui,
                M::TestMenu,
                &[M::SkipTest, M::SendTest, M::ChangeConfiguration, M::Exit],
            ) {
                Ok(0) => {
                    apply(&mut cfg, &entry);
                    return Ok(cfg);
                }
                Ok(1) => {
                    ui.busy(M::PendingTest)?;
                    match connection.test(&entry) {
                        Ok(()) => {
                            ui.say(M::TestSuccess, &[]);
                            match ui.confirm(&ui.label(M::EnterTui), true) {
                                Ok(true) => {
                                    apply(&mut cfg, &entry);
                                    return Ok(cfg);
                                }
                                Ok(false) | Err(Error::Back) => {}
                                Err(e) => return Err(e),
                            }
                        }
                        Err(error) => ui.say(M::TestFailure, &[&error]),
                    }
                }
                Ok(2) | Err(Error::Back) => break,
                Ok(_) => return Err(Error::Cancelled),
                Err(e) => return Err(e),
            }
        }
    }
}

fn credential(ui: &mut impl Questions) -> Result<(&'static str, String)> {
    loop {
        let mut options = vec![];
        if cfg!(unix) {
            options.push(M::LocalKey);
        }
        options.push(M::EnvKey);
        let selected = choose(ui, M::CredentialMenu, &options)?;
        match credential_value(ui, options[selected] == M::LocalKey) {
            Err(Error::Back) => {}
            other => return other,
        }
    }
}
fn credential_value(ui: &mut impl Questions, local: bool) -> Result<(&'static str, String)> {
    if local {
        ui.say(M::KeyWarning, &[]);
        loop {
            let value = ui.secret(&ui.label(M::ApiKey))?;
            match Field::Key.validate(&value) {
                Ok(()) => return Ok(("apiKey", value.trim().into())),
                Err(id) => ui.say(id, &[]),
            }
        }
    }
    let value = ui.input(&ui.label(M::EnvName), "", Field::Env, &[])?;
    Ok(("apiKeyEnv", value))
}
