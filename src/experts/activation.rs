//! Successful activation is separate from a reusable permission to attempt it.
//! Atomic replacement is the state commit; a missing tool reply does not undo it.
//! Restoring this file only reads state and never repeats the activating operation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

pub use super::state::Activations;
use super::{Candidate, Identity};
use crate::{core_events as ce, EventEnvelope};

pub const ACTIVATE: &str = "ActivateExpert";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRef {
    pub ledger: PathBuf,
    pub stream: String,
    pub event: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub v: u32,
    pub identity: Identity,
    pub revision: String,
    pub authorization: AuditRef,
}

pub(crate) fn check_candidate(
    candidate: &Candidate,
    arguments: &serde_json::Value,
) -> Result<(), String> {
    if arguments["operation"] != "activate"
        || arguments["target"] != json!(candidate.identity)
        || arguments["definition"] != json!(candidate.definition)
        || arguments["fileVersion"] != candidate.file_version
    {
        return Err("activation arguments do not match the inspected expert revision".into());
    }
    Ok(())
}

impl Activation {
    /// Called only after the real gate has forwarded a request. The expected
    /// source is the configured gate instance, not a caller-supplied argument.
    pub fn approved(
        candidate: &Candidate,
        event: &EventEnvelope,
        ledger: &Path,
        gate: &str,
    ) -> Result<Self, String> {
        check_candidate(candidate, &event.payload["arguments"])?;
        if event.event_type != ce::TOOL_EXEC_STARTED
            || event.source != gate
            || event.payload["tool"] != ACTIVATE
        {
            return Err("authorization does not match this exact expert activation".into());
        }
        Ok(Self {
            v: 1,
            identity: candidate.identity.clone(),
            revision: candidate.definition.revision(),
            authorization: AuditRef {
                ledger: ledger.to_owned(),
                stream: event.stream.clone(),
                event: event.id.clone(),
            },
        })
    }

    pub fn matches(&self, candidate: &Candidate) -> bool {
        self.v == 1
            && self.identity == candidate.identity
            && self.revision == candidate.definition.revision()
    }

    /// A stored pointer is not proof by itself. Its source event must still bind
    /// the same identity and content; do not turn missing evidence into approval.
    pub fn verify(
        &self,
        candidate: &Candidate,
        event: &EventEnvelope,
        gate: &str,
    ) -> Result<(), String> {
        if !self.matches(candidate)
            || event.id != self.authorization.event
            || event.stream != self.authorization.stream
        {
            return Err(
                "expert activation does not match the current definition or evidence".into(),
            );
        }
        // Formatting can change without changing approved semantics. Validate
        // the original file token against the original request, not today's bytes.
        let original = Candidate {
            identity: candidate.identity.clone(),
            definition: candidate.definition.clone(),
            file_version: event.payload["arguments"]["fileVersion"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or("activation evidence has no original file version")?
                .into(),
        };
        Self::approved(&original, event, &self.authorization.ledger, gate)?;
        Ok(())
    }
}
