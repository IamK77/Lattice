//! A disposable, incrementally reconstructed view of alias assignments.

use super::{AssignmentError, Identity, Kind, OriginalId, Registry};
use crate::{core_events as ce, EventEnvelope, LogReader};
use serde::{Deserialize, Serialize};
use std::io;

const CONSUMER: &str = "background-aliases";
const VERSION: u32 = 1;
const EXPERT_REQUESTED: &str = "subagent.stream.requested";
type Assign = fn(&mut Registry, Identity) -> Result<String, AssignmentError>;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    // Order is part of the collision rule. Completed work stays in this list.
    identities: Vec<Identity>,
}

/// Keep this view while processing a stream; advancing it reads only new headers
/// and relevant bodies. It contains no process handles or execution authority.
pub struct Projection {
    source: crate::kernel::log::ReaderIdentity,
    through: u64,
    registry: Registry,
    state: State,
    assign: Assign,
    pub cold_reason: Option<String>,
    #[cfg(test)]
    headers_read: usize,
    #[cfg(test)]
    bodies_read: usize,
}

impl Projection {
    pub fn recover(reader: &LogReader, through: u64) -> io::Result<Self> {
        Self::recover_with(reader, through, Registry::assign)
    }

    fn recover_with(reader: &LogReader, through: u64, assign: Assign) -> io::Result<Self> {
        if through > reader.snapshot_end() {
            return Err(io::Error::other(
                "alias boundary exceeds the committed ledger",
            ));
        }
        let checkpoint = reader.load_checkpoint::<State>(CONSUMER, VERSION, through)?;
        let mut state = checkpoint.state.unwrap_or_default();
        let mut previous = checkpoint.through;
        let mut cold_reason = checkpoint.cold_reason;
        let mut registry = Registry::new(reader.stream());
        for identity in &state.identities {
            if registry.alias(identity).is_some()
                || assign(&mut registry, identity.clone()).is_err()
            {
                // Bad derived state is disposable, unlike a source read error.
                cold_reason = Some("invalid background alias checkpoint; rebuilding".into());
                previous = 0;
                registry = Registry::new(reader.stream());
                state = State::default();
                break;
            }
        }
        let mut projection = Self {
            source: reader.identity(),
            through: previous,
            registry,
            state,
            assign,
            cold_reason,
            #[cfg(test)]
            headers_read: 0,
            #[cfg(test)]
            bodies_read: 0,
        };
        projection.advance(reader, through)?;
        Ok(projection)
    }

    pub fn through(&self) -> u64 {
        self.through
    }

    pub fn identities(&self) -> &[Identity] {
        &self.state.identities
    }

    pub fn alias(&self, identity: &Identity) -> Option<&str> {
        self.registry.alias(identity)
    }

    /// Resolution identifies a historical invocation, not its liveness or rights.
    pub fn resolve(&self, alias: &str) -> Option<&Identity> {
        self.registry.resolve(alias)
    }

    /// Newest known incarnation of this typed provider ID in this prefix.
    /// This says nothing about liveness: finished invocations still supersede
    /// older ones, and late evidence never changes creation order.
    pub(crate) fn is_current(&self, reader: &LogReader, identity: &Identity) -> io::Result<bool> {
        self.check_source(reader)?;
        let sequence = |id: &str| -> io::Result<u64> {
            reader.header(id)?.map(|header| header.seq).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("background creation event is absent: {id}"),
                )
            })
        };
        let created = sequence(&identity.created_by)?;
        for other in self.identities() {
            if other.kind == identity.kind
                && other.original_id == identity.original_id
                && sequence(&other.created_by)? > created
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn check_source(&self, reader: &LogReader) -> io::Result<()> {
        if !self.source.matches(reader) || self.registry.stream != reader.stream() {
            return Err(io::Error::other(
                "alias projection belongs to another ledger",
            ));
        }
        Ok(())
    }

    pub fn advance(&mut self, reader: &LogReader, through: u64) -> io::Result<()> {
        self.check_source(reader)?;
        if through < self.through || through > reader.snapshot_end() {
            return Err(io::Error::other(
                "invalid background alias projection boundary",
            ));
        }
        reader.try_visit_header_range(self.through + 1, through, |batch| {
            for header in batch {
                #[cfg(test)]
                {
                    self.headers_read += 1;
                }
                if eligible_event(&header.event_type)
                    && !creators(reader, &header.causes)?.is_empty()
                {
                    let event = reader.get(&header.id)?.ok_or_else(|| {
                        io::Error::other("background alias source event is absent")
                    })?;
                    #[cfg(test)]
                    {
                        self.bodies_read += 1;
                    }
                    if let Some(identity) = identity_for_event(reader, &event)? {
                        if self.registry.alias(&identity).is_none() {
                            (self.assign)(&mut self.registry, identity.clone())
                                .map_err(io::Error::other)?;
                            self.state.identities.push(identity);
                        }
                    }
                }
                // On a read error the prefix remains at the last fully applied
                // event. A retry is safe and cannot publish an overstated cache.
                self.through = header.seq;
            }
            Ok(())
        })
    }

    pub fn save(&self, reader: &LogReader) -> io::Result<bool> {
        self.check_source(reader)?;
        reader.save_checkpoint(CONSUMER, VERSION, self.through, &self.state)
    }

    /// Retain usable derived identities when only cache publication fails.
    /// Ownership, source reads and serialization errors still propagate.
    pub(crate) fn save_best_effort(&self, reader: &LogReader) -> io::Result<bool> {
        self.check_source(reader)?;
        reader.save_checkpoint_best_effort(CONSUMER, VERSION, self.through, &self.state)
    }
}

