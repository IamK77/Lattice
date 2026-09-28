//! A small identity/causality index and separately loaded event bodies.
//! This module never invents a partially populated EventEnvelope.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};

use super::event_store::EventStore;
use crate::core_events::{self, EventRelations};
use crate::EventEnvelope;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Header {
    pub v: u32,
    pub id: String,
    pub seq: u64,
    pub event_type: String,
    pub source: String,
    pub tool: Option<String>,
    pub causes: Vec<String>,
    /// Presence, including null, matches payload.get("purpose").is_some().
    /// This projection avoids decoding request bodies just to classify a call.
    pub has_purpose: bool,
    pub has_system: bool,
    pub purpose: Option<String>,
    pub call: Option<String>,
    /// Missing IDs must survive: they represent unpaired invocations, not no calls.
    pub tool_calls: Vec<Option<String>>,
}

impl Header {
    pub fn from_event(event: &EventEnvelope) -> Self {
        Self {
            v: event.v,
            id: event.id.clone(),
            seq: event.seq,
            event_type: event.event_type.clone(),
            source: event.source.clone(),
            causes: event.causes.clone(),
            tool: (event.event_type == core_events::TOOL_EXEC_STARTED)
                .then(|| event.payload["tool"].as_str().map(str::to_owned))
                .flatten(),
            has_purpose: event.payload.get("purpose").is_some(),
            has_system: !event.payload["system"].is_null(),
            purpose: event
                .payload
                .get("purpose")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            call: matches!(
                event.event_type.as_str(),
                core_events::TOOL_EXEC_STARTED | core_events::TOOL_EXEC_COMPLETED
            )
            .then(|| event.payload["call"].as_str().map(str::to_owned))
            .flatten(),
            tool_calls: if event.event_type == core_events::MODEL_CALL_COMPLETED {
                event.payload["toolCalls"]
                    .as_array()
                    .map(|calls| {
                        calls
                            .iter()
                            .map(|call| call["id"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
        }
    }

    pub fn retained_size(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.id.capacity()
            + self.event_type.capacity()
            + self.source.capacity()
            + self.tool.as_ref().map_or(0, String::capacity)
            + self.purpose.as_ref().map_or(0, String::capacity)
            + self.call.as_ref().map_or(0, String::capacity)
            + self.causes.capacity() * std::mem::size_of::<String>()
            + self.causes.iter().map(String::capacity).sum::<usize>()
            + self.tool_calls.capacity() * std::mem::size_of::<Option<String>>()
            + self
                .tool_calls
                .iter()
                .flatten()
                .map(String::capacity)
                .sum::<usize>()
    }

    pub fn relations(&self) -> EventRelations<'_> {
        EventRelations {
            id: &self.id,
            event_type: &self.event_type,
            causes: &self.causes,
        }
    }
}

#[derive(Debug)]
enum Bodies {
    /// With no backing file the body is the sole copy and cannot be evicted.
    Memory(Vec<Arc<EventEnvelope>>),
    Disk(EventStore),
}

/// Owns at most one index window; it never holds the history lock between
/// callbacks. The range is a fixed snapshot, not a live tail subscription.
pub(crate) struct HeaderCursor {
    positions: std::ops::Range<usize>,
    reverse: bool,
    window: Option<super::segmented::IndexWindow>,
}

impl HeaderCursor {
    pub fn new(from: usize, through: usize, reverse: bool) -> Self {
        Self {
            positions: from..through,
            reverse,
            window: None,
        }
    }

    pub fn next(&mut self, history: &History) -> io::Result<Option<(Header, u64)>> {
        let Some(position) = (if self.reverse {
            self.positions.next_back()
        } else {
            self.positions.next()
        }) else {
            return Ok(None);
        };
        if let Some(ledger) = &history.segmented {
            let seq = position as u64 + 1;
            if let Some(entry) = self.window.as_ref().and_then(|window| window.metadata(seq)) {
                return Ok(Some(entry));
            }
            let source = ledger
                .lock()
                .map_err(|_| io::Error::other("segmented ledger lock poisoned"))?;
            self.window = Some(source.index_window(seq)?);
            return self
                .window
                .as_ref()
                .and_then(|window| window.metadata(seq))
                .map(Some)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "index window omitted committed position",
                    )
                });
        }
        history.entry_at(position).map(Some)
    }

    pub fn load(&self, history: &History, seq: u64) -> io::Result<Arc<EventEnvelope>> {
        let position = usize::try_from(
            seq.checked_sub(1)
                .ok_or_else(|| io::Error::other("invalid ledger sequence"))?,
        )
        .map_err(|_| io::Error::other("ledger sequence exceeds addressable positions"))?;
        history
            .load_with_window(position, self.window.as_ref())?
            .ok_or_else(|| io::Error::other("indexed ledger event missing"))
    }
}

#[derive(Debug)]
pub(crate) struct History {
    legacy_headers: Vec<Header>,
    legacy_index: HashMap<String, usize>,
    legacy_sizes: Vec<u64>,
    segmented: Option<Arc<Mutex<super::segmented::Ledger>>>,
    count: usize,
    total_bytes: u64,
    latest_id: Option<String>,
    bodies: Mutex<Bodies>,
}

impl History {
    pub fn new(file: Option<File>, budget: usize) -> Self {
        Self {
            legacy_headers: Vec::new(),
            legacy_index: HashMap::new(),
            legacy_sizes: Vec::new(),
            segmented: None,
            count: 0,
            total_bytes: 0,
            latest_id: None,
            bodies: Mutex::new(match file {
                Some(file) => Bodies::Disk(EventStore::new(file, budget)),
                None => Bodies::Memory(Vec::new()),
            }),
        }
    }

