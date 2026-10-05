//! A resumable frontend draft: provider, dialect, connection, model, capabilities, review.
//! Presets describe documented formats, not live-model compatibility guarantees:
//! https://developers.openai.com/api/reference/resources/responses/methods/create
//! https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create
//! https://platform.claude.com/docs/en/cli-sdks-libraries/libraries/openai-sdk
//! https://api-docs.deepseek.com/guides/anthropic_api/
//! https://api-docs.deepseek.com/guides/responses_api/
use super::{
    choose, credential, discovery, validate_target, Error, Questions, Result, SetupNetwork,
};
use lattice::models::catalog::{validate_name, Snapshot};
use serde_json::{json, Value};
use std::path::Path;

#[path = "capabilities.rs"]
mod capabilities;
use capabilities::Capabilities;

#[derive(Clone, Copy, PartialEq)]
enum Provider {
    OpenAI,
    Anthropic,
    DeepSeek,
}
impl Provider {
    fn protocols(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::OpenAI => &[
                ("responses", "OpenAI Responses (recommended)"),
                ("openai", "OpenAI Chat Completions"),
            ],
            Self::Anthropic => &[
                ("anthropic", "Anthropic Messages (recommended)"),
                ("openai", "OpenAI Chat Completions (compatibility layer)"),
            ],
            Self::DeepSeek => &[
                ("openai", "OpenAI Chat Completions (recommended)"),
                ("anthropic", "Anthropic Messages"),
                (
                    "responses",
                    "OpenAI Responses (provider compatibility limits apply)",
                ),
            ],
        }
    }
    fn base(self, adapter: &str) -> &'static str {
        match (self, adapter) {
            (Self::OpenAI, _) => "https://api.openai.com/v1",
            (Self::Anthropic, "anthropic") => "https://api.anthropic.com",
            (Self::Anthropic, _) => "https://api.anthropic.com/v1",
            (Self::DeepSeek, "anthropic") => "https://api.deepseek.com/anthropic",
            (Self::DeepSeek, _) => "https://api.deepseek.com",
        }
    }
}

pub(super) struct Configured {
    pub id: String,
    pub spec: Value,
    pub preferred: bool,
}

#[derive(Clone, Copy)]
enum Step {
    Provider,
    Protocol,
    Connection,
    Model,
    Capabilities,
    Review,
}

struct Draft {
    spec: Value,
    listed: Vec<discovery::Model>,
    capabilities: Option<Capabilities>,
}
impl Draft {
    fn new() -> Self {
        Self {
            spec: json!({}),
            listed: vec![],
            capabilities: None,
        }
    }

    fn connection(&mut self, adapter: &str, base: &str) {
        if self.spec["adapter"] == adapter && self.spec["baseUrl"] == base {
            return;
        }
        if self.spec["baseUrl"] != base {
            // Never carry a credential to a changed endpoint automatically.
            self.spec.as_object_mut().unwrap().remove("apiKey");
            self.spec.as_object_mut().unwrap().remove("apiKeyEnv");
        }
        self.spec["adapter"] = json!(adapter);
        self.spec["baseUrl"] = json!(base);
        self.forget_models();
    }

    fn credential(&mut self, field: &str, value: &str) {
        if self.spec[field] == value {
            return;
        }
        self.spec.as_object_mut().unwrap().remove("apiKey");
        self.spec.as_object_mut().unwrap().remove("apiKeyEnv");
        self.spec[field] = json!(value);
        // Capabilities and list visibility may be specific to this account.
        self.forget_models();
    }

    fn forget_models(&mut self) {
        self.spec.as_object_mut().unwrap().remove("model");
        self.spec.as_object_mut().unwrap().remove("profile");
        self.listed.clear();
        self.capabilities = None;
    }

    fn model(&mut self, model: &discovery::Model) {
        let incoming = Capabilities::new(
            &model.id,
            self.spec["adapter"].as_str().unwrap(),
            &model.profile,
            model.input_limit,
            self.spec["baseUrl"].as_str().unwrap(),
        );
        if self.spec["model"] == model.id {
            if let Some(current) = &mut self.capabilities {
                current.refresh(incoming);
                return;
            }
        }
        self.spec["model"] = json!(model.id);
        self.capabilities = Some(incoming);
    }
}

pub(super) struct Session {
    custom: bool,
    provider: Option<Provider>,
    draft: Draft,
    step: Step,
    id: String,
    preferred: bool,
}
impl Session {
    pub fn new(custom: bool) -> Self {
        Self {
            custom,
            provider: None,
            draft: Draft::new(),
            step: if custom {
                Step::Protocol
            } else {
                Step::Provider
            },
            id: String::new(),
            preferred: true,
        }
    }
}