fn eligible_event(event_type: &str) -> bool {
    matches!(
        event_type,
        ce::TOOL_EXEC_COMPLETED | ce::WAKE | EXPERT_REQUESTED
    )
}

fn creators(reader: &LogReader, causes: &[String]) -> io::Result<Vec<(Kind, String)>> {
    let mut found = Vec::new();
    for cause in causes {
        let Some(header) = reader.header(cause)? else {
            return Err(io::Error::other("background alias cause is absent"));
        };
        if header.event_type != ce::TOOL_EXEC_STARTED {
            continue;
        }
        let kind = match header.tool.as_deref() {
            Some("ask") => Kind::Expert,
            Some("Run") => Kind::Run,
            Some("Schedule") => Kind::Timer,
            Some("Watch") => Kind::Watch,
            _ => continue,
        };
        let entry = (kind, header.id);
        if !found.contains(&entry) {
            found.push(entry);
        }
    }
    Ok(found)
}

/// Recognize the actual creating request, not a source-name suffix or a model
/// call label. Cancellation receipts and unrelated result-shaped JSON are not
/// creations. Internal standing watches without a Watch request stay internal.
pub fn identity_for_event(
    reader: &LogReader,
    event: &EventEnvelope,
) -> io::Result<Option<Identity>> {
    if !eligible_event(&event.event_type) {
        return Ok(None);
    }
    if event.stream != reader.stream() {
        return Err(io::Error::other(
            "background event belongs to another stream",
        ));
    }
    let body = match event.event_type.as_str() {
        ce::TOOL_EXEC_COMPLETED if event.payload["status"] == "ok" => &event.payload["result"],
        ce::WAKE => &event.payload["body"],
        EXPERT_REQUESTED => &event.payload,
        _ => return Ok(None),
    };
    let mut identity = None;
    for (kind, created_by) in creators(reader, &event.causes)? {
        if event.event_type == EXPERT_REQUESTED && kind != Kind::Expert {
            continue;
        }
        if event.event_type == ce::TOOL_EXEC_COMPLETED
            && kind == Kind::Run
            && body["background"] != true
        {
            continue;
        }
        let original_id = match kind {
            Kind::Timer => body["timer"].as_u64().map(OriginalId::Number),
            Kind::Watch => body["watch"].as_u64().map(OriginalId::Number),
            Kind::Expert => body["job"].as_u64().map(OriginalId::Number),
            Kind::Run => body["job"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(|id| OriginalId::Text(id.into()))
                .or_else(|| body["job"].as_u64().map(OriginalId::Number)),
        };
        let Some(original_id) = original_id else {
            continue;
        };
        let next = Identity {
            stream: reader.stream().into(),
            kind,
            original_id,
            created_by,
        };
        if identity.as_ref().is_some_and(|old| old != &next) {
            return Err(io::Error::other(format!(
                "background event {} has ambiguous creating requests",
                event.id
            )));
        }
        identity = Some(next);
    }
    Ok(identity)
}

#[cfg(test)]
mod tests;
