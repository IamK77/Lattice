//! Credential-only repair keeps the original catalog payload and an unsaved draft.
use super::{
    api_name, change_language, choose, connection_context, credential, Error, Field, Questions,
    Result, M,
};
use lattice::models::catalog::Snapshot;
use serde_json::{json, Value};
use std::path::Path;

pub(super) struct Draft {
    pub original: Value,
    pub spec: Value,
    pub id: String,
    pub preferred: bool,
    ready: bool,
    prompt_key: bool,
    existing: bool,
}
impl Draft {
    pub fn new(original: Value, id: String, needs_key: bool, existing: bool) -> Self {
        Self {
            spec: original.clone(),
            original,
            id,
            preferred: true,
            ready: !needs_key,
            prompt_key: needs_key,
            existing,
        }
    }
    pub fn configure(
        &mut self,
        ui: &mut impl Questions,
        preferences: Option<&Path>,
        path: &Path,
        snapshot: &Snapshot,
    ) -> Result<bool> {
        loop {
            if self.prompt_key {
                ui.page(M::ConnectionStage, &connection_context(&self.spec));
                match credential(ui) {
                    Ok((field, value)) => {
                        self.spec.as_object_mut().unwrap().remove("apiKey");
                        self.spec.as_object_mut().unwrap().remove("apiKeyEnv");
                        self.spec[field] = json!(value);
                        self.ready = true;
                    }
                    Err(Error::Back) => {}
                    Err(e) => return Err(e),
                }
                self.prompt_key = false;
            }
            ui.page(M::PageTitle, &[]);
            ui.say(
                M::CompactSummary,
                &[
                    self.spec["model"].as_str().unwrap(),
                    api_name(self.spec["adapter"].as_str().unwrap()),
                    self.spec["baseUrl"].as_str().unwrap(),
                    &self.id,
                    &ui.label(if self.preferred {
                        M::SetDefault
                    } else {
                        M::KeepDefault
                    }),
                ],
            );
            ui.say(
                if self.spec.get("apiKey").is_some() {
                    M::CredentialLocal
                } else {
                    M::CredentialEnv
                },
                &[],
            );
            if self.spec["baseUrl"]
                .as_str()
                .is_some_and(|s| s.starts_with("http://"))
            {
                ui.say(M::HttpWarning, &[]);
            }
            let mut options = vec![
                M::SaveContinue,
                M::ReplaceKey,
                M::ToggleDefault,
                M::LanguageMenu,
            ];
            if !self.existing {
                options.push(M::EditName);
            }
            options.extend([M::HomeBack, M::Exit, M::ShowDetails]);
            let action = match choose(ui, M::Review, &options) {
                Ok(choice) => options[choice],
                Err(Error::Back) => return Ok(false),
                Err(e) => return Err(e),
            };
            match action {
                M::SaveContinue if self.ready => return Ok(true),
                M::SaveContinue => {
                    ui.say(M::MissingKey, &[]);
                    self.prompt_key = true;
                }
                M::ReplaceKey => self.prompt_key = true,
                M::ToggleDefault => self.preferred = !self.preferred,
                M::LanguageMenu => match change_language(ui, preferences) {
                    Ok(()) | Err(Error::Back) => {}
                    Err(e) => return Err(e),
                },
                M::EditName => match ui.input(
                    &ui.label(M::LocalName),
                    &self.id,
                    Field::Name(snapshot.entries().map(|(id, _)| id.to_owned()).collect()),
                    &[],
                ) {
                    Ok(id) => self.id = id,
                    Err(Error::Back) => {}
                    Err(e) => return Err(e),
                },
                M::ShowDetails => ui.details(
                    M::ShowDetails,
                    &[ui.message(
                        M::ReviewSummary,
                        &[
                            &self.id,
                            self.spec["model"].as_str().unwrap(),
                            api_name(self.spec["adapter"].as_str().unwrap()),
                            self.spec["baseUrl"].as_str().unwrap(),
                            &path.display().to_string(),
                            &ui.label(if self.preferred {
                                M::SetDefault
                            } else {
                                M::KeepDefault
                            }),
                        ],
                    )],
                )?,
                M::Exit => return Err(Error::Cancelled),
                _ => return Ok(false),
            }
        }
    }
}
