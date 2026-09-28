//! Derived recovery state for official consumers. This is not an execution
//! checkpoint: only data folded from an already committed prefix belongs here.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::LogReader;

#[cfg(test)]
#[path = "log_checkpoint_tests.rs"]
mod tests;

const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Shared by all readers of one live history, including legacy ledgers.
/// Only unfinished calls survive the fold; no event bodies are retained.
#[derive(Default, Debug)]
pub(super) struct PendingCache {
    entries: std::collections::HashMap<String, (u64, crate::core_events::PendingCalls)>,
    #[cfg(test)]
    observed_headers: u64,
}

/// Derived state bound to an exact committed prefix, not an execution snapshot.
pub struct Checkpoint<T> {
    pub through: u64,
    pub state: Option<T>,
    /// A cold recovery is explicit, even when the cache simply does not exist.
    pub cold_reason: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    format: u32,
    consumer: String,
    version: u32,
    stream: String,
    through: u64,
    prefix: [u8; 32],
    body: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    data: Data,
    checksum: [u8; 32],
}

fn path(root: &Path, consumer: &str) -> PathBuf {
    root.join(format!(
        "checkpoint-{:x}.json",
        Sha256::digest(consumer.as_bytes())
    ))
}

fn read(path: &Path) -> io::Result<Stored> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_BYTES {
        return Err(io::Error::other(
            "recovery checkpoint exceeds its read budget or is not a file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::other(
            "recovery checkpoint grew beyond its read budget",
        ));
    }
    let stored: Stored = serde_json::from_slice(&bytes)?;
    let actual: [u8; 32] = Sha256::digest(serde_json::to_vec(&stored.data)?).into();
    if actual != stored.checksum {
        return Err(io::Error::other("recovery checkpoint checksum mismatch"));
    }
    Ok(stored)
}

impl LogReader {
    pub(super) fn pending_heads(&self, started_type: &str) -> io::Result<Vec<String>> {
        Ok(self.pending_calls(started_type)?.heads())
    }

    pub(crate) fn pending_requests(&self, started_type: &str) -> io::Result<Vec<String>> {
        Ok(self.pending_calls(started_type)?.requests())
    }

    fn pending_calls(&self, started_type: &str) -> io::Result<crate::core_events::PendingCalls> {
        let mut cache = self
            .pending
            .lock()
            .map_err(|_| io::Error::other("pending recovery cache lock poisoned"))?;
        let through = self.snapshot_end();
        let key = format!("kernel-pending:{started_type}");
        let (previous, mut state) = match cache.entries.get(started_type) {
            Some((previous, state)) => {
                if *previous == through {
                    return Ok(state.clone());
                }
                (*previous, state.clone())
            }
            None => {
                let checkpoint: super::Checkpoint<crate::core_events::PendingCalls> =
                    self.load_checkpoint(&key, 2, through)?;
                // A missing cache is an expected cold path, not a terminal
                // message. In particular, legacy ledgers cannot persist it.
                (
                    checkpoint.through,
                    checkpoint
                        .state
                        .unwrap_or_else(|| crate::core_events::PendingCalls::new(started_type)),
                )
            }
        };
        self.visit_header_range(previous + 1, through, |batch| {
            for header in batch {
                state.observe(header.relations());
                #[cfg(test)]
                {
                    cache.observed_headers += 1;
                }
            }
        })?;
        if let Err(error) = self.save_checkpoint(&key, 2, through, &state) {
            eprintln!("cannot save derived recovery state for {key}: {error}");
        }
        // Publish only after the entire suffix was read successfully. Readers
        // of another EventLog never share this projection, even for one path.
        cache
            .entries
            .insert(started_type.to_owned(), (through, state.clone()));
        Ok(state)
    }

    /// Load consumer-owned data only when its version and source prefix match.
    /// Invalid derived state requests cold recovery; unreadable source is an error.
    pub fn load_checkpoint<T: DeserializeOwned>(
        &self,
        consumer: &str,
        version: u32,
        through: u64,
    ) -> io::Result<Checkpoint<T>> {
        let cold = |reason: String| Checkpoint {
            through: 0,
            state: None,
            cold_reason: Some(reason),
        };
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        if history.checkpoint_digest(0)?.is_none() {
            return Ok(cold(
                "legacy or memory history has no segmented recovery cache".into(),
            ));
        }
        let root = self
            .path
            .as_ref()
            .ok_or_else(|| io::Error::other("segmented history has no path"))?;
        let stored = match read(&path(root, consumer)) {
            Ok(stored) => stored,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(cold(
                    "recovery checkpoint has not been created yet; rebuilding from the ledger"
                        .into(),
                ));
            }
            Err(error) => return Ok(cold(error.to_string())),
        };
        let data = stored.data;
        if data.format != 1
            || data.consumer != consumer
            || data.version != version
            || data.stream != self.stream
            || data.through > through
            || data.through > history.len() as u64
        {
            return Ok(cold(
                "recovery checkpoint identity, version or boundary mismatch".into(),
            ));
        }
        // Failure to read SOURCE metadata is fatal, not a reason to invent an
        // empty state. Only a bad derived cache is allowed to fall back.
        if history.checkpoint_digest(data.through)? != Some(data.prefix) {
            return Ok(cold(
                "recovery checkpoint belongs to a different ledger prefix".into(),
            ));
        }
        match serde_json::from_str(&data.body) {
            Ok(state) => Ok(Checkpoint {
                through: data.through,
                state: Some(state),
                cold_reason: None,
            }),
            Err(error) => Ok(cold(format!("incompatible recovery state: {error}"))),
        }
    }

    /// Publish pure data folded from a committed prefix. Returns false for
    /// histories without a persistent recovery cache; never stores execution state.
    pub fn save_checkpoint<T: Serialize>(
        &self,
        consumer: &str,
        version: u32,
        through: u64,
        state: &T,
    ) -> io::Result<bool> {
        // Serializes competing publishers sharing this reader and prevents a
        // late worker from replacing a newer checkpoint with an older one.
        let history = self
            .history
            .write()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        let Some(prefix) = history.checkpoint_digest(through)? else {
            return Ok(false);
        };
        let root = self
            .path
            .as_ref()
            .ok_or_else(|| io::Error::other("segmented history has no path"))?;
        let target = path(root, consumer);
        if let Ok(prior) = read(&target) {
            let data = prior.data;
            if data.format == 1
                && data.consumer == consumer
                && data.version == version
                && data.stream == self.stream
                && data.through > through
                && data.through <= history.len() as u64
                && history.checkpoint_digest(data.through)? == Some(data.prefix)
            {
                return Ok(false);
            }
        }
        let data = Data {
            format: 1,
            consumer: consumer.into(),
            version,
            stream: self.stream.clone(),
            through,
            prefix,
            body: serde_json::to_string(state)?,
        };
        let checksum = Sha256::digest(serde_json::to_vec(&data)?).into();
        let bytes = serde_json::to_vec(&Stored { data, checksum })?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::other(
                "recovery checkpoint exceeds its write budget",
            ));
        }
        let temporary = target.with_extension("next");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&temporary)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other(
                "recovery checkpoint temporary is not a file",
            ));
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        File::open(root)?.sync_all()?;
        Ok(true)
    }
}
