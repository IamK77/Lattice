//! Frontend-owned bootstrap: ordinary prompts before any conversation or TUI.
use lattice::{
    models::{self, catalog::Snapshot, Entry},
    preset::PresetConfig,
};
use serde_json::{json, Value};
use std::{io::IsTerminal, path::Path};

#[path = "setup/discovery.rs"]
mod discovery;
#[path = "setup/probe.rs"]
mod probe;
#[path = "setup/prompts.rs"]
mod prompts;
#[cfg(test)]
#[path = "setup/tests.rs"]
mod tests;
#[path = "setup/wizard.rs"]
mod wizard;

#[derive(Debug)]
enum Error {
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
    fn tell(&mut self, message: &str);
    fn select(&mut self, message: &str, options: &[String]) -> Result<usize>;
    fn text(&mut self, message: &str, default: &str) -> Result<String>;
    fn secret(&mut self, message: &str) -> Result<String>;
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool>;
}
trait SetupNetwork {
    fn test(&mut self, entry: &Entry) -> std::result::Result<(), String>;
    fn models(&mut self, spec: &Value) -> std::result::Result<Vec<discovery::Model>, String>;
}

fn choose(ui: &mut impl Questions, message: &str, options: &[&str]) -> Result<usize> {
    ui.select(
        message,
        &options.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
    )
}

