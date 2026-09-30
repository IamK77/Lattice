//! One resolver for inspection, activation and delegation. Definition presence
//! is not consent; a current revision also needs verifiable activation evidence.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::activation::{Activation, Activations};
use super::execution::Execution;
use super::state::Record;
use super::{Candidate, Definitions, Identity, Scope};
use crate::{EventEnvelope, LogReader};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub workspace: PathBuf,
    pub home: PathBuf,
    pub model_catalog: PathBuf,
    pub gate: String,
    pub defaults: crate::preset::PresetConfig,
}

/// Build a request from one inspected snapshot. This is not authorization:
/// providers still validate the exact version and state after human approval.
pub fn mutation_arguments(details: &Value, operation: &str) -> Result<Value, String> {
    if !matches!(operation, "put" | "activate" | "delete") {
        return Err("unknown expert mutation operation".into());
    }
    if !details["target"].is_object()
        || details.get("fileVersion").is_none()
        || details.get("activation").is_none()
    {
        return Err("inspect a custom expert before changing it".into());
    }
    if operation != "put" && !details["fileVersion"].is_string() {
        return Err("the inspected expert definition does not exist".into());
    }
    let mut arguments = json!({"operation":operation,"target":details["target"],
        "fileVersion":details["fileVersion"],"expectedActivation":details["activation"]});
    if operation != "delete" {
        arguments["definition"] = details["definition"].clone();
    }
    Ok(arguments)
}

pub struct Catalog {
    pub config: Config,
    pub definitions: Definitions,
    pub activations: Activations,
}

impl Catalog {
    pub fn new(config: Config) -> Result<Self, String> {
        let definitions = Definitions::new(&config.workspace, &config.home)?;
        let activations = Activations::new(&config.home);
        Ok(Self {
            config,
            definitions,
            activations,
        })
    }

