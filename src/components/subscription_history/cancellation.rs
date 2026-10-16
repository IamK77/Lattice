//! Cancellation is a committed receipt, not a request or a buffered emission.

use super::Kind;
use crate::kernel::log::Header;
use crate::{core_events as ce, EventDraft, EventTypeDecl, LogReader};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io;
use std::sync::Mutex;

impl Kind {
    pub(in crate::components) fn cancellation_activation(self) -> &'static str {
        match self {
            Self::Timer => "timer.cancellation.activated",
            Self::Watch => "watch.cancellation.activated",
        }
    }
}

pub(in crate::components) fn declaration(kind: Kind) -> EventTypeDecl {
    EventTypeDecl::new(
        kind.cancellation_activation(),
        "This provider now applies only committed cancellation receipts",
    )
    .with_schema(json!({
        "type":"object", "required":["v","legacyThrough"],
        "properties":{
            "v":{"const":1},
            "legacyThrough":{"type":"integer","minimum":0}
        },
        "additionalProperties":false
    }))
}

pub(in crate::components) fn activation(kind: Kind, legacy_through: u64) -> EventDraft {
    EventDraft::new(
        kind.cancellation_activation(),
        &[],
        json!({"v":1,"legacyThrough":legacy_through}),
    )
}

/// Constructing this value has no cancellation side effect. The ledger commit
/// is the change of subscription state; a worker observes it at its next check.
pub(in crate::components) fn receipt(kind: Kind, id: u64) -> Value {
    let field = kind.tools().2;
    let note = match kind {
        Kind::Timer => "cancelled",
        Kind::Watch => "stopped",
    };
    json!({"status":"ok","result":{
        (field):id, "note":note, "cancellation":{"v":1}
    }})
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::components) struct Protocol {
    pub legacy_through: Option<u64>,
    owner: Option<String>,
}

pub(super) enum Observation {
    None,
    Activated { first: bool },
    LegacyCancellation(u64),
    RecordedCancellation(u64),
}

impl Protocol {
    /// Preserve only the migration boundary when rebuilding from the beginning;
    /// ownership must then follow the historical activation records again.
    pub(super) fn for_rebuild(&self) -> Self {
        Self {
            legacy_through: self.legacy_through,
            owner: None,
        }
    }

    pub(super) fn observe(
        &mut self,
        reader: &LogReader,
        header: &Header,
        kind: Kind,
    ) -> io::Result<Observation> {
        self.observe_with(reader, header, kind, |id| required(reader, id))
    }

    pub(super) fn observe_with(
        &mut self,
        reader: &LogReader,
        header: &Header,
        kind: Kind,
        mut get: impl FnMut(&str) -> io::Result<crate::EventEnvelope>,
    ) -> io::Result<Observation> {
        let (_, stop_tool, field) = kind.tools();
        if header.event_type == kind.cancellation_activation() {
            let event = get(&header.id)?;
            let cutoff = event.payload["legacyThrough"].as_u64().ok_or_else(|| {
                io::Error::other("cancellation activation has no legacy boundary")
            })?;
            if event.payload["v"] != 1 || cutoff >= header.seq {
                return Err(io::Error::other("invalid cancellation activation boundary"));
            }
            if self
                .legacy_through
                .is_some_and(|previous| previous != cutoff)
            {
                return Err(io::Error::other("cancellation legacy boundary changed"));
            }
            let first = self.legacy_through.is_none();
            self.legacy_through = Some(cutoff);
            self.owner = Some(header.source.clone());
            return Ok(Observation::Activated { first });
        }
        if header.event_type == ce::TOOL_EXEC_STARTED
            && header.tool.as_deref() == Some(stop_tool)
            && self
                .legacy_through
                .is_none_or(|cutoff| header.seq <= cutoff)
        {
            let event = get(&header.id)?;
            return Ok(event.payload["arguments"][field]
                .as_u64()
                .map(Observation::LegacyCancellation)
                .unwrap_or(Observation::None));
        }
        if header.event_type != ce::TOOL_EXEC_COMPLETED
            || self.owner.as_deref() != Some(header.source.as_str())
        {
            return Ok(Observation::None);
        }
        let mut requests = Vec::new();
        for cause in &header.causes {
            let source = reader
                .header(cause)?
                .ok_or_else(|| io::Error::other("cancellation receipt source is absent"))?;
            if source.event_type == ce::TOOL_EXEC_STARTED
                && source.tool.as_deref() == Some(stop_tool)
            {
                requests.push(source.id);
            }
        }
        if requests.is_empty() {
            return Ok(Observation::None);
        }
        let event = get(&header.id)?;
        if event.payload["status"] != "ok" {
            return Ok(Observation::None);
        }
        if event.payload["result"]["cancellation"]["v"] != 1 {
            return Err(io::Error::other(
                "successful cancellation lacks its recorded-state marker",
            ));
        }
        let id = event.payload["result"][field].as_u64().ok_or_else(|| {
            io::Error::other("cancellation receipt has no subscription identifier")
        })?;
        for request in requests {
            if get(&request)?.payload["arguments"][field].as_u64() != Some(id) {
                return Err(io::Error::other(
                    "cancellation receipt identifies a different subscription",
                ));
            }
        }
        Ok(Observation::RecordedCancellation(id))
    }
}