pub(crate) fn prepare() -> std::io::Result<Option<PresetConfig>> {
    let cfg = PresetConfig::from_env();
    if ready(&cfg) {
        return Ok(Some(cfg));
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(std::io::Error::other("model configuration is incomplete; run lattice in an interactive terminal to configure it, or supply a valid catalog and credential"));
    }
    let mut ui = prompts::Terminal;
    ui.tell("Lattice needs a usable model configuration before opening the TUI.\nAnswer the following questions, or press Esc / Ctrl-C to exit.");
    let Some(path) = models::path() else {
        ui.tell("The model catalog is disabled or has no location. Set HOME or LATTICE_MODELS, then run again.");
        return Ok(None);
    };
    let mut connection = probe::HttpTest::new(path.with_file_name("setup-tests"));
    match guide(
        cfg,
        &path,
        lattice::preferences::path().as_deref(),
        &mut ui,
        &mut connection,
    ) {
        Ok(cfg) => Ok(Some(cfg)),
        Err(Error::Cancelled) => {
            ui.tell("Setup exited. Any configuration already saved remains available.");
            Ok(None)
        }
        Err(Error::Failed(message)) => Err(std::io::Error::other(message)),
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
    loop {
        let mut snapshot = match Snapshot::read(path) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                ui.tell(&error);
                if choose(
                    ui,
                    "The catalog cannot be edited safely.",
                    &["Check again after repair", "Exit"],
                )? == 1
                {
                    return Err(Error::Cancelled);
                }
                continue;
            }
        };
        let (loaded, _) = models::load_from(path);
        // Repairable raw entries include credentials rejected by the runtime
        // loader (for example its own redaction placeholder).
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
        let mut options = vec![
            "Choose a provider".to_owned(),
            "Custom configuration".to_owned(),
        ];
        options.extend(
            entries
                .iter()
                .map(|e| format!("Use / repair {} ({})", e.id, e.model)),
        );
        let explicit = [
            "LATTICE_ADAPTER",
            "LATTICE_MODEL",
            "LATTICE_BASE_URL",
            "LATTICE_API_KEY_ENV",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some());
        let repair_launch = explicit && validate_target(&target(&cfg)).is_ok();
        if repair_launch {
            options.push(format!("Repair this launch: {}", cfg.model));
        }
        options.push("Exit".into());
        let selected = ui.select("Choose how to configure the model", &options)?;
        if selected == options.len() - 1 {
            return Err(Error::Cancelled);
        }
        if explicit {
            ui.tell("Your launch environment overrides saved preferences on future launches. This selection overrides it for this launch only.");
        }
        let fresh = if selected < 2 {
            let Some(configured) =
                wizard::configure(ui, connection, path, &snapshot, &mut drafts[selected])?
            else {
                continue;
            };
            Some(configured)
        } else {
            None
        };
        let existing = selected.checked_sub(2).and_then(|i| entries.get(i));
        let mut spec = if let Some(entry) = existing {
            snapshot
                .entry(&entry.id)
                .cloned()
                .ok_or_else(|| Error::Failed("catalog entry disappeared".into()))?
        } else if let Some(configured) = &fresh {
            configured.spec.clone()
        } else {
            target(&cfg)
        };
        if let Err(error) = validate_target(&spec) {
            ui.tell(&error);
            continue;
        }
        let id = if let Some(entry) = existing {
            entry.id.clone()
        } else if let Some(configured) = &fresh {
            configured.id.clone()
        } else {
            loop {
                let id = ui.text("Local name for this model", "my-model")?;
                let id = id.trim();
                if let Err(error) = models::catalog::validate_name(id) {
                    ui.tell(&error);
                    continue;
                }
                if snapshot.entry(id).is_some() {
                    ui.tell(
                        "That name already exists; choose another name or use its repair option.",
                    );
                    continue;
                }
                break id.to_owned();
            }
        };
        let needs_key = existing.is_none_or(|e| !e.key_present());
        let change_key = fresh.is_none()
            && (needs_key || ui.confirm("Replace this model's existing credential?", false)?);
        if change_key {
            let (field, value) = credential(ui)?;
            spec.as_object_mut()
                .expect("validated model object")
                .remove("apiKey");
            spec.as_object_mut()
                .expect("validated model object")
                .remove("apiKeyEnv");
            spec[field] = json!(value);
        }
        let local_key = spec.get("apiKey").is_some();
        if fresh.is_none() {
            ui.tell(&format!(
                "Model: {}\nEndpoint: {}\nCatalog: {}\nCredential: {}",
                spec["model"].as_str().unwrap_or(""),
                spec["baseUrl"].as_str().unwrap_or(""),
                path.display(),
                if local_key {
                    "stored locally (hidden)"
                } else {
                    "environment variable reference"
                }
            ));
            if local_key {
                ui.tell("A local key is saved in an agent-readable file. File-tool reads can place it in permanent history and model context.");
            }
            if spec["baseUrl"]
                .as_str()
                .is_some_and(|s| s.starts_with("http://"))
            {
                ui.tell("Warning: this endpoint uses unencrypted HTTP, including its credential.");
            }
        }
        let preferred = if let Some(configured) = &fresh {
            configured.preferred
        } else {
            let preferred = ui.confirm("Save as the default model for future launches?", true)?;
            match choose(
                ui,
                "Review complete",
                &["Save and continue", "Back to model selection", "Exit"],
            )? {
                1 => continue,
                2 => return Err(Error::Cancelled),
                _ => {}
            }
            preferred
        };
        if let Some(entry) = existing {
            if change_key {
                let field = if local_key { "apiKey" } else { "apiKeyEnv" };
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
                ui.tell(&format!(
                    "Not saved: {error}\nChoose again after correcting the problem."
                ));
                continue;
            }
        }
        ui.tell("Model configuration saved / selected. Exiting now will not delete it.");
        let saved = Snapshot::read(path)?;
        if saved.entry(&id) != Some(&spec) {
            ui.tell("The model changed after selection; review it again.");
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
            ui.tell("The selected credential is no longer available; please repair it.");
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
                        ui.tell(&format!(
                            "Model saved; default selection NOT saved: {error}"
                        ));
                        match choose(
                            ui,
                            "How would you like to continue?",
                            &[
                                "Continue for this launch",
                                "Retry saving the default",
                                "Exit",
                            ],
                        )? {
                            0 => break,
                            1 => continue,
                            _ => return Err(Error::Cancelled),
                        }
                    }
                }
            }
        }
        loop {
            ui.tell("A connection test sends a short request to this provider and may incur a small charge. It does not test tools or a full conversation.");
            match choose(ui, "Connection test (optional)", &["Skip test and enter TUI", "Send a test request", "Change configuration", "Exit"])? {
                0 => { apply(&mut cfg, &entry); return Ok(cfg); }
                1 => match connection.test(&entry) {
                    Ok(()) => {
                        ui.tell("The provider returned a valid test response.");
                        if ui.confirm("Enter the TUI now?", true)? { apply(&mut cfg, &entry); return Ok(cfg); }
                    }
                    Err(error) => ui.tell(&format!("Test did not succeed: {error}\nYou may retry explicitly, change configuration, skip, or exit. Saved configuration was retained.")),
                },
                2 => break,
                _ => return Err(Error::Cancelled),
            }
        }
    }
}

fn credential(ui: &mut impl Questions) -> Result<(&'static str, String)> {
    let mut options = Vec::new();
    if cfg!(unix) {
        options.push("Save a key locally".to_owned());
    }
    options.push("Use an existing environment variable".to_owned());
    let selected = ui.select("How should Lattice obtain the key?", &options)?;
    let local = cfg!(unix) && selected == 0;
    loop {
        let value = if local {
            ui.secret("API key")?
        } else {
            ui.text("Environment variable name", "")?
        };
        let value = value.trim();
        if local {
            if value.is_empty() || value == "[redacted]" || value.chars().any(char::is_control) {
                ui.tell(
                    "Enter a nonempty key without control characters, not a redaction placeholder.",
                );
                continue;
            }
            return Ok(("apiKey", value.into()));
        }
        let valid = !value.is_empty()
            && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !value.starts_with(|c: char| c.is_ascii_digit());
        if !valid || !std::env::var(value).is_ok_and(|v| !v.is_empty()) {
            ui.tell("Choose an existing variable with a nonempty value and a valid variable name.");
            continue;
        }
        return Ok(("apiKeyEnv", value.into()));
    }
}
