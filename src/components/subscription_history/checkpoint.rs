//! Committed-prefix subscription recovery. Each source owns its checkpoint;
//! only live subscriptions and facts about future IDs remain in it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;

use serde::{Deserialize, Serialize};

use crate::components::subscription_history::{Restored, Subscription};
use crate::{core_events as ce, EventEnvelope, LogReader};

const VERSION: u32 = 2;

#[derive(Clone, Copy)]
pub(in crate::components) enum Kind {
    Timer,
    Watch,
}

impl Kind {
    fn consumer(self) -> &'static str {
        match self {
            Self::Timer => "timer-subscriptions",
            Self::Watch => "watch-subscriptions",
        }
    }

    fn tools(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Timer => ("Schedule", "Unschedule", "timer"),
            Self::Watch => ("Watch", "Unwatch", "watch"),
        }
    }

    fn eligible(self, args: &serde_json::Value) -> bool {
        match self {
            Self::Timer => args["interval_ms"].as_u64().is_some(),
            Self::Watch => true,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Live {
    id: u64,
    fired: u64,
    limit: u64,
    cause: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    next_id: u64,
    live: Vec<Live>,
    // Unschedule accepts not-yet-assigned IDs; a wake may also precede its
    // assignment. These facts remain relevant until that ID has been assigned.
    future_cancelled: BTreeSet<u64>,
    future_fired: BTreeMap<u64, u64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            next_id: 1,
            live: Vec::new(),
            future_cancelled: BTreeSet::new(),
            future_fired: BTreeMap::new(),
        }
    }
}

#[derive(Default)]
struct Reads {
    #[cfg(test)]
    headers: usize,
    #[cfg(test)]
    bodies: usize,
}

impl Reads {
    fn get(&mut self, reader: &LogReader, id: &str) -> io::Result<EventEnvelope> {
        #[cfg(test)]
        {
            self.bodies += 1;
        }
        reader
            .get(id)?
            .ok_or_else(|| io::Error::other("subscription recovery source is absent"))
    }
}

pub(in crate::components) struct Recovery {
    pub state: Restored,
    pub cold_reason: Option<String>,
    #[cfg(test)]
    headers: usize,
    #[cfg(test)]
    bodies: usize,
}

