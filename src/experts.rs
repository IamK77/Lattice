//! Product-owned reusable expert definitions. The kernel does not interpret these.

pub mod activation;
pub mod catalog;
pub mod execution;
pub mod management;
pub mod state;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Capability groups select shipped providers, not arbitrary executable entries.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    Read,
    Write,
    Web,
    Commands,
    SkillInstall,
}

impl Capability {
    pub fn instances(self) -> &'static [&'static str] {
        match self {
            Self::Read => &["fs", "search"],
            Self::Write => &["fs-write"],
            Self::Web => &["net", "search-web"],
            Self::Commands => &["shell"],
            Self::SkillInstall => &["skill-installer"],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub v: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub model: String,
    pub capabilities: Vec<Capability>,
}

impl Definition {
    pub fn validate(&self) -> Result<(), String> {
        if self.v != 1 {
            return Err(format!("unsupported expert definition version: {}", self.v));
        }
        validate_id(&self.id)?;
        for (field, value) in [
            ("name", &self.name),
            ("description", &self.description),
            ("instructions", &self.instructions),
            ("model", &self.model),
        ] {
            if value.trim().is_empty() {
                return Err(format!("expert {field} must not be empty"));
            }
        }
        let unique: HashSet<_> = self.capabilities.iter().collect();
        if unique.len() != self.capabilities.len() {
            return Err("expert capability groups must not be repeated".into());
        }
        Ok(())
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let definition: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid expert definition: {error}"))?;
        definition.validate()?;
        Ok(definition)
    }

    /// Semantic revision. Formatting alone does not introduce new instructions.
    pub fn revision(&self) -> String {
        digest(&serde_json::to_vec(self).expect("an expert definition is JSON data"))
    }

    pub fn tool_instances(&self) -> Vec<&'static str> {
        self.capabilities
            .iter()
            .flat_map(|group| group.instances().iter().copied())
            .collect()
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
    {
        return Err("expert id must start with a lowercase ASCII letter and contain at most 64 lowercase letters, digits, hyphens or underscores".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Project,
    Personal,
}

/// Authority comes from the configured root, never from fields inside a definition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub scope: Scope,
    pub root: PathBuf,
    pub id: String,
}

impl Identity {
    pub fn name(&self) -> String {
        let scope = match self.scope {
            Scope::Project => "project",
            Scope::Personal => "personal",
        };
        format!("{scope}:{}", self.id)
    }
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub identity: Identity,
    pub definition: Definition,
    /// Exact bytes inspected, used for mutation conflicts independently of semantic approval.
    pub file_version: String,
}

/// Paths are supplied by the product. Tests do not mutate process-wide HOME or cwd.
#[derive(Clone, Debug)]
pub struct Definitions {
    project: PathBuf,
    personal: PathBuf,
}

impl Definitions {
    pub fn new(workspace: &Path, home: &Path) -> Result<Self, String> {
        let project = std::fs::canonicalize(workspace)
            .map_err(|error| format!("cannot resolve expert workspace: {error}"))?;
        let personal = std::fs::canonicalize(home)
            .map_err(|error| format!("cannot resolve expert home: {error}"))?;
        Ok(Self { project, personal })
    }

    pub fn identity(&self, scope: Scope, id: &str) -> Result<Identity, String> {
        validate_id(id)?;
        Ok(Identity {
            scope,
            root: match scope {
                Scope::Project => self.project.clone(),
                Scope::Personal => self.personal.clone(),
            },
            id: id.into(),
        })
    }

    pub fn path(&self, scope: Scope, id: &str) -> Result<PathBuf, String> {
        let identity = self.identity(scope, id)?;
        Ok(identity
            .root
            .join(".lattice/expert-definitions")
            .join(format!("{id}.json")))
    }

    pub fn read(&self, scope: Scope, id: &str) -> Result<Candidate, String> {
        self.find(scope, id)?.ok_or_else(|| {
            format!(
                "expert definition {}:{id} does not exist",
                match scope {
                    Scope::Project => "project",
                    Scope::Personal => "personal",
                }
            )
        })
    }

    /// Only absence is empty. Broken JSON, links and unreadable files remain errors.
    pub fn find(&self, scope: Scope, id: &str) -> Result<Option<Candidate>, String> {
        use std::io::Read;
        let identity = self.identity(scope, id)?;
        let path = self.path(scope, id)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot open {}: {error}", path.display())),
        };
        let metadata = file
            .metadata()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!(
                "expert definition must be a regular file: {}",
                path.display()
            ));
        }
        // Bound malformed or accidentally huge authoring files before parsing JSON.
        const MAX_BYTES: u64 = 1024 * 1024;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("expert definition exceeds 1 MiB".into());
        }
        let definition = Definition::parse(&bytes)?;
        if definition.id != id {
            return Err("expert definition id does not match its file name".into());
        }
        Ok(Some(Candidate {
            identity,
            definition,
            file_version: digest(&bytes),
        }))
    }
}

#[cfg(test)]
mod activation_tests;
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod definition_schema_tests;
#[cfg(test)]
mod management_pipeline_tests;
#[cfg(test)]
mod tests;
