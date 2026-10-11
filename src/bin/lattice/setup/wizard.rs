//! Resumable frontend drafts, with a short default path and one editable review.
//! Presets describe documented formats, not live-model compatibility guarantees:
//! https://developers.openai.com/api/reference/resources/responses/methods/create
//! https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create
//! https://platform.claude.com/docs/en/cli-sdks-libraries/libraries/openai-sdk
//! https://api-docs.deepseek.com/guides/anthropic_api/
//! https://api-docs.deepseek.com/guides/responses_api/
use super::{
    api_name, change_language, choose, connection_context, credential_value, discovery,
    review_action, Error, Field, Questions, Result, SetupNetwork, M,
};
use lattice::models::catalog::{validate_name, Snapshot};
use serde_json::{json, Value};
use std::path::Path;

#[path = "capabilities.rs"]
mod capabilities;
use capabilities::Capabilities;

use super::providers::{self, Provider, PROVIDERS};
pub(super) struct Configured {
    pub id: String,
    pub spec: Value,
    pub preferred: bool,
}
#[derive(Clone, Copy)]
enum Step {
    Provider,
    Service,
    Protocol,
    Endpoint,
    Connection,
    Model,
    Limits,
    Capabilities,
    Name,
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
            // A changed endpoint must not inherit a credential automatically.
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
        // List visibility and capability metadata may be account-specific.
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
    fn after_connection(&self) -> Step {
        if self.capabilities.is_some() {
            Step::Review
        } else {
            Step::Model
        }
    }
}
pub(super) struct Session {
    custom: bool,
    provider: Option<Provider>,
    service: usize,
    draft: Draft,
    step: Step,
    id: String,
    preferred: bool,
    named: bool,
    editing_from_review: bool,
}
impl Session {
    pub fn new(custom: bool) -> Self {
        Self {
            custom,
            provider: None,
            service: 0,
            draft: Draft::new(),
            step: if custom {
                Step::Protocol
            } else {
                Step::Provider
            },
            id: String::new(),
            preferred: true,
            named: false,
            editing_from_review: false,
        }
    }

    fn back(&mut self) -> bool {
        // An edit opened from Review returns there; walking backwards through
        // the initial path must not bounce between Model and Review forever.
        if self.editing_from_review
            && self.draft.capabilities.is_some()
            && matches!(
                self.step,
                Step::Provider | Step::Service | Step::Protocol | Step::Connection | Step::Model
            )
        {
            self.step = Step::Review;
            return true;
        }
        self.step = match self.step {
            Step::Provider => return false,
            Step::Service => Step::Provider,
            Step::Protocol => {
                let target = json!({"adapter":self.draft.spec["adapter"],"baseUrl":self.draft.spec["baseUrl"],"model":"draft"});
                if super::validate_target(&target).is_err() {
                    return false;
                }
                Step::Connection
            }
            Step::Endpoint => Step::Protocol,
            Step::Connection if self.custom => return false,
            Step::Connection if self.provider.is_some_and(|p| p.services.len() > 1) => {
                Step::Service
            }
            Step::Connection => Step::Provider,
            Step::Model => Step::Connection,
            Step::Limits => Step::Model,
            Step::Capabilities | Step::Name => Step::Review,
            Step::Review => Step::Model,
        };
        true
    }
}

