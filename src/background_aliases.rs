//! Stream-scoped aliases for background invocations, not execution identities.
//!
//! Replay first identity-bearing receipts/wakes in ledger order, including
//! retired work. Do not sort by request order: an older request can finish late.
//! Reserving every exposed alias makes collision handling deterministic.
//! This module neither starts work nor grants permission to operate on it.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub mod operations;
mod presentation;
mod projection;
pub(crate) use presentation::reference_for_event;
pub use projection::{identity_for_event, Projection};

const DEFAULT_LENGTH: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Expert,
    Run,
    Timer,
    Watch,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OriginalId {
    Number(u64),
    Text(String),
}

/// The creation event distinguishes invocations when a legacy ID was reused.
/// Labels, command contents, status, and render positions are deliberately absent.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub stream: String,
    pub kind: Kind,
    pub original_id: OriginalId,
    pub created_by: String,
}

/// A presentation reference keeps its owning stream and invocation identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub alias: String,
    pub identity: Identity,
}

impl std::fmt::Display for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} {}; stream={:?}",
            self.identity.kind, self.alias, self.identity.stream
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssignmentError {
    WrongStream,
    HashExhausted,
}

impl std::fmt::Display for AssignmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongStream => f.write_str("background identity belongs to another stream"),
            Self::HashExhausted => f.write_str("background alias has a full SHA-256 collision"),
        }
    }
}

impl std::error::Error for AssignmentError {}

/// A derived lookup for one stream. There is deliberately no removal operation:
/// completion must not make an exposed alias available to unrelated work.
#[derive(Clone, Debug)]
pub struct Registry {
    stream: String,
    by_alias: HashMap<String, Identity>,
    by_identity: HashMap<Identity, String>,
}

impl Registry {
    pub fn new(stream: impl Into<String>) -> Self {
        Self {
            stream: stream.into(),
            by_alias: HashMap::new(),
            by_identity: HashMap::new(),
        }
    }

    /// Call in first identity-bearing event order, never in display sort order.
    pub fn assign(&mut self, identity: Identity) -> Result<String, AssignmentError> {
        let digest: [u8; 32] = Sha256::digest(canonical(&identity)).into();
        self.assign_digest(identity, digest)
    }

    pub fn alias(&self, identity: &Identity) -> Option<&str> {
        self.by_identity.get(identity).map(String::as_str)
    }

    /// Exact, case-sensitive lookup. This does not assert that work is live or
    /// authorized; the existing operation must check the resolved incarnation.
    pub fn resolve(&self, alias: &str) -> Option<&Identity> {
        self.by_alias.get(alias)
    }

    fn assign_digest(
        &mut self,
        identity: Identity,
        digest: [u8; 32],
    ) -> Result<String, AssignmentError> {
        if identity.stream != self.stream {
            return Err(AssignmentError::WrongStream);
        }
        if let Some(alias) = self.by_identity.get(&identity) {
            return Ok(alias.clone());
        }
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        for length in DEFAULT_LENGTH..=hex.len() {
            let candidate = &hex[..length];
            if !self.by_alias.contains_key(candidate) {
                let alias = candidate.to_owned();
                self.by_alias.insert(alias.clone(), identity.clone());
                self.by_identity.insert(identity, alias.clone());
                return Ok(alias);
            }
        }
        Err(AssignmentError::HashExhausted)
    }
}

