//! Disk-backed event bodies. The index is small; decoded bodies have a byte
//! budget. An outstanding Arc pins only the event its caller is using.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::EventEnvelope;

#[derive(Debug)]
enum Location {
    File {
        offset: u64,
        bytes: usize,
        digest: [u8; 32],
    },
}

#[derive(Debug)]
enum Source {
    File(File),
    Segmented(Arc<Mutex<super::segmented::Ledger>>),
}

#[derive(Debug)]
struct Cached {
    event: Arc<EventEnvelope>,
    bytes: usize,
}

/// Owned by a synchronized ledger reader. File handles pin the opened file,
/// not its pathname; a rename cannot silently switch the ledger being read.
#[derive(Debug)]
pub(crate) struct EventStore {
    source: Source,
    locations: Vec<Location>,
    cache: HashMap<usize, Cached>,
    recent: VecDeque<usize>,
    budget: usize,
    retained: usize,
    decodes: u64,
    hits: u64,
}

impl EventStore {
    pub fn new(file: File, budget: usize) -> Self {
        Self::with_source(Source::File(file), budget)
    }

    pub fn segmented(ledger: Arc<Mutex<super::segmented::Ledger>>, budget: usize) -> Self {
        Self::with_source(Source::Segmented(ledger), budget)
    }

    fn with_source(source: Source, budget: usize) -> Self {
        Self {
            source,
            locations: Vec::new(),
            cache: HashMap::new(),
            recent: VecDeque::new(),
            budget,
            retained: 0,
            decodes: 0,
            hits: 0,
        }
    }

    pub fn memory_stats(&self) -> crate::memory::CacheStats {
        crate::memory::CacheStats {
            entries: self.cache.len(),
            estimated_retained_bytes: self.retained,
            budget_bytes: self.budget,
            decodes: self.decodes,
            hits: self.hits,
        }
    }

    /// Register an already-validated, committed line without retaining its body.
    pub fn register(&mut self, offset: u64, line: &[u8]) -> usize {
        let position = self.locations.len();
        self.locations.push(Location::File {
            offset,
            bytes: line.len(),
            digest: Sha256::digest(line).into(),
        });
        position
    }

    pub fn register_event(&mut self, event: &EventEnvelope, offset: u64, line: &[u8]) -> usize {
        match self.source {
            Source::File(_) => self.register(offset, line),
            Source::Segmented(_) => (event.seq - 1) as usize,
        }
    }

    /// Missing positions and unreadable bodies are different outcomes. Never
    /// treat a failed read as absence: that could make a completed call run again.
    pub fn get(&mut self, position: usize) -> io::Result<Option<Arc<EventEnvelope>>> {
        self.get_with_window(position, None)
    }

    pub fn get_with_window(
        &mut self,
        position: usize,
        window: Option<&super::segmented::IndexWindow>,
    ) -> io::Result<Option<Arc<EventEnvelope>>> {
        if let Some(cached) = self.cache.get(&position) {
            self.hits += 1;
            let event = Arc::clone(&cached.event);
            self.touch(position);
            return Ok(Some(event));
        }
        let event: EventEnvelope = match &self.source {
            Source::File(file) => {
                let Some(Location::File {
                    offset,
                    bytes,
                    digest,
                }) = self.locations.get(position)
                else {
                    return Ok(None);
                };
                let mut body = vec![0; *bytes];
                file.read_exact_at(&mut body, *offset)?;
                let actual: [u8; 32] = Sha256::digest(&body).into();
                if actual != *digest {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "committed ledger bytes changed after indexing",
                    ));
                }
                serde_json::from_slice(&body)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            }
            Source::Segmented(ledger) => {
                let ledger = ledger
                    .lock()
                    .map_err(|_| io::Error::other("segmented ledger lock poisoned"))?;
                match window {
                    Some(window) => window.read(&ledger, position as u64 + 1)?,
                    None => {
                        let Some(event) = ledger.get_at(position as u64 + 1)? else {
                            return Ok(None);
                        };
                        event
                    }
                }
            }
        };
        self.decodes += 1;
        let event = Arc::new(event);
        self.retain(position, Arc::clone(&event));
        Ok(Some(event))
    }

    fn touch(&mut self, position: usize) {
        if let Some(old) = self.recent.iter().position(|&item| item == position) {
            self.recent.remove(old);
        }
        self.recent.push_back(position);
    }

    fn retain(&mut self, position: usize, event: Arc<EventEnvelope>) {
        let bytes = retained_size(&event);
        // An individual large event may be read, but cannot displace the whole
        // cache and remain there above budget. Its caller alone owns that load.
        if bytes > self.budget {
            return;
        }
        while bytes > self.budget.saturating_sub(self.retained) {
            let Some(old) = self.recent.pop_front() else {
                break;
            };
            if let Some(cached) = self.cache.remove(&old) {
                self.retained -= cached.bytes;
            }
        }
        self.retained += bytes;
        self.cache.insert(position, Cached { event, bytes });
        self.touch(position);
    }
}

