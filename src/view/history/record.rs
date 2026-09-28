//! A card is a small recipe over immutable events, not another copy of its text.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{core_events as ce, EventEnvelope};

use super::super::{command_job, ingest, Entry};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::view) enum Kind {
    User,
    Tool,
    Thinking,
    Anchor,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::view) struct Identity {
    pub kind: Kind,
    pub call: Option<String>,
    pub command: bool,
}

impl Identity {
    pub fn of(entry: &Entry) -> Self {
        let (kind, call, command) = match entry {
            Entry::User(_) => (Kind::User, None, false),
            Entry::Tool(card) => (Kind::Tool, card.call.clone(), card.name == "Run"),
            Entry::Thinking(_) => (Kind::Thinking, None, false),
            Entry::Wake(_) | Entry::Attachment(_) => (Kind::Anchor, None, false),
            _ => (Kind::Other, None, false),
        };
        Self {
            kind,
            call,
            command,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::view) struct Record {
    pub source: String,
    pub ordinal: usize,
    pub identity: Identity,
    pub completion: Option<String>,
    pub wake: Option<String>,
}

impl crate::view::pages::Item for Record {
    fn lookup_key(&self) -> Option<&str> {
        self.identity
            .command
            .then_some(self.identity.call.as_deref())
            .flatten()
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Record {
    pub fn new(source: &str, ordinal: usize, entry: &Entry) -> Self {
        Self {
            source: source.into(),
            ordinal,
            identity: Identity::of(entry),
            completion: None,
            wake: None,
        }
    }

    /// Lookup failures stay failures. In particular, an absent source must not
    /// become a missing card that silently shifts every subsequent ordinal.
    pub fn materialize(
        &self,
        mut get: impl FnMut(&str) -> io::Result<EventEnvelope>,
    ) -> io::Result<Entry> {
        let source = get(&self.source)?;
        if source.id != self.source {
            return Err(invalid("card source identity mismatch"));
        }
        let mut entries = Vec::new();
        ingest(&mut entries, &source);
        let entry = entries
            .into_iter()
            .nth(self.ordinal)
            .ok_or_else(|| invalid("card source ordinal is absent"))?;
        if Identity::of(&entry) != self.identity {
            return Err(invalid("card source metadata mismatch"));
        }
        let mut entries = vec![entry];
        let mut previous_seq = source.seq;
        drop(source);
        for (id, event_type) in [
            (&self.completion, ce::TOOL_EXEC_COMPLETED),
            (&self.wake, ce::WAKE),
        ] {
            let Some(id) = id else { continue };
            let event = get(id)?;
            if event.id != *id || event.event_type != event_type || event.seq <= previous_seq {
                return Err(invalid("card update identity, type or order mismatch"));
            }
            if event_type == ce::WAKE && command_job(&event.payload).is_none() {
                return Err(invalid("card wake is not a command completion"));
            }
            if !ingest(&mut entries, &event)
                || entries.len() != 1
                || Identity::of(&entries[0]) != self.identity
            {
                return Err(invalid("card update does not address this card"));
            }
            previous_seq = event.seq;
        }
        entries
            .pop()
            .ok_or_else(|| invalid("card materialization produced no card"))
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
