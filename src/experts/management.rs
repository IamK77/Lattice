//! Shared, version-checked management. Saving and activating are distinct actions.

use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::activation::AuditRef;
use super::catalog::Catalog;
use super::state::{Inactive, InactiveKind, Record, StateGuard};
use super::{digest, Candidate, Definition, Identity};

pub const SAVE: &str = "SaveExpert";
pub const DELETE: &str = "DeleteExpert";
pub const LIST: &str = "ListExperts";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Put,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Mutation {
    pub operation: Operation,
    pub target: Identity,
    pub file_version: Option<String>,
    pub expected_activation: Option<Record>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<Definition>,
    pub reason: String,
}

impl Mutation {
    pub fn parse(value: &Value) -> Result<Self, String> {
        for field in ["fileVersion", "expectedActivation"] {
            if value.get(field).is_none() {
                return Err(format!(
                    "inspect the expected {field} before requesting a change"
                ));
            }
        }
        let request: Self =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        if request.reason.trim().is_empty() {
            return Err("expert management requires a nonempty reason".into());
        }
        match (request.operation, &request.definition, &request.file_version) {
            (Operation::Put, Some(definition), _) => {
                definition.validate()?;
                if definition.id != request.target.id { return Err("definition id must match the target".into()); }
            }
            (Operation::Delete, None, Some(_)) => {}
            _ => return Err("put requires a full definition; delete requires an existing version and no replacement definition".into()),
        }
        Ok(request)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub message: String,
    pub activation_changed: bool,
    pub definition_changed: bool,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            activation_changed: false,
            definition_changed: false,
        }
    }
}

struct Prepared {
    before: Option<Candidate>,
    bytes: Option<Vec<u8>>,
}

impl Catalog {
    fn prepare_mutation(
        &self,
        request: &Mutation,
        guard: &StateGuard<'_>,
    ) -> Result<Prepared, String> {
        let identity = self
            .definitions
            .identity(request.target.scope, &request.target.id)?;
        if identity != request.target {
            return Err("expert target does not match the configured source root".into());
        }
        guard.check(request.expected_activation.as_ref())?;
        let before = self.definitions.find(identity.scope, &identity.id)?;
        if before.as_ref().map(|candidate| &candidate.file_version) != request.file_version.as_ref()
        {
            return Err("expert definition changed; inspect it again before retrying".into());
        }
        let bytes = if let Some(definition) = &request.definition {
            let mut bytes =
                serde_json::to_vec_pretty(definition).map_err(|error| error.to_string())?;
            bytes.push(b'\n');
            if bytes.len() > 1024 * 1024 {
                return Err("expert definition exceeds 1 MiB".into());
            }
            let candidate = Candidate {
                identity,
                definition: definition.clone(),
                file_version: digest(&bytes),
            };
            self.model(&candidate)?;
            Some(bytes)
        } else {
            None
        };
        Ok(Prepared { before, bytes })
    }

    pub fn review_mutation(&self, arguments: &Value) -> Result<String, String> {
        let request = Mutation::parse(arguments)?;
        // Construct authority from configured roots, not from the request's path.
        let identity = self
            .definitions
            .identity(request.target.scope, &request.target.id)?;
        let guard = self.activations.lock(&identity)?;
        let prepared = self.prepare_mutation(&request, &guard)?;
        let before = prepared
            .before
            .as_ref()
            .map(|candidate| json!(candidate.definition))
            .unwrap_or(Value::Null);
        let action = match request.operation {
            Operation::Put => "Save expert definition (pending activation)",
            Operation::Delete => "Delete expert definition",
        };
        Ok(format!("{action}: {}\nSource: {}\nBefore:\n{}\nAfter:\n{}\n{}",
            identity.name(), identity.root.display(),
            serde_json::to_string_pretty(&before).map_err(|error| error.to_string())?,
            serde_json::to_string_pretty(&request.definition).map_err(|error| error.to_string())?,
            match request.operation {
                Operation::Put => "Saving does not activate the expert. Review and activate the saved revision next.",
                Operation::Delete => "Accepted work keeps running. Execution history is preserved. Recreating this definition requires fresh activation.",
            }))
    }

    /// The caller supplies an audited, authorized change reference. No operation
    /// here waits for a human or retries after a partial commit.
    pub(crate) fn apply_mutation(
        &self,
        arguments: &Value,
        change: AuditRef,
    ) -> Result<Value, Failure> {
        self.apply_mutation_with(arguments, change, || Ok(()))
    }

    fn apply_mutation_with(
        &self,
        arguments: &Value,
        change: AuditRef,
        after_state: impl FnOnce() -> Result<(), String>,
    ) -> Result<Value, Failure> {
        let request = Mutation::parse(arguments)?;
        let identity = self
            .definitions
            .identity(request.target.scope, &request.target.id)?;
        let guard = self.activations.lock(&identity)?;
        let prepared = self.prepare_mutation(&request, &guard)?;
        if request.operation == Operation::Put
            && prepared
                .before
                .as_ref()
                .is_some_and(|before| Some(&before.definition) == request.definition.as_ref())
        {
            let candidate = prepared
                .before
                .as_ref()
                .expect("checked existing definition");
            return Ok(
                json!({"unchanged":true,"details":self.inspect_state(candidate, request.expected_activation.as_ref(), None)}),
            );
        }
        let path = self.definitions.path(identity.scope, &identity.id)?;
        // Stage and sync new bytes before withdrawing old availability.
        let temporary = if let Some(bytes) = &prepared.bytes {
            let parent = path.parent().expect("definition has a parent");
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let mut temporary =
                tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
            temporary
                .write_all(bytes)
                .map_err(|error| error.to_string())?;
            temporary
                .as_file()
                .sync_all()
                .map_err(|error| error.to_string())?;
            Some(temporary)
        } else {
            None
        };
        let marker = Record::Inactive(Inactive {
            v: 2,
            identity: identity.clone(),
            state: if request.operation == Operation::Put {
                InactiveKind::Pending
            } else {
                InactiveKind::Deleted
            },
            change,
        });
        guard.commit(&marker, request.expected_activation.as_ref())?;
        let commit = (|| -> Result<(), String> {
            after_state()?;
            let observed = self.definitions.find(identity.scope, &identity.id)?;
            if observed.as_ref().map(|candidate| &candidate.file_version)
                != request.file_version.as_ref()
            {
                return Err(
                    "definition changed during commit; the external version was preserved".into(),
                );
            }
            if let Some(temporary) = temporary {
                if request.file_version.is_none() {
                    temporary
                        .persist_noclobber(&path)
                        .map_err(|error| error.to_string())?;
                } else {
                    temporary
                        .persist(&path)
                        .map_err(|error| error.to_string())?;
                }
            } else {
                std::fs::remove_file(&path).map_err(|error| error.to_string())?;
            }
            Ok(())
        })();
        if let Err(message) = commit {
            return Err(Failure {
                message: format!("Expert availability was withdrawn, but the definition change did not finish: {message}. Inspect the current state before a new operation."),
                activation_changed: true, definition_changed: false,
            });
        }
        if let (Some(definition), Some(bytes)) = (request.definition, prepared.bytes) {
            let candidate = Candidate {
                identity,
                definition,
                file_version: digest(&bytes),
            };
            Ok(
                json!({"saved":true,"ready":false,"details":self.inspect_state(&candidate, Some(&marker), None)}),
            )
        } else {
            Ok(json!({"deleted":true,"target":identity,"state":marker,"ready":false}))
        }
    }
}

#[cfg(test)]
mod tests;
