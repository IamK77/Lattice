//! User-restorable file state, separate from disposable startup checkpoints.
//!
//! The archive layer never truncates a ledger or executes historical requests.
//! Product integration must obtain a stable conversation boundary, publish a
//! restore plan to the ledger, and obtain confirmation before invoking file
//! restoration. The file layer cannot stop arbitrary external writers.

use std::io;

use serde::{Deserialize, Serialize};

/// Content identity, not a caller-selected disk path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectRef {
    pub sha256: String,
    pub bytes: u64,
}

impl ObjectRef {
    pub fn validate(&self) -> io::Result<()> {
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid file archive content address",
            ));
        }
        Ok(())
    }
}

pub const FILE_ARCHIVE_SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "linux"));

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod files;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod fs;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod objects;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod permissions;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod scope;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use files::{Archive, Change, FileState, Limits, Plan, Snapshot};
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use scope::{IgnoreKind, IgnoreRule, ScopeInput};

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