fn fold(
    reader: &LogReader,
    previous: u64,
    through: u64,
    state: &mut State,
    reads: &mut Reads,
    kind: Kind,
) -> io::Result<bool> {
    let (start_tool, stop_tool, id_field) = kind.tools();
    let initial_next = state.next_id;
    let mut starts = HashMap::new();
    let mut assigned = HashMap::new();
    // Alias requests retain their target incarnation. This changes neither
    // legacy numeric cancellation nor request-based cancellation timing.
    // A later assignment of an earlier request already forces a cold rebuild,
    // so unassigned scoped cancellations need no additional tombstone cache.
    let mut scoped_cancelled = BTreeSet::new();
    let mut late = false;
    reader.try_visit_header_range(previous + 1, through, |batch| {
        for header in batch {
            #[cfg(test)]
            {
                reads.headers += 1;
            }
            match header.event_type.as_str() {
                ce::TOOL_EXEC_STARTED if header.tool.as_deref() == Some(start_tool) => {
                    starts.insert(header.id.clone(), header.seq);
                }
                ce::TOOL_EXEC_STARTED if header.tool.as_deref() == Some(stop_tool) => {
                    let event = reads.get(reader, &header.id)?;
                    if let Some(id) = event.payload["arguments"][id_field].as_u64() {
                        use crate::background_aliases::{operations, Kind as BackgroundKind};
                        match operations::reference(&event.payload) {
                            Ok(Some(reference)) => {
                                let target_kind = match kind {
                                    Kind::Timer => BackgroundKind::Timer,
                                    Kind::Watch => BackgroundKind::Watch,
                                };
                                if operations::validate(
                                    &event.payload,
                                    reader.stream(),
                                    target_kind,
                                    Some(id),
                                    Some(&reference.identity.created_by),
                                )
                                .is_ok()
                                {
                                    scoped_cancelled.insert((id, reference.identity.created_by));
                                }
                            }
                            Ok(None) => {
                                state.future_cancelled.insert(id);
                            }
                            Err(_) => {} // Malformed alias metadata cannot select a numeric target.
                        }
                    }
                }
                ce::WAKE => {
                    let event = reads.get(reader, &header.id)?;
                    if let (Some(id), Some(n)) = (
                        event.payload["body"][id_field].as_u64(),
                        event.payload["body"]["fire"].as_u64(),
                    ) {
                        let high = state.future_fired.entry(id).or_default();
                        *high = (*high).max(n);
                    }
                }
                ce::TOOL_EXEC_COMPLETED => {
                    // Classify by indexed request metadata before loading a
                    // result, so unrelated tool output is not decoded.
                    let mut relevant = Vec::new();
                    for cause in &header.causes {
                        if let Some(&seq) = starts.get(cause) {
                            relevant.push((cause.clone(), seq));
                        } else if previous > 0 {
                            if let Some(source) = reader.header(cause)? {
                                if source.event_type == ce::TOOL_EXEC_STARTED
                                    && source.tool.as_deref() == Some(start_tool)
                                {
                                    relevant.push((cause.clone(), source.seq));
                                }
                            }
                        }
                    }
                    if relevant.is_empty() {
                        continue;
                    }
                    let event = reads.get(reader, &header.id)?;
                    if let Some(id) = event.payload["result"][id_field].as_u64() {
                        for (cause, seq) in relevant {
                            // A late/duplicate assignment may have been answered
                            // in the old prefix, or reuse an already closed ID.
                            // Explicitly rebuild instead of keeping all tombstones.
                            if previous > 0 && (seq <= previous || id < initial_next) {
                                late = true;
                            }
                            assigned.entry(cause).or_insert((seq, id));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })?;
    if late {
        return Ok(true);
    }
    let mut assigned: Vec<_> = assigned.into_iter().collect();
    assigned.sort_by_key(|(_, (seq, _))| *seq);
    for (cause, (_, id)) in assigned {
        let next = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("subscription identifier space exhausted"))?;
        state.next_id = state.next_id.max(next);
        let event = reads.get(reader, &cause)?;
        let args = &event.payload["arguments"];
        if kind.eligible(args) {
            state.live.push(Live {
                id,
                fired: 0,
                limit: args["max_fires"].as_u64().unwrap_or(100).max(1),
                cause,
            });
        }
    }
    for timer in &mut state.live {
        timer.fired = timer
            .fired
            .max(state.future_fired.get(&timer.id).copied().unwrap_or(0));
    }
    state.live.retain(|timer| {
        !state.future_cancelled.contains(&timer.id)
            && !scoped_cancelled.contains(&(timer.id, timer.cause.clone()))
            && timer.fired < timer.limit
    });
    state.future_cancelled.retain(|id| *id >= state.next_id);
    state.future_fired.retain(|id, _| *id >= state.next_id);
    Ok(false)
}

pub(in crate::components) fn recover(reader: &LogReader, kind: Kind) -> io::Result<Recovery> {
    let through = reader.snapshot_end();
    let consumer = kind.consumer();
    let checkpoint = reader.load_checkpoint::<State>(consumer, VERSION, through)?;
    let mut state = checkpoint.state.unwrap_or_default();
    let mut cold_reason = checkpoint.cold_reason;
    let mut reads = Reads::default();
    if fold(
        reader,
        checkpoint.through,
        through,
        &mut state,
        &mut reads,
        kind,
    )? {
        cold_reason =
            Some("subscription assignment refers to an older request or identifier".into());
        state = State::default();
        fold(reader, 0, through, &mut state, &mut reads, kind)?;
    }
    let mut live = Vec::new();
    // Read all required original requests before saving or rearming anything.
    for timer in &state.live {
        let event = reads.get(reader, &timer.cause)?;
        live.push(Subscription {
            id: timer.id,
            fired: timer.fired,
            arguments: event.payload["arguments"].clone(),
            cause: timer.cause.clone(),
        });
    }
    if let Err(error) = reader.save_checkpoint(consumer, VERSION, through, &state) {
        eprintln!("cannot save {consumer} recovery state: {error}");
    }
    Ok(Recovery {
        state: Restored {
            next_id: state.next_id,
            live,
        },
        cold_reason,
        #[cfg(test)]
        headers: reads.headers,
        #[cfg(test)]
        bodies: reads.bodies,
    })
}

#[cfg(test)]
mod tests;