fn canonical(identity: &Identity) -> Vec<u8> {
    // A versioned JSON tuple is unambiguous even when IDs contain separators.
    // These concrete string/integer/enum values cannot fail JSON serialization.
    serde_json::to_vec(&(
        "lattice.background-alias.v1",
        &identity.stream,
        identity.kind,
        &identity.original_id,
        &identity.created_by,
    ))
    .expect("background identity consists only of JSON strings and integers")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn identity(kind: Kind, id: u64, creation: &str) -> Identity {
        Identity {
            stream: "stream-a".into(),
            kind,
            original_id: OriginalId::Number(id),
            created_by: creation.into(),
        }
    }

    #[test]
    fn canonical_encoding_is_versioned_typed_and_unambiguous() {
        let mut value = identity(Kind::Timer, 7, "event-7");
        assert_eq!(
            canonical(&value),
            br#"["lattice.background-alias.v1","stream-a","timer",7,"event-7"]"#
        );
        // Independently calculated with Python hashlib.sha256 over the tuple.
        assert_eq!(
            Registry::new("stream-a").assign(value.clone()).unwrap(),
            "36d61dd3"
        );
        let numeric = canonical(&value);
        value.original_id = OriginalId::Text("7".into());
        assert_ne!(canonical(&value), numeric);
        value.stream = "a\",\"b".into();
        let encoded: serde_json::Value = serde_json::from_slice(&canonical(&value)).unwrap();
        assert_eq!(encoded[1], "a\",\"b");
    }

    #[test]
    fn all_kinds_share_the_format_but_not_the_identity() {
        let mut registry = Registry::new("stream-a");
        let mut aliases = HashSet::new();
        for kind in [Kind::Expert, Kind::Run, Kind::Timer, Kind::Watch] {
            let task = identity(kind, 1, "creation");
            let alias = registry.assign(task.clone()).unwrap();
            assert_eq!(alias.len(), 8);
            assert!(alias
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
            assert_eq!(registry.resolve(&alias), Some(&task));
            assert!(aliases.insert(alias));
        }
    }

    #[test]
    fn reused_legacy_id_is_a_different_invocation() {
        let mut registry = Registry::new("stream-a");
        let first = identity(Kind::Timer, 1, "creation-1");
        let next = identity(Kind::Timer, 1, "creation-2");
        let old_alias = registry.assign(first.clone()).unwrap();
        let new_alias = registry.assign(next.clone()).unwrap();
        assert_ne!(old_alias, new_alias);
        assert_eq!(old_alias.len(), 8);
        assert_eq!(new_alias.len(), 8);
        assert_eq!(
            Registry::new("stream-a").assign(next.clone()).unwrap(),
            new_alias
        );
        assert_eq!(registry.resolve(&old_alias), Some(&first));
        assert_eq!(registry.resolve(&new_alias), Some(&next));
        assert_eq!(registry.assign(first).unwrap(), old_alias);
    }

    #[test]
    fn lookup_is_exact_even_for_a_numeric_alias() {
        let mut registry = Registry::new("stream-a");
        let task = identity(Kind::Expert, 1, "creation");
        let alias = registry.assign_digest(task.clone(), [0x12; 32]).unwrap();
        assert_eq!(alias, "12121212");
        assert_eq!(registry.resolve(&alias), Some(&task));
        for unknown in ["1212121", "121212121", " 12121212", "1", "expert:1"] {
            assert_eq!(registry.resolve(unknown), None);
        }
        let other = identity(Kind::Watch, 1, "watch");
        let letters = registry.assign_digest(other, [0xab; 32]).unwrap();
        assert_eq!(registry.resolve(&letters.to_uppercase()), None);
    }

    #[test]
    fn collisions_extend_only_new_aliases_and_replay_identically() {
        let tasks = [
            identity(Kind::Expert, 1, "first"),
            identity(Kind::Run, 1, "second"),
            identity(Kind::Watch, 1, "third"),
        ];
        let replay = || {
            let mut registry = Registry::new("stream-a");
            let aliases: Vec<_> = tasks
                .iter()
                .map(|task| registry.assign_digest(task.clone(), [0xab; 32]).unwrap())
                .collect();
            (registry, aliases)
        };
        let (registry, aliases) = replay();
        assert_eq!(aliases, ["abababab", "ababababa", "ababababab"]);
        for (task, alias) in tasks.iter().zip(&aliases) {
            assert_eq!(registry.alias(task), Some(alias.as_str()));
            assert_eq!(registry.resolve(alias), Some(task));
        }
        let (recovered, replayed) = replay();
        assert_eq!(replayed, aliases);
        for alias in aliases.iter().rev() {
            assert_eq!(recovered.resolve(alias), registry.resolve(alias));
        }
    }

    #[test]
    fn stream_is_part_of_hash_and_lookup_scope() {
        let first = identity(Kind::Timer, 1, "creation");
        let mut other = first.clone();
        other.stream = "stream-b".into();
        let mut a = Registry::new("stream-a");
        let mut b = Registry::new("stream-b");
        let alias_a = a.assign(first).unwrap();
        let alias_b = b.assign(other.clone()).unwrap();
        assert_ne!(alias_a, alias_b);
        assert!(a.resolve(&alias_b).is_none());
        assert!(b.resolve(&alias_a).is_none());
        assert_eq!(a.assign(other), Err(AssignmentError::WrongStream));
    }

    #[test]
    fn exhaustion_does_not_overwrite_an_existing_assignment() {
        let mut registry = Registry::new("stream-a");
        for id in 0..=56 {
            let alias = registry
                .assign_digest(identity(Kind::Timer, id, "creation"), [0; 32])
                .unwrap();
            assert_eq!(alias.len(), 8 + id as usize);
        }
        assert_eq!(
            registry.assign_digest(identity(Kind::Timer, 57, "creation"), [0; 32]),
            Err(AssignmentError::HashExhausted)
        );
        assert_eq!(
            registry.resolve("00000000"),
            Some(&identity(Kind::Timer, 0, "creation"))
        );
    }
}