    pub fn segmented(
        ledger: Arc<Mutex<super::segmented::Ledger>>,
        budget: usize,
    ) -> io::Result<Self> {
        let mut history = Self::new(None, budget);
        {
            let source = ledger
                .lock()
                .map_err(|_| io::Error::other("segmented ledger lock poisoned"))?;
            history.count = usize::try_from(source.count())
                .map_err(|_| io::Error::other("history exceeds addressable positions"))?;
            history.total_bytes = source.byte_len();
            history.latest_id = source.metadata_at(source.count())?.map(|entry| entry.0.id);
        }
        history.bodies = Mutex::new(Bodies::Disk(EventStore::segmented(
            Arc::clone(&ledger),
            budget,
        )));
        history.segmented = Some(ledger);
        Ok(history)
    }

    pub fn checkpoint_digest(&self, through: u64) -> io::Result<Option<[u8; 32]>> {
        self.segmented
            .as_ref()
            .map(|ledger| {
                ledger
                    .lock()
                    .map_err(|_| io::Error::other("segmented ledger lock poisoned"))?
                    .prefix_digest(through)
            })
            .transpose()
    }

    pub fn len(&self) -> usize {
        self.count
    }
    pub fn latest_id(&self) -> Option<String> {
        self.latest_id.clone()
    }
    pub fn byte_len(&self) -> u64 {
        self.total_bytes
    }

