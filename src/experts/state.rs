//! Current availability, including non-active revisions. Markers never drive replay.

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::activation::{Activation, AuditRef};
use super::{digest, Identity};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InactiveKind {
    Pending,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inactive {
    pub v: u32,
    pub identity: Identity,
    pub state: InactiveKind,
    pub change: AuditRef,
}

/// Existing v1 activation files remain readable without rewriting them. A v2
/// marker changes the expected state even when identical content is recreated.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Record {
    Active(Activation),
    Inactive(Inactive),
}

impl Record {
    pub fn active(&self) -> Option<&Activation> {
        match self {
            Self::Active(record) => Some(record),
            Self::Inactive(_) => None,
        }
    }

    fn validate(&self, identity: &Identity) -> Result<(), String> {
        let valid = match self {
            Self::Active(record) => record.v == 1 && &record.identity == identity,
            Self::Inactive(record) => record.v == 2 && &record.identity == identity,
        };
        if valid {
            Ok(())
        } else {
            Err("expert state has an unsupported version or wrong identity".into())
        }
    }
}

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

    pub fn state(&self, identity: &Identity) -> Result<Option<Record>, String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(self.path(identity)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read expert activation: {error}")),
        };
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("expert activation must be a regular file".into());
        }
        const MAX_BYTES: u64 = 64 * 1024;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("expert activation exceeds 64 KiB".into());
        }
        let record: Record = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid expert activation: {error}"))?;
        record.validate(identity)?;
        Ok(Some(record))
    }

    /// Active-only view. Managed comparisons use `state`, preserving markers.
    pub fn read(&self, identity: &Identity) -> Result<Option<Activation>, String> {
        Ok(self
            .state(identity)?
            .and_then(|record| record.active().cloned()))
    }

    /// The same stable lock covers definition writes, deletion, activation and
    /// snapshot capture. Never unlink it when deleting the definition.
    pub(crate) fn lock(&self, identity: &Identity) -> Result<StateGuard<'_>, String> {
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| format!("cannot create activation directory: {error}"))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(self.path(identity).with_extension("lock"))
            .map_err(|error| format!("cannot open activation lock: {error}"))?;
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("expert activation lock must be a regular file".into());
        }
        file.lock()
            .map_err(|error| format!("cannot lock activation: {error}"))?;
        Ok(StateGuard {
            store: self,
            identity: identity.clone(),
            _lock: file,
        })
    }

    pub fn commit(&self, record: &Activation, expected: Option<&Activation>) -> Result<(), String> {
        self.lock(&record.identity)?.commit(
            &Record::Active(record.clone()),
            expected
                .map(|record| Record::Active(record.clone()))
                .as_ref(),
        )
    }
}

pub(crate) struct StateGuard<'a> {
    store: &'a Activations,
    identity: Identity,
    _lock: File,
}

impl StateGuard<'_> {
    pub fn read(&self) -> Result<Option<Record>, String> {
        self.store.state(&self.identity)
    }

    pub fn check(&self, expected: Option<&Record>) -> Result<(), String> {
        if self.read()?.as_ref() != expected {
            return Err("expert activation changed; inspect it again before retrying".into());
        }
        Ok(())
    }

    pub fn commit(&self, record: &Record, expected: Option<&Record>) -> Result<(), String> {
        self.check(expected)?;
        record.validate(&self.identity)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.store.directory)
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
            .persist(self.store.path(&self.identity))
            .map_err(|error| format!("cannot commit activation: {error}"))?;
        Ok(())
    }
}