fn required(reader: &LogReader, id: &str) -> io::Result<crate::EventEnvelope> {
    reader
        .get(id)?
        .ok_or_else(|| io::Error::other("cancellation source is absent"))
}

struct LiveState {
    through: u64,
    protocol: Protocol,
    cancelled: BTreeSet<u64>,
}

/// Shared by a provider's workers. No caller can insert a fresh cancellation:
/// only a committed receipt observed from this ledger changes the live view.
pub(in crate::components) struct LiveCancellations {
    reader: LogReader,
    kind: Kind,
    state: Mutex<LiveState>,
}

impl LiveCancellations {
    pub(in crate::components) fn new(
        reader: LogReader,
        kind: Kind,
        through: u64,
        protocol: Protocol,
        cancelled: BTreeSet<u64>,
    ) -> Self {
        Self {
            reader,
            kind,
            state: Mutex::new(LiveState {
                through,
                protocol,
                cancelled,
            }),
        }
    }

    pub(in crate::components) fn stopped(&self, id: u64) -> io::Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("cancellation view lock poisoned"))?;
        let through = self.reader.snapshot_end();
        self.reader
            .try_visit_header_range(state.through + 1, through, |batch| {
                for header in batch {
                    // All requests arriving during this provider lifetime use the
                    // new protocol. A queued activation may appear after a request;
                    // never apply the old request-only rule to live activity.
                    if header.event_type != ce::TOOL_EXEC_STARTED {
                        if let Observation::RecordedCancellation(cancelled) =
                            state.protocol.observe(&self.reader, header, self.kind)?
                        {
                            state.cancelled.insert(cancelled);
                        }
                    }
                    state.through = header.seq;
                }
                Ok(())
            })?;
        Ok(state.cancelled.contains(&id))
    }

    pub(in crate::components) fn stopped_or_report(
        &self,
        id: u64,
        injector: &crate::kernel::host::Injector,
        started: Option<&str>,
    ) -> bool {
        match self.stopped(id) {
            Ok(stopped) => stopped,
            Err(error) => {
                let field = self.kind.tools().2;
                let causes: Vec<_> = started.into_iter().collect();
                injector.emit("wake", EventDraft::new(ce::WAKE, &causes, json!({
                    "source":format!("{field}:{id}"),
                    "summary":format!("{field} {id} stopped: cancellation history is unreadable"),
                    "body":{(field):id, "interrupted":"cancellation_history_unreadable", "error":error.to_string()}
                })));
                true
            }
        }
    }
}

#[cfg(test)]
pub(in crate::components) mod provider_tests;
#[cfg(test)]
mod tests;