    pub fn entry_at(&self, position: usize) -> io::Result<(Header, u64)> {
        if position >= self.count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "history position out of range",
            ));
        }
        if let Some(ledger) = &self.segmented {
            let source = ledger
                .lock()
                .map_err(|_| io::Error::other("segmented ledger lock poisoned"))?;
            return source.metadata_at(position as u64 + 1)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "indexed history position missing",
                )
            });
        }
        Ok((
            self.legacy_headers[position].clone(),
            self.legacy_sizes[position],
        ))
    }

    pub fn header_at(&self, position: usize) -> io::Result<Header> {
        self.entry_at(position).map(|entry| entry.0)
    }

    pub fn position(&self, id: &str) -> io::Result<Option<usize>> {
        if let Some(ledger) = &self.segmented {
            let source = ledger
                .lock()
                .map_err(|_| io::Error::other("segmented ledger lock poisoned"))?;
            return source
                .sequence_of(id)?
                .map(|seq| {
                    usize::try_from(seq - 1)
                        .map_err(|_| io::Error::other("history exceeds addressable positions"))
                })
                .transpose();
        }
        Ok(self.legacy_index.get(id).copied())
    }

    pub fn header(&self, id: &str) -> io::Result<Option<Header>> {
        self.position(id)?
            .map(|position| self.header_at(position))
            .transpose()
    }

    /// Called only after validation and durable append (or validated recovery).
    /// Disk-backed histories keep the header and byte location, not the body.
    pub fn push(&mut self, event: &EventEnvelope, offset: u64, line: &[u8]) -> io::Result<()> {
        let position = self.count;
        let bodies = self
            .bodies
            .get_mut()
            .map_err(|_| io::Error::other("history body lock poisoned"))?;
        match bodies {
            Bodies::Memory(events) => events.push(Arc::new(event.clone())),
            Bodies::Disk(store) => {
                store.register_event(event, offset, line);
            }
        }
        if self.segmented.is_none() {
            self.legacy_index.insert(event.id.clone(), position);
            self.legacy_sizes.push(line.len() as u64 + 1);
            self.legacy_headers.push(Header::from_event(event));
        }
        self.count += 1;
        self.total_bytes += line.len() as u64 + 1;
        self.latest_id = Some(event.id.clone());
        Ok(())
    }

    pub fn memory_stats(&self) -> io::Result<crate::memory::HistoryStats> {
        let bodies = self.bodies.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "history body cache busy")
            }
            std::sync::TryLockError::Poisoned(_) => io::Error::other("history body lock poisoned"),
        })?;
        let (in_memory_bodies, cache) = match &*bodies {
            Bodies::Memory(events) => (events.len(), None),
            Bodies::Disk(store) => (0, Some(store.memory_stats())),
        };
        Ok(crate::memory::HistoryStats {
            events: self.count,
            in_memory_bodies,
            cache,
        })
    }

    pub fn load(&self, position: usize) -> io::Result<Option<Arc<EventEnvelope>>> {
        self.load_with_window(position, None)
    }

    fn load_with_window(
        &self,
        position: usize,
        window: Option<&super::segmented::IndexWindow>,
    ) -> io::Result<Option<Arc<EventEnvelope>>> {
        let mut bodies = self
            .bodies
            .lock()
            .map_err(|_| io::Error::other("history body lock poisoned"))?;
        let result = match &mut *bodies {
            Bodies::Memory(events) => Ok(events.get(position).cloned()),
            Bodies::Disk(store) => match window {
                Some(window) => store.get_with_window(position, Some(window)),
                None => store.get(position),
            },
        };
        result.map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("reading ledger sequence {}: {error}", position + 1),
            )
        })
    }

    pub fn get(&self, id: &str) -> io::Result<Option<Arc<EventEnvelope>>> {
        match self.position(id)? {
            Some(position) => self.load(position),
            None => Ok(None),
        }
    }

    pub fn has_outcome(&self, started_id: &str) -> io::Result<bool> {
        let Some(start) = self.position(started_id)? else {
            return Ok(false);
        };
        // A validated cause precedes its outcome. Old unrelated volumes need
        // not be read to decide whether a newly issued call has ended.
        let mut cursor = HeaderCursor::new(start + 1, self.count, true);
        while let Some((header, _)) = cursor.next(self)? {
            if core_events::relation_ends_call(header.relations(), started_id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[cfg(test)]
    pub fn hanging(&self, started_type: &str) -> io::Result<Vec<String>> {
        let mut pending = core_events::PendingCalls::new(started_type);
        let mut cursor = HeaderCursor::new(0, self.count, false);
        while let Some((header, _)) = cursor.next(self)? {
            pending.observe(header.relations());
        }
        Ok(pending.heads())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core_events as ce, EventDraft, EventLog};
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn memory_observation_does_not_wait_for_an_active_body_reader() {
        let history = Arc::new(History::new(None, 0));
        let held = history.bodies.lock().unwrap();
        let other = Arc::clone(&history);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || tx.send(other.memory_stats()).unwrap());
        let observed = rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(held);
        worker.join().unwrap();
        assert_eq!(
            observed
                .expect("observation waited for the body reader")
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn segmented_history_retains_no_duplicate_identity_or_header_arrays() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bounded.ledger");
        let mut log =
            EventLog::open_segmented(ce::core_event_decls(), "history", path.clone(), 1).unwrap();
        for _ in 0..12 {
            log.append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"hello"})),
                "ui",
            )
            .unwrap();
        }
        drop(log);
        let source = Arc::new(Mutex::new(
            super::super::segmented::Ledger::open_for(&path, 1, Some("history")).unwrap(),
        ));
        let mut history = History::segmented(Arc::clone(&source), 0).unwrap();
        assert_eq!(history.len(), 12);
        let mut event = source.lock().unwrap().get_at(12).unwrap().unwrap();
        event.seq = 13;
        event.id = "ev_13_fixture".into();
        let line = serde_json::to_vec(&event).unwrap();
        {
            let mut ledger = source.lock().unwrap();
            ledger.validate_append(&event).unwrap();
            ledger.append_validated_line(&event, &line).unwrap();
        }
        history.push(&event, 0, &line).unwrap();
        assert_eq!(history.len(), 13);
        assert!(history.legacy_headers.is_empty());
        assert!(history.legacy_index.is_empty());
        assert!(history.legacy_sizes.is_empty());
        assert_eq!(history.header_at(0).unwrap().seq, 1);
        assert_eq!(history.header_at(12).unwrap().id, event.id);
    }

    #[test]
    fn outcome_checks_survive_unreadable_bodies_without_guessing() {
        let mut source = EventLog::in_memory(ce::core_event_decls(), "history");
        let call = source.append(EventDraft::new(ce::MODEL_CALL_STARTED, &[],
            json!({"model":"offline", "input":{"parts":[], "fingerprint":"sha256:test"}, "tools":[]})), "model").unwrap();
        let done = source
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&call.id],
                    json!({"status":"ok", "text":"finished"}),
                ),
                "model",
            )
            .unwrap();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let mut history = History::new(Some(file.reopen().unwrap()), 0);
        let mut offset = 0;
        for event in [&call, &done] {
            let line = serde_json::to_vec(event).unwrap();
            file.write_all(&line).unwrap();
            file.write_all(b"\n").unwrap();
            history.push(event, offset, &line).unwrap();
            offset += line.len() as u64 + 1;
        }
        file.as_file().set_len(0).unwrap();
        let stats = history.memory_stats().unwrap();
        assert_eq!(stats.events, 2);
        assert_eq!(stats.in_memory_bodies, 0);
        let cache = stats.cache.unwrap();
        assert_eq!(cache.entries, 0);
        assert_eq!(cache.decodes, 0);
        assert_eq!(cache.hits, 0);
        assert_eq!(cache.estimated_retained_bytes, 0);
        assert!(history.get(&done.id).is_err());
        assert!(history.get("missing").unwrap().is_none());
        assert!(
            history.hanging(ce::MODEL_CALL_STARTED).unwrap().is_empty(),
            "Unavailable bodies must not turn an answered call into an unanswered call"
        );
        assert_eq!(history.header_at(history.len() - 1).unwrap().seq, done.seq);
    }
}