/// Conservative accounting, not a claim about allocator RSS. JSON text bytes
/// drastically undercount one-key BTreeMap objects. Charge an entire possible
/// tree node per entry, plus owned capacities, so tiny objects are not free.
pub(crate) fn retained_size(event: &EventEnvelope) -> usize {
    fn value_size(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(text) => text.capacity(),
            serde_json::Value::Array(items) => items
                .capacity()
                .saturating_mul(std::mem::size_of::<serde_json::Value>())
                .saturating_add(
                    items
                        .iter()
                        .map(value_size)
                        .fold(0usize, usize::saturating_add),
                ),
            serde_json::Value::Object(fields) => {
                let node = 16 * std::mem::size_of::<(String, serde_json::Value)>() + 128;
                fields.iter().fold(0usize, |total, (key, value)| {
                    total
                        .saturating_add(node)
                        .saturating_add(key.capacity())
                        .saturating_add(value_size(value))
                })
            }
            _ => 0,
        }
    }
    let strings = [
        &event.id,
        &event.stream,
        &event.time,
        &event.event_type,
        &event.source,
    ];
    let mut size = std::mem::size_of::<EventEnvelope>() + 128;
    for text in strings {
        size = size.saturating_add(text.capacity());
    }
    size = size.saturating_add(event.causes.capacity() * std::mem::size_of::<String>());
    for cause in &event.causes {
        size = size.saturating_add(cause.capacity());
    }
    if let Some(reason) = &event.reason {
        size = size.saturating_add(reason.capacity());
    }
    if let Some(origin) = &event.origin {
        size = size
            .saturating_add(origin.stream.capacity())
            .saturating_add(origin.event.capacity());
    }
    size.saturating_add(value_size(&event.payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Seek, SeekFrom, Write};

    fn fixture(budget: usize) -> (tempfile::NamedTempFile, EventStore, Vec<Vec<u8>>) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let lines: Vec<Vec<u8>> = (1..=3)
            .map(|seq| {
                serde_json::to_vec(&json!({
                    "v":1, "id":format!("ev_{seq}_test"), "seq":seq, "stream":"test",
                    "time":"2026-09-14T00:00:00Z", "type":"test.event", "source":"test",
                    "causes":[], "payload":{"text":"x".repeat(4096)}
                }))
                .unwrap()
            })
            .collect();
        for line in &lines {
            file.write_all(line).unwrap();
            file.write_all(b"\n").unwrap();
        }
        file.flush().unwrap();
        let mut store = EventStore::new(file.reopen().unwrap(), budget);
        let mut offset = 0;
        for line in &lines {
            store.register(offset, line);
            offset += line.len() as u64 + 1;
        }
        (file, store, lines)
    }

    #[test]
    fn indexing_is_cold_and_repeated_reads_share_one_decode() {
        let (_file, mut store, lines) = fixture(1024 * 1024);
        assert_eq!(store.decodes, 0);
        assert_eq!(store.retained, 0);
        let first = store.get(0).unwrap().unwrap();
        let again = store.get(0).unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(store.decodes, 1);
        assert_eq!(
            serde_json::to_value(first.as_ref()).unwrap(),
            serde_json::from_slice::<serde_json::Value>(&lines[0]).unwrap()
        );
        assert!(store.get(99).unwrap().is_none());
    }

    #[test]
    fn eviction_releases_unpinned_bodies_and_preserves_active_readers() {
        let (_file, mut store, _) = fixture(1024 * 1024);
        let active = store.get(0).unwrap().unwrap();
        store.budget = retained_size(&active);
        let second = store.get(1).unwrap().unwrap();
        let weak = Arc::downgrade(&second);
        drop(second);
        let _ = store.get(2).unwrap().unwrap();
        assert!(
            weak.upgrade().is_none(),
            "Evicted bodies must not be retained elsewhere in the store"
        );
        assert_eq!(
            active.seq, 1,
            "Eviction must not invalidate an active reader"
        );
        assert!(store.retained <= store.budget);
        assert!(!store.cache.contains_key(&0));
        assert!(!store.cache.contains_key(&1));
    }

    #[test]
    fn memory_counters_distinguish_hits_from_decodes_without_loading() {
        let (_file, mut store, _) = fixture(1024 * 1024);
        assert_eq!(store.memory_stats().decodes, 0);
        store.get(0).unwrap().unwrap();
        store.get(0).unwrap().unwrap();
        let stats = store.memory_stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.decodes, 1);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.estimated_retained_bytes, store.retained);
        assert_eq!(store.memory_stats().decodes, 1);
    }

    #[test]
    fn oversized_bodies_are_not_cached() {
        let (_file, mut store, _) = fixture(1);
        let first = store.get(0).unwrap().unwrap();
        let weak = Arc::downgrade(&first);
        drop(first);
        assert!(weak.upgrade().is_none());
        assert_eq!(store.retained, 0);
        store.get(0).unwrap().unwrap();
        assert_eq!(store.decodes, 2);
    }

    #[test]
    fn changed_or_truncated_bytes_are_errors_not_missing_events() {
        let (mut file, mut store, _) = fixture(0);
        file.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"!").unwrap();
        assert_eq!(store.get(0).unwrap_err().kind(), io::ErrorKind::InvalidData);
        file.as_file().set_len(0).unwrap();
        assert_eq!(
            store.get(1).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn pointer_objects_are_charged_more_than_their_text_size() {
        let (_file, mut store, _) = fixture(0);
        let mut event = store.get(0).unwrap().unwrap().as_ref().clone();
        event.payload = json!({"parts":(0..100).map(|i| json!({"event":format!("ev_{i}")})).collect::<Vec<_>>()});
        let text = serde_json::to_vec(&event).unwrap();
        assert!(retained_size(&event) > text.len() * 10);
    }
}
