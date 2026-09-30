//! Successful activation is separate from a reusable permission to attempt it.
//! Atomic replacement is the state commit; a missing tool reply does not undo it.
//! Restoring this file only reads state and never repeats the activating operation.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{digest, Candidate, Identity};
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

/// App-owned activation state is outside project-authored definition files.
/// This is not a boundary against arbitrary processes modifying local state.
#[derive(Clone, Debug)]
pub struct Activations {
    directory: PathBuf,
}

impl Activations {
    pub fn new(home: &Path) -> Self {
        Self {
            directory: home.join(".lattice/expert-activations"),
        }
    }

    fn path(&self, identity: &Identity) -> PathBuf {
        let hash = digest(&serde_json::to_vec(identity).expect("expert identity is JSON data"));
        self.directory.join(format!("{}.json", &hash[7..]))
    }

    pub fn read(&self, identity: &Identity) -> Result<Option<Activation>, String> {
        let path = self.path(identity);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read expert activation: {error}")),
        };
        let record: Activation = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid expert activation: {error}"))?;
        if record.v != 1 || &record.identity != identity {
            return Err("expert activation has an unsupported version or wrong identity".into());
        }
        Ok(Some(record))
    }

    /// All managed writers serialize on the same stable sidecar. The expected
    /// record detects a competing managed activation rather than overwriting it.
    /// This lock does not constrain external editors; reads still compare the
    /// actual definition revision and verify the referenced authorization.
    pub fn commit(&self, record: &Activation, expected: Option<&Activation>) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| format!("cannot create activation directory: {error}"))?;
        let path = self.path(&record.identity);
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .map_err(|error| format!("cannot open activation lock: {error}"))?;
        lock.lock()
            .map_err(|error| format!("cannot lock activation: {error}"))?;
        let current = self.read(&record.identity)?;
        if current.as_ref() != expected {
            return Err("expert activation changed; inspect it again before retrying".into());
        }
        if record.v != 1 {
            return Err("unsupported expert activation version".into());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)
            .map_err(|error| format!("cannot stage activation: {error}"))?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
        temporary
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        temporary
            .persist(&path)
            .map_err(|error| format!("cannot commit activation: {error}"))?;
        Ok(())
    }
}