pub(super) fn configure(
    ui: &mut impl Questions,
    network: &mut impl SetupNetwork,
    path: &Path,
    snapshot: &Snapshot,
    session: &mut Session,
    preferences: Option<&Path>,
) -> Result<Option<Configured>> {
    enum Transition {
        Next,
        Leave(Option<Configured>),
    }
    loop {
        // Handle Back at the owning step. Completed fields remain in the draft;
        // a cancelled input never commits its unsubmitted text.
        let context = if matches!(
            session.step,
            Step::Provider | Step::Service | Step::Review | Step::Capabilities
        ) {
            vec![]
        } else {
            connection_context(&session.draft.spec)
        };
        ui.page(
            if matches!(
                session.step,
                Step::Connection | Step::Protocol | Step::Endpoint
            ) {
                M::ConnectionStage
            } else {
                M::PageTitle
            },
            &context,
        );
        let outcome: Result<Transition> = (|| {
            match session.step {
                Step::Provider => {
                    let mut options = PROVIDERS
                        .iter()
                        .map(|p| p.label(ui.language()).to_owned())
                        .collect::<Vec<_>>();
                    options.push(ui.label(M::Back));
                    let selected = ui.select(&ui.label(M::Provider), &options)?;
                    let Some(&next) = PROVIDERS.get(selected) else {
                        return Err(Error::Back);
                    };
                    if session.provider != Some(next) {
                        session.draft = Draft::new();
                        session.service = 0;
                    }
                    session.provider = Some(next);
                    session.step = if next.services.len() > 1 {
                        Step::Service
                    } else {
                        if session.draft.spec["baseUrl"].as_str().is_none() {
                            let route = next.services[0].routes[0];
                            session.draft.connection(route.adapter, route.base);
                        }
                        Step::Connection
                    };
                }
                Step::Service => {
                    let provider = session.provider.unwrap();
                    ui.say(M::ServiceNotice, &[]);
                    let mut options = provider
                        .services
                        .iter()
                        .map(|s| s.label(ui.language()).to_owned())
                        .collect::<Vec<_>>();
                    options.push(ui.label(M::Back));
                    let selected = ui.select(&ui.label(M::Service), &options)?;
                    if selected == provider.services.len() {
                        return Err(Error::Back);
                    }
                    if session.service != selected
                        || session.draft.spec["baseUrl"].as_str().is_none()
                    {
                        // A different service is a different credential audience.
                        session.draft = Draft::new();
                        session.service = selected;
                        let route = provider.services[selected].routes[0];
                        session.draft.connection(route.adapter, route.base);
                    }
                    session.step = Step::Connection;
                }
                Step::Protocol => {
                    let protocols = session
                        .provider
                        .map(|p| {
                            p.services[session.service]
                                .routes
                                .iter()
                                .map(|r| (r.adapter, r.name()))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_else(|| {
                            vec![
                                ("openai", "OpenAI Chat Completions"),
                                ("responses", "OpenAI Responses"),
                                ("anthropic", "Anthropic Messages"),
                            ]
                        });
                    let mut options: Vec<_> = protocols
                        .iter()
                        .enumerate()
                        .map(|(i, (_, name))| {
                            if session.provider.is_some() {
                                ui.message(
                                    if i == 0 {
                                        M::Recommended
                                    } else {
                                        M::Compatibility
                                    },
                                    &[name],
                                )
                            } else {
                                (*name).into()
                            }
                        })
                        .collect();
                    options.push(ui.label(M::Back));
                    let selected = ui.select(&ui.label(M::Protocol), &options)?;
                    if selected == protocols.len() {
                        return Err(Error::Back);
                    }
                    let adapter = protocols[selected].0;
                    let old_adapter = session.draft.spec["adapter"].as_str().unwrap_or("");
                    let old_base = session.draft.spec["baseUrl"].as_str().unwrap_or("");
                    let follows_preset = session.provider.is_some_and(|p| {
                        p.services[session.service]
                            .routes
                            .iter()
                            .any(|r| r.adapter == old_adapter && r.base == old_base)
                    });
                    let base = if old_adapter != adapter && follows_preset {
                        session.provider.unwrap().services[session.service].routes[selected]
                            .base
                            .to_owned()
                    } else {
                        old_base.to_owned()
                    };
                    session.draft.connection(adapter, &base);
                    session.step = Step::Endpoint;
                }
                Step::Endpoint => {
                    let base = ui.input(
                        &ui.label(M::Endpoint),
                        session.draft.spec["baseUrl"].as_str().unwrap_or(""),
                        Field::Url,
                        &[],
                    )?;
                    let adapter = session.draft.spec["adapter"].as_str().unwrap().to_owned();
                    session
                        .draft
                        .connection(&adapter, base.trim_end_matches('/'));
                    session.step = Step::Connection;
                }
                Step::Connection => {
                    if session.provider.is_some() {
                        ui.say(M::StandardApiNotice, &[]);
                    }
                    let draft = &mut session.draft;
                    if draft.spec["baseUrl"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("http://"))
                    {
                        ui.say(M::HttpWarning, &[]);
                    }
                    let mut options = vec![];
                    if cfg!(unix) {
                        options.push(M::LocalKey);
                    }
                    options.extend([M::EnvKey, M::EditConnection]);
                    if discovery::key(&draft.spec).is_ok() {
                        options.push(M::UseConnection);
                    }
                    options.push(M::Back);
                    let selected = choose(ui, M::CredentialMenu, &options)?;
                    match options[selected] {
                        M::EditConnection => session.step = Step::Protocol,
                        M::UseConnection => session.step = draft.after_connection(),
                        M::Back => return Err(Error::Back),
                        action => match credential_value(ui, action == M::LocalKey) {
                            Ok((field, value)) => {
                                draft.credential(field, &value);
                                session.step = draft.after_connection();
                            }
                            Err(Error::Back) => {}
                            other => {
                                other?;
                            }
                        },
                    }
                }
                Step::Model => {
                    if select_model(ui, network, &mut session.draft)? {
                        session.step = if session.draft.capabilities.as_ref().unwrap().valid() {
                            Step::Review
                        } else {
                            Step::Limits
                        };
                    } else {
                        return Err(Error::Back);
                    }
                }
                Step::Limits => {
                    let caps = session.draft.capabilities.as_mut().unwrap();
                    ui.say(M::LimitsMissing, &[]);
                    caps.limits(ui, true)?;
                    if caps.valid() {
                        session.step = Step::Review;
                    }
                }
                Step::Capabilities => {
                    let adapter = session.draft.spec["adapter"].as_str().unwrap().to_owned();
                    session
                        .draft
                        .capabilities
                        .as_mut()
                        .unwrap()
                        .edit(ui, &adapter)?;
                    session.step = Step::Review;
                }
                Step::Name => {
                    session.id = name(ui, snapshot, &session.id)?;
                    session.named = true;
                    session.step = Step::Review;
                }
                Step::Review => {
                    session.editing_from_review = false;
                    if !session.named {
                        session.id =
                            suggested_name(snapshot, session.draft.spec["model"].as_str().unwrap());
                    }
                    ui.say(
                        M::CompactSummary,
                        &[
                            session.draft.spec["model"].as_str().unwrap(),
                            api_name(session.draft.spec["adapter"].as_str().unwrap()),
                            session.draft.spec["baseUrl"].as_str().unwrap(),
                            &session.id,
                            &ui.label(if session.preferred {
                                M::SetDefault
                            } else {
                                M::KeepDefault
                            }),
                        ],
                    );
                    ui.say(
                        if session.draft.spec.get("apiKey").is_some() {
                            M::CredentialLocal
                        } else {
                            M::CredentialEnv
                        },
                        &[],
                    );
                    session.draft.capabilities.as_ref().unwrap().summary(ui);
                    let action = review_action(ui)?;
                    session.editing_from_review = matches!(action, 1..=4 | 6);
                    match action {
                        0 => {
                            if !session.draft.capabilities.as_ref().unwrap().valid() {
                                session.step = Step::Limits;
                            } else if snapshot.entry(&session.id).is_some() {
                                ui.say(M::DuplicateName, &[]);
                                session.step = Step::Name;
                            } else {
                                session.draft.spec["profile"] =
                                    session.draft.capabilities.as_ref().unwrap().value.clone();
                                return Ok(Transition::Leave(Some(Configured {
                                    id: session.id.clone(),
                                    spec: session.draft.spec.clone(),
                                    preferred: session.preferred,
                                })));
                            }
                        }
                        1 => session.step = Step::Connection,
                        2 => session.step = Step::Model,
                        3 => session.step = Step::Capabilities,
                        4 => session.step = Step::Name,
                        5 => session.preferred = !session.preferred,
                        6 => {
                            session.step = if session.custom {
                                Step::Protocol
                            } else {
                                Step::Provider
                            }
                        }
                        7 => return Ok(Transition::Leave(None)),
                        8 => match change_language(ui, preferences) {
                            Ok(()) | Err(Error::Back) => {}
                            Err(e) => return Err(e),
                        },
                        10 => {
                            let mut lines = vec![ui.message(
                                M::ReviewSummary,
                                &[
                                    &session.id,
                                    session.draft.spec["model"].as_str().unwrap(),
                                    api_name(session.draft.spec["adapter"].as_str().unwrap()),
                                    session.draft.spec["baseUrl"].as_str().unwrap(),
                                    &path.display().to_string(),
                                    &ui.label(if session.preferred {
                                        M::SetDefault
                                    } else {
                                        M::KeepDefault
                                    }),
                                ],
                            )];
                            lines.extend(session.draft.capabilities.as_ref().unwrap().lines(ui));
                            ui.details(M::ShowDetails, &lines)?;
                        }
                        11 => {}
                        _ => return Err(Error::Cancelled),
                    }
                }
            }
            Ok(Transition::Next)
        })();
        match outcome {
            Ok(Transition::Next) => {}
            Ok(Transition::Leave(configured)) => return Ok(configured),
            Err(Error::Back) => {
                if !session.back() {
                    return Ok(None);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn suggested_name(snapshot: &Snapshot, model: &str) -> String {
    let base: String = model
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    let base = if validate_name(&base).is_ok() {
        base
    } else {
        "my-model".into()
    };
    let mut name = base.clone();
    let mut suffix = 2;
    while snapshot.entry(&name).is_some() {
        name = format!("{base}-{suffix}");
        suffix += 1;
    }
    name
}
fn name(ui: &mut impl Questions, snapshot: &Snapshot, default: &str) -> Result<String> {
    ui.input(
        &ui.label(M::LocalName),
        default,
        Field::Name(snapshot.entries().map(|(id, _)| id.to_owned()).collect()),
        &[],
    )
}
fn select_model(
    ui: &mut impl Questions,
    network: &mut impl SetupNetwork,
    draft: &mut Draft,
) -> Result<bool> {
    loop {
        let manual = providers::manual_models(&draft.spec);
        let mut options = if manual {
            ui.say(M::ManualModelsNotice, &[]);
            vec![M::ManualModel]
        } else {
            let (url, _) = discovery::endpoint(&draft.spec)?;
            ui.say(M::DiscoveryNotice, &[url.as_str()]);
            vec![M::FetchModels, M::ManualModel]
        };
        if !draft.listed.is_empty() {
            options.push(M::CachedModels);
        }
        if draft.capabilities.is_some() {
            options.push(M::KeepModel);
        }
        options.extend([M::EditConnection, M::Back]);
        let action = options[choose(ui, M::ModelMenu, &options)?];
        let result = match action {
            M::FetchModels => {
                ui.busy(M::PendingModels)?;
                match network.models(&draft.spec) {
                    Ok(models) => draft.listed = models,
                    Err(error) => {
                        ui.say(M::DiscoveryFailed, &[&error]);
                        continue;
                    }
                }
                pick_list(ui, draft)
            }
            M::ManualModel => (|| {
                let suggestions = draft
                    .listed
                    .iter()
                    .map(|m| m.id.clone())
                    .collect::<Vec<_>>();
                let id = ui.input(
                    &ui.label(M::ModelIdentifier),
                    draft.spec["model"].as_str().unwrap_or(""),
                    Field::Model,
                    &suggestions,
                )?;
                let model = draft
                    .listed
                    .iter()
                    .find(|m| m.id == id)
                    .cloned()
                    .unwrap_or_else(|| discovery::Model {
                        id,
                        profile: json!({}),
                        input_limit: None,
                    });
                draft.model(&model);
                Ok(true)
            })(),
            M::CachedModels => pick_list(ui, draft),
            M::KeepModel => return Ok(true),
            _ => return Ok(false),
        };
        match result {
            Ok(true) => return Ok(true),
            Ok(false) | Err(Error::Back) => {}
            Err(e) => return Err(e),
        }
    }
}
fn pick_list(ui: &mut impl Questions, draft: &mut Draft) -> Result<bool> {
    if draft.listed.is_empty() {
        ui.say(M::EmptyModels, &[]);
        return Ok(false);
    }
    let mut options: Vec<_> = draft.listed.iter().map(|m| m.id.clone()).collect();
    options.push(ui.label(M::Back));
    let selected = ui.select(&ui.label(M::AvailableModels), &options)?;
    if let Some(model) = draft.listed.get(selected).cloned() {
        draft.model(&model);
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
#[path = "wizard_tests.rs"]
mod tests;