    pub fn names(&self) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        for (scope, prefix) in [(Scope::Project, "project"), (Scope::Personal, "personal")] {
            let path = self.definitions.path(scope, "placeholder")?;
            let entries = match std::fs::read_dir(path.parent().expect("definition has a parent")) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("cannot list expert definitions: {error}")),
            };
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                let name = entry.file_name();
                let name = name.to_str().ok_or("expert definition name is not UTF-8")?;
                if let Some(id) = name.strip_suffix(".json") {
                    names.push(format!("{prefix}:{id}"));
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn qualify(&self, name: &str, builtin: bool) -> Result<String, String> {
        if name.contains(':') {
            return Ok(name.into());
        }
        let mut matches: Vec<_> = self
            .names()?
            .into_iter()
            .filter(|qualified| qualified.split_once(':').is_some_and(|(_, id)| id == name))
            .collect();
        if builtin {
            matches.push(format!("builtin:{name}"));
        }
        match matches.as_slice() {
            [only] => Ok(only.clone()),
            [] => Err(format!("no expert named {name}")),
            _ => Err(format!(
                "expert name {name} is ambiguous; choose {}",
                matches.join(", ")
            )),
        }
    }

    pub fn listing(&self, current: Option<&LogReader>) -> Result<Vec<Value>, String> {
        self.names()?.into_iter().map(|name| {
            let entry = match self.inspect(&name, current) {
                Ok(details) => json!({"name":name,"displayName":details["definition"]["name"],"state":details["state"],"description":details["definition"]["description"],"ready":details["ready"],"unavailable":details["unavailable"]}),
                Err(error) => json!({"name":name,"ready":false,"unavailable":error}),
            };
            Ok(entry)
        }).collect()
    }

    pub fn management_listing(&self, current: Option<&LogReader>) -> Result<Value, String> {
        let mut experts: Vec<Value> = crate::preset::EXPERTS
            .iter()
            .map(|expert| {
                json!({
                    "name":format!("builtin:{}",expert.name),"description":expert.description,
                    "ready":true,"state":"builtin","builtin":true
                })
            })
            .collect();
        experts.extend(self.listing(current)?);
        let models = std::fs::read_to_string(&self.config.model_catalog)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                serde_json::from_str::<Value>(&text).map_err(|error| error.to_string())
            })
            .and_then(|document| {
                document["models"]
                    .as_object()
                    .map(|models| models.keys().cloned().collect::<Vec<_>>())
                    .ok_or("model catalog requires a models object".into())
            });
        Ok(
            json!({"experts":experts,"models":models.as_ref().ok(),"modelProblem":models.err(),"toolRoot":self.config.defaults.workspace,
            "projectRoot":self.definitions.identity(Scope::Project,"placeholder")?.root,
            "personalRoot":self.definitions.identity(Scope::Personal,"placeholder")?.root}),
        )
    }

    fn builtin_details(&self, id: &str) -> Result<Value, String> {
        let expert = crate::preset::EXPERTS
            .iter()
            .find(|expert| expert.name == id)
            .ok_or("unknown built-in expert")?;
        let groups = [
            super::Capability::Read,
            super::Capability::Write,
            super::Capability::Web,
            super::Capability::Commands,
            super::Capability::SkillInstall,
        ];
        let capabilities: Vec<_> = groups
            .into_iter()
            .filter(|group| {
                group
                    .instances()
                    .iter()
                    .all(|instance| expert.tools.contains(instance))
            })
            .collect();
        let expanded: std::collections::HashSet<_> = capabilities
            .iter()
            .flat_map(|group| group.instances().iter().copied())
            .collect();
        if expert.tools.iter().any(|tool| !expanded.contains(tool)) {
            return Err(
                "built-in capabilities cannot be represented by the supported groups".into(),
            );
        }
        Ok(
            json!({"name":format!("builtin:{id}"),"builtin":true,"state":"builtin","ready":true,"toolRoot":self.config.defaults.workspace,
            "copyTemplate":{"v":1,"id":id,"name":id,"description":expert.description,
                "instructions":expert.prompt,"model":"","capabilities":capabilities},
            "note":"Choose a project or personal identity and an existing model when copying this built-in. Activation is not copied."}),
        )
    }

    pub fn candidate(&self, name: &str) -> Result<Candidate, String> {
        let identity = self.identity(name)?;
        self.definitions.read(identity.scope, &identity.id)
    }

    pub fn identity(&self, name: &str) -> Result<Identity, String> {
        let (scope, id) = name
            .split_once(':')
            .ok_or("qualify the expert as project:<id> or personal:<id>")?;
        let scope = match scope {
            "project" => Scope::Project,
            "personal" => Scope::Personal,
            "builtin" => {
                return Err("built-in experts are read-only; copy a definition instead".into())
            }
            _ => return Err("unknown expert definition scope".into()),
        };
        self.definitions.identity(scope, id)
    }

    /// Parse the inspected bytes once using the same catalog normalization as
    /// the main product. Never use models::find's synthetic current-model row.
    pub fn model(&self, candidate: &Candidate) -> Result<crate::models::Entry, String> {
        let text = std::fs::read_to_string(&self.config.model_catalog)
            .map_err(|error| format!("cannot read expert model catalog: {error}"))?;
        let document: Value = serde_json::from_str(&text)
            .map_err(|error| format!("invalid expert model catalog: {error}"))?;
        let id = &candidate.definition.model;
        let entry = document
            .get("models")
            .and_then(|models| models.get(id))
            .ok_or_else(|| format!("expert model {id} is not in the configured catalog"))?;
        // An unrelated broken entry does not make a healthy selection disappear.
        let selected = json!({"models": {id: entry}});
        let (mut entries, complaints) =
            crate::models::merge(&selected, &self.config.model_catalog.display().to_string());
        if !complaints.is_empty() {
            return Err(complaints.join("; "));
        }
        let model = entries
            .pop()
            .ok_or_else(|| format!("expert model {id} is unavailable"))?;
        if model.adapter != "scripted" && !model.key_present() {
            return Err(format!("expert model {id} has no available credential"));
        }
        Ok(model)
    }

    fn evidence(
        &self,
        activation: &Activation,
        current: Option<&LogReader>,
    ) -> Result<EventEnvelope, String> {
        let reference = &activation.authorization;
        if let Some(reader) = current.filter(|reader| {
            reader.stream() == reference.stream
                && reader.path().is_some_and(|path| {
                    path == reference.ledger
                        || path
                            .canonicalize()
                            .is_ok_and(|path| path == reference.ledger)
                })
        }) {
            return reader
                .get(&reference.event)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "expert activation evidence is missing".into());
        }
        // Historical access is read-only: never open an EventLog writer merely
        // to inspect evidence. This also supports legacy JSONL authorization.
        for line in crate::ledgers::lines(&reference.ledger)
            .map_err(|error| format!("cannot read activation evidence: {error}"))?
        {
            let line = line.map_err(|error| error.to_string())?;
            let event: EventEnvelope = serde_json::from_str(&line)
                .map_err(|error| format!("invalid activation evidence: {error}"))?;
            if event.v != crate::ENVELOPE_VERSION {
                return Err("unsupported activation evidence version".into());
            }
            if event.id == reference.event {
                return Ok(event);
            }
        }
        Err("expert activation evidence is missing".into())
    }

    pub fn resolve(&self, name: &str, current: Option<&LogReader>) -> Result<Execution, String> {
        let identity = self.identity(name)?;
        let guard = self.activations.lock(&identity)?;
        let candidate = self.definitions.read(identity.scope, &identity.id)?;
        let state = guard.read()?;
        self.resolve_candidate(&candidate, state.as_ref().and_then(Record::active), current)
    }

    fn resolve_candidate(
        &self,
        candidate: &Candidate,
        activation: Option<&Activation>,
        current: Option<&LogReader>,
    ) -> Result<Execution, String> {
        let activation = activation.ok_or("expert definition is pending activation")?;
        if !activation.matches(candidate) {
            return Err("current expert revision is pending activation; the old revision is not substituted".into());
        }
        let evidence = self.evidence(activation, current)?;
        activation.verify(candidate, &evidence, &self.config.gate)?;
        let model = self.model(candidate)?;
        Execution::capture(candidate, activation, &model, &self.config.defaults)
    }

    pub fn inspect(&self, name: &str, current: Option<&LogReader>) -> Result<Value, String> {
        if let Some(id) = name.strip_prefix("builtin:") {
            return self.builtin_details(id);
        }
        let identity = self.identity(name)?;
        let guard = self.activations.lock(&identity)?;
        let state = guard.read()?;
        match self.definitions.find(identity.scope, &identity.id)? {
            Some(candidate) => Ok(self.inspect_state(&candidate, state.as_ref(), current)),
            None => Ok(
                json!({"target":identity,"definition":null,"fileVersion":null,
                "activation":state,"ready":false,"unavailable":"expert definition does not exist",
                "state":"absent","toolRoot":self.config.defaults.workspace}),
            ),
        }
    }

    #[cfg(test)]
    pub(super) fn inspect_candidate(
        &self,
        candidate: &Candidate,
        activation: Option<&Activation>,
        current: Option<&LogReader>,
    ) -> Value {
        self.inspect_state(
            candidate,
            activation
                .map(|record| Record::Active(record.clone()))
                .as_ref(),
            current,
        )
    }

    pub(super) fn inspect_state(
        &self,
        candidate: &Candidate,
        state: Option<&Record>,
        current: Option<&LogReader>,
    ) -> Value {
        let activation = state.and_then(Record::active);
        let availability = self.resolve_candidate(candidate, activation, current);
        let status = if availability.is_ok() {
            "ready"
        } else if activation.is_some_and(|record| record.matches(candidate)) {
            "unavailable"
        } else {
            "pending"
        };
        json!({
            "target":candidate.identity,"definition":candidate.definition,
            "fileVersion":candidate.file_version,"activation":state,"state":status,
            "ready":availability.is_ok(),"unavailable":availability.err(),
            "toolRoot":self.config.defaults.workspace
        })
    }

    pub fn review(&self, arguments: &Value) -> Result<String, String> {
        let target: super::Identity = serde_json::from_value(arguments["target"].clone())
            .map_err(|error| format!("invalid activation target: {error}"))?;
        let identity = self.definitions.identity(target.scope, &target.id)?;
        let guard = self.activations.lock(&identity)?;
        let candidate = self.definitions.read(target.scope, &target.id)?;
        super::activation::check_candidate(&candidate, arguments)?;
        if arguments["reason"]
            .as_str()
            .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err("expert activation requires a nonempty reason".into());
        }
        if arguments.get("expectedActivation").is_none() {
            return Err("inspect the expected activation before requesting approval".into());
        }
        let expected: Option<Record> =
            serde_json::from_value(arguments["expectedActivation"].clone())
                .map_err(|error| error.to_string())?;
        if guard.read()? != expected {
            return Err("expert activation changed; inspect it again before approval".into());
        }
        let model = self.model(&candidate)?;
        let prior = expected
            .as_ref()
            .and_then(Record::active)
            .map(|old| old.revision.as_str())
            .unwrap_or("not activated");
        Ok(format!("{}\nWorkspace/source: {}\nPrevious: {}\nReviewed revision: {}\nModel: {} ({})\nCapabilities: {}\nStanding instructions:\n{}",
            candidate.identity.name(), candidate.identity.root.display(), prior,
            candidate.definition.revision(), model.id, model.adapter,
            serde_json::to_string(&candidate.definition.capabilities).map_err(|error|error.to_string())?,
            candidate.definition.instructions))
    }

    pub fn activate(
        &self,
        event: &EventEnvelope,
        reader: &LogReader,
    ) -> Result<Activation, String> {
        self.activate_inspected(event, reader)
            .map(|(activation, _)| activation)
    }

    /// Return the exact committed snapshot, not a later read that can race a save.
    pub fn activate_inspected(
        &self,
        event: &EventEnvelope,
        reader: &LogReader,
    ) -> Result<(Activation, Value), String> {
        let target: super::Identity =
            serde_json::from_value(event.payload["arguments"]["target"].clone())
                .map_err(|error| format!("invalid activation target: {error}"))?;
        let identity = self.definitions.identity(target.scope, &target.id)?;
        let guard = self.activations.lock(&identity)?;
        let candidate = self.definitions.read(target.scope, &target.id)?;
        let expected: Option<Record> =
            serde_json::from_value(event.payload["arguments"]["expectedActivation"].clone())
                .map_err(|error| format!("invalid expected activation: {error}"))?;
        let ledger = reader
            .path()
            .ok_or("expert activation requires a persistent audit ledger")?;
        let ledger = std::fs::canonicalize(ledger)
            .map_err(|error| format!("cannot locate activation audit ledger: {error}"))?;
        let activation = Activation::approved(&candidate, event, &ledger, &self.config.gate)?;
        // Fail unavailable dependencies before changing activation state. This
        // captures and checks a recipe but never starts a child or a provider.
        let model = self.model(&candidate)?;
        Execution::capture(&candidate, &activation, &model, &self.config.defaults)?;
        let state = Record::Active(activation.clone());
        guard.commit(&state, expected.as_ref())?;
        let details = self.inspect_state(&candidate, Some(&state), Some(reader));
        Ok((activation, details))
    }
}