pub(super) fn configure(
    ui: &mut impl Questions,
    network: &mut impl SetupNetwork,
    path: &Path,
    snapshot: &Snapshot,
    session: &mut Session,
) -> Result<Option<Configured>> {
    let Session {
        custom,
        provider,
        draft,
        step,
        id,
        preferred,
    } = session;
    loop {
        *step = match *step {
            Step::Provider => {
                let next = match choose(
                    ui,
                    "Choose a provider",
                    &["OpenAI", "Anthropic", "DeepSeek", "Back"],
                )? {
                    0 => Provider::OpenAI,
                    1 => Provider::Anthropic,
                    2 => Provider::DeepSeek,
                    _ => return Ok(None),
                };
                if *provider != Some(next) {
                    *draft = Draft::new();
                }
                *provider = Some(next);
                Step::Protocol
            }
            Step::Protocol => {
                let protocols = provider.map(Provider::protocols).unwrap_or(&[
                    ("openai", "OpenAI Chat Completions"),
                    ("responses", "OpenAI Responses"),
                    ("anthropic", "Anthropic Messages"),
                ]);
                let mut options: Vec<_> =
                    protocols.iter().map(|(_, name)| name.to_string()).collect();
                options.push("Back".into());
                let selected = ui.select("Choose the API format (not the provider)", &options)?;
                if selected == protocols.len() {
                    if provider.is_some() {
                        Step::Provider
                    } else {
                        return Ok(None);
                    }
                } else {
                    let adapter = protocols[selected].0;
                    let base = if draft.spec["adapter"] == adapter {
                        draft.spec["baseUrl"].as_str().unwrap_or("").to_owned()
                    } else {
                        provider.map(|p| p.base(adapter)).unwrap_or("").to_owned()
                    };
                    draft.connection(adapter, &base);
                    Step::Connection
                }
            }
            Step::Connection => {
                let adapter = draft.spec["adapter"].as_str().unwrap().to_owned();
                let route = match adapter.as_str() {
                    "anthropic" => "/v1/messages",
                    "responses" => "/responses",
                    _ => "/chat/completions",
                };
                let base = ui.text(
                    &format!("API base URL (Lattice appends {route})"),
                    draft.spec["baseUrl"].as_str().unwrap_or(""),
                )?;
                let base = base.trim().trim_end_matches('/');
                if let Err(error) =
                    validate_target(&json!({"adapter":adapter,"baseUrl":base,"model":"draft"}))
                {
                    ui.tell(&error);
                    continue;
                }
                draft.connection(&adapter, base);
                if base.starts_with("http://") {
                    ui.tell("Warning: HTTP sends the credential without transport encryption.");
                }
                if choose(
                    ui,
                    "Connection settings",
                    &["Continue with this endpoint", "Back to API format"],
                )? == 1
                {
                    Step::Protocol
                } else {
                    if discovery::key(&draft.spec).is_err()
                        || ui.confirm("Replace the credential already entered?", false)?
                    {
                        let (field, value) = credential(ui)?;
                        draft.credential(field, &value);
                    }
                    Step::Model
                }
            }
            Step::Model => {
                if select_model(ui, network, draft)? {
                    Step::Capabilities
                } else {
                    Step::Connection
                }
            }
            Step::Capabilities => {
                let adapter = draft.spec["adapter"].as_str().unwrap();
                if draft
                    .capabilities
                    .as_mut()
                    .expect("selected model")
                    .edit(ui, adapter)?
                {
                    Step::Review
                } else {
                    Step::Model
                }
            }
            Step::Review => {
                if id.is_empty() {
                    *id = name(ui, snapshot, draft.spec["model"].as_str().unwrap())?;
                    *preferred =
                        ui.confirm("Save as the default model for future launches?", *preferred)?;
                } else if snapshot.entry(id).is_some() {
                    ui.tell("That local name was saved by another operation; choose a different name for this draft.");
                    *id = name(ui, snapshot, id)?;
                }
                ui.tell(&format!("Review configuration\nLocal name: {id}\nModel: {}\nAPI format: {}\nEndpoint: {}\nCatalog: {}\nDefault model: {preferred}",
                    draft.spec["model"].as_str().unwrap(), draft.spec["adapter"].as_str().unwrap(), draft.spec["baseUrl"].as_str().unwrap(), path.display()));
                if draft.spec.get("apiKey").is_some() {
                    ui.tell("Credential: stored locally (hidden). A local key is saved in an agent-readable file; file-tool reads can place it in permanent history and model context.");
                } else {
                    ui.tell("Credential: reference to an existing environment variable.");
                }
                draft.capabilities.as_ref().unwrap().show(ui);
                match choose(
                    ui,
                    "Review complete",
                    &[
                        "Save and continue",
                        "Edit endpoint or credential",
                        "Choose another model",
                        "Edit model capabilities",
                        "Edit local name",
                        "Change default preference",
                        "Change provider / API format",
                        "Back to setup choices",
                        "Exit",
                    ],
                )? {
                    0 => {
                        draft.spec["profile"] = draft.capabilities.as_ref().unwrap().value.clone();
                        return Ok(Some(Configured {
                            id: id.clone(),
                            spec: draft.spec.clone(),
                            preferred: *preferred,
                        }));
                    }
                    1 => Step::Connection,
                    2 => Step::Model,
                    3 => Step::Capabilities,
                    4 => {
                        *id = name(ui, snapshot, id)?;
                        Step::Review
                    }
                    5 => {
                        *preferred = ui.confirm(
                            "Save as the default model for future launches?",
                            *preferred,
                        )?;
                        Step::Review
                    }
                    6 => {
                        if *custom {
                            Step::Protocol
                        } else {
                            Step::Provider
                        }
                    }
                    7 => return Ok(None),
                    _ => return Err(Error::Cancelled),
                }
            }
        };
    }
}

fn name(ui: &mut impl Questions, snapshot: &Snapshot, default: &str) -> Result<String> {
    let suggested: String = default
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    loop {
        let name = ui.text("Local name for this model", &suggested)?;
        let name = name.trim();
        if let Err(error) = validate_name(name) {
            ui.tell(&error);
            continue;
        }
        if snapshot.entry(name).is_some() {
            ui.tell("That name already exists; use its repair option or choose another name.");
            continue;
        }
        return Ok(name.into());
    }
}

fn select_model(
    ui: &mut impl Questions,
    network: &mut impl SetupNetwork,
    draft: &mut Draft,
) -> Result<bool> {
    loop {
        let (url, _) = discovery::endpoint(&draft.spec)?;
        ui.tell(&format!("Model discovery will GET {url} with the credential you entered, only if you select Fetch. It sends no generation request. A listed model is not proof of format/tool compatibility."));
        let mut options = vec![
            "Fetch model list from this service",
            "Enter model name manually",
        ];
        let cached = if draft.listed.is_empty() {
            None
        } else {
            options.push("Choose from fetched list");
            Some(options.len() - 1)
        };
        let keep = if draft.capabilities.is_none() {
            None
        } else {
            options.push("Keep the selected model");
            Some(options.len() - 1)
        };
        options.push("Back to connection settings");
        match choose(ui, "Choose a model", &options)? {
            0 => {
                match network.models(&draft.spec) {
                    Ok(models) => draft.listed = models,
                    Err(error) => {
                        ui.tell(&format!("Could not fetch models: {error}\nRetry explicitly or enter a model manually."));
                        continue;
                    }
                }
                if pick_list(ui, draft)? {
                    return Ok(true);
                }
            }
            1 => {
                let id = ui.text(
                    "Exact model identifier",
                    draft.spec["model"].as_str().unwrap_or(""),
                )?;
                let id = id.trim();
                if !discovery::valid_id(id) {
                    ui.tell("Enter a nonempty model identifier without control characters (at most 1024 bytes).");
                    continue;
                }
                let model = draft
                    .listed
                    .iter()
                    .find(|m| m.id == id)
                    .cloned()
                    .unwrap_or_else(|| discovery::Model {
                        id: id.into(),
                        profile: json!({}),
                        input_limit: None,
                    });
                draft.model(&model);
                return Ok(true);
            }
            selected if Some(selected) == cached => {
                if pick_list(ui, draft)? {
                    return Ok(true);
                }
            }
            selected if Some(selected) == keep => return Ok(true),
            _ => return Ok(false),
        }
    }
}

fn pick_list(ui: &mut impl Questions, draft: &mut Draft) -> Result<bool> {
    if draft.listed.is_empty() {
        ui.tell(
            "No models are available in the fetched list. Enter a model manually or fetch again.",
        );
        return Ok(false);
    }
    let mut options: Vec<_> = draft.listed.iter().map(|m| m.id.clone()).collect();
    options.push("Back — manual entry is available".into());
    let selected = ui.select("Available models (type to filter)", &options)?;
    if let Some(model) = draft.listed.get(selected).cloned() {
        draft.model(&model);
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
#[path = "wizard_tests.rs"]
mod tests;
