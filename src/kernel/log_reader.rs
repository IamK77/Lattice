//! Fallible, bounded access to historical bodies. Missing identities are not
//! read failures, and callbacks cannot turn a failed read into a false answer.

use std::io;
use std::sync::Arc;

use super::{LogReader, Nearby};
use crate::kernel::history::HeaderCursor;
use crate::EventEnvelope;

const BATCH_BYTES: usize = 64 * 1024 * 1024;
const HEADER_BATCH_BYTES: usize = 1024 * 1024;

impl LogReader {
    /// Inspect a closed segmented ledger without opening a source writer or
    /// repairing its tail. Missing derived indexes may be rebuilt. Drop every
    /// clone before starting a runtime on the same ledger.
    pub fn segmented_snapshot(path: &std::path::Path) -> io::Result<Self> {
        let ledger = crate::kernel::segmented::Ledger::snapshot(path)?;
        if ledger.recovery.rebuilt_indexes > 0 {
            eprintln!(
                "warning: {} rebuilt {} derived indexes for inspection",
                path.display(),
                ledger.recovery.rebuilt_indexes
            );
        }
        let stream = ledger.stream().to_owned();
        let history = crate::kernel::history::History::segmented(
            Arc::new(std::sync::Mutex::new(ledger)),
            super::HISTORY_CACHE_BYTES,
        )?;
        Ok(Self {
            stream,
            path: Some(path.to_owned()),
            history: Arc::new(std::sync::RwLock::new(history)),
            pending: Arc::default(),
            cost: Arc::default(),
        })
    }

    pub(crate) fn identity(&self) -> super::ReaderIdentity {
        super::ReaderIdentity(Arc::downgrade(&self.history))
    }

    /// Best-effort accounting: never reads a body or waits for a busy lock.
    pub fn memory_stats(&self) -> io::Result<crate::memory::HistoryStats> {
        self.history
            .try_read()
            .map_err(|error| match error {
                std::sync::TryLockError::WouldBlock => {
                    io::Error::new(io::ErrorKind::WouldBlock, "log history busy")
                }
                std::sync::TryLockError::Poisoned(_) => {
                    io::Error::other("log history lock poisoned")
                }
            })?
            .memory_stats()
    }

    /// Identity checks never decode the body and remain usable after a read failure.
    pub fn contains_id(&self, id: &str) -> io::Result<bool> {
        Ok(self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?
            .position(id)?
            .is_some())
    }

    pub(crate) fn header(&self, id: &str) -> io::Result<Option<crate::kernel::history::Header>> {
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        history.header(id)
    }

    pub(crate) fn scan_back_headers<T>(
        &self,
        mut pick: impl FnMut(&crate::kernel::history::Header, Nearby<'_>) -> io::Result<Option<T>>,
    ) -> io::Result<Option<T>> {
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        let nearby = Nearby { history: &history };
        let mut cursor = HeaderCursor::new(0, history.len(), true);
        while let Some((header, _)) = cursor.next(&history)? {
            if let Some(value) = pick(&header, nearby)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    pub(crate) fn event_type(&self, id: &str) -> io::Result<Option<String>> {
        Ok(self.header(id)?.map(|header| header.event_type))
    }

    pub fn latest_id(&self) -> Option<String> {
        self.history
            .read()
            .expect("log history lock poisoned")
            .latest_id()
    }

    pub fn has_outcome(&self, started_id: &str) -> io::Result<bool> {
        self.history
            .read()
            .expect("log history lock poisoned")
            .has_outcome(started_id)
    }

    pub(crate) fn any_header(
        &self,
        test: impl Fn(&crate::kernel::history::Header) -> bool,
    ) -> io::Result<bool> {
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        let mut cursor = HeaderCursor::new(0, history.len(), false);
        while let Some((header, _)) = cursor.next(&history)? {
            if test(&header) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn snapshot_end(&self) -> u64 {
        self.history
            .read()
            .expect("log history lock poisoned")
            .len() as u64
    }

    /// Visit an inclusive sequence range captured at entry, without scanning
    /// earlier headers. Appends made by a callback belong to a later snapshot.
    pub(crate) fn visit_header_range(
        &self,
        from_seq: u64,
        through: u64,
        mut visit: impl FnMut(&[crate::kernel::history::Header]),
    ) -> io::Result<()> {
        self.try_visit_header_range(from_seq, through, |batch| {
            visit(batch);
            Ok(())
        })
    }

    pub(crate) fn try_visit_header_range(
        &self,
        from_seq: u64,
        through: u64,
        mut visit: impl FnMut(&[crate::kernel::history::Header]) -> io::Result<()>,
    ) -> io::Result<()> {
        let (start, end) = self.range_bounds(from_seq, through)?;
        let mut cursor = HeaderCursor::new(start, end, false);
        let mut pending = None;
        loop {
            let history = self
                .history
                .read()
                .map_err(|_| io::Error::other("log history lock poisoned"))?;
            let mut batch = Vec::new();
            let mut bytes = 0usize;
            for _ in 0..64 {
                let header = match pending.take() {
                    Some(header) => header,
                    None => {
                        let Some((header, _)) = cursor.next(&history)? else {
                            break;
                        };
                        header
                    }
                };
                let size = header.retained_size();
                if !batch.is_empty() && size > HEADER_BATCH_BYTES.saturating_sub(bytes) {
                    pending = Some(header);
                    break;
                }
                bytes = bytes.saturating_add(size);
                batch.push(header);
                if bytes >= HEADER_BATCH_BYTES {
                    break;
                }
            }
            drop(history);
            if batch.is_empty() {
                return Ok(());
            }
            visit(&batch)?;
        }
    }

    /// Process a fixed prefix without retaining the whole conversation. One
    /// oversized event is delivered alone. Callback errors propagate; callers
    /// rebuilding state must not publish partial recovery as success.
    pub fn visit_prefix(
        &self,
        through: u64,
        visit: impl FnMut(&[Arc<EventEnvelope>]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.visit_range(1, through, visit)
    }

    /// Read the preceding page in chronological order, seeking directly to
    /// `before` rather than scanning from today's tail. Limits apply to event
    /// count and recorded source bytes; one oversized event is returned alone.
    pub fn page_before(
        &self,
        before: u64,
        event_limit: usize,
        byte_limit: u64,
    ) -> io::Result<Vec<Arc<EventEnvelope>>> {
        let through = before
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("invalid page boundary"))?;
        if event_limit == 0 || byte_limit == 0 || through > self.snapshot_end() {
            return Err(io::Error::other("page boundary or limits are invalid"));
        }
        let (start, end) = self.range_bounds(1, through)?;
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        let mut cursor = HeaderCursor::new(start, end, true);
        let mut result = Vec::new();
        let mut bytes = 0u64;
        while result.len() < event_limit {
            let Some((header, size)) = cursor.next(&history)? else {
                break;
            };
            if !result.is_empty() && size > byte_limit.saturating_sub(bytes) {
                break;
            }
            result.push(cursor.load(&history, header.seq)?);
            bytes = bytes.saturating_add(size);
            if bytes >= byte_limit {
                break;
            }
        }
        result.reverse();
        Ok(result)
    }

    /// Read only this inclusive sequence range in bounded batches. The upper
    /// boundary is fixed at entry, including when `through` exceeds the tail.
    /// Rebuilding an old page must not require decoding the preceding pages.
    pub fn visit_range(
        &self,
        from_seq: u64,
        through: u64,
        mut visit: impl FnMut(&[Arc<EventEnvelope>]) -> io::Result<()>,
    ) -> io::Result<()> {
        let (start, end) = self.range_bounds(from_seq, through)?;
        let mut cursor = HeaderCursor::new(start, end, false);
        let mut pending = None;
        loop {
            let history = self
                .history
                .read()
                .map_err(|_| io::Error::other("log history lock poisoned"))?;
            let mut batch = Vec::new();
            let mut bytes = 0usize;
            while batch.len() < 64 {
                let event = match pending.take() {
                    Some(event) => event,
                    None => {
                        let Some((header, _)) = cursor.next(&history)? else {
                            break;
                        };
                        cursor.load(&history, header.seq)?
                    }
                };
                let size = crate::kernel::event_store::retained_size(&event);
                if !batch.is_empty() && size > BATCH_BYTES.saturating_sub(bytes) {
                    pending = Some(event);
                    break;
                }
                bytes = bytes.saturating_add(size);
                batch.push(event);
                if bytes >= BATCH_BYTES {
                    break;
                }
            }
            drop(history);
            if batch.is_empty() {
                return Ok(());
            }
            self.cost.note(0, 0);
            visit(&batch)?;
        }
    }

    fn range_bounds(&self, from_seq: u64, through: u64) -> io::Result<(usize, usize)> {
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        let end = usize::try_from(through)
            .unwrap_or(usize::MAX)
            .min(history.len());
        let start = usize::try_from(from_seq.max(1) - 1)
            .unwrap_or(usize::MAX)
            .min(end);
        Ok((start, end))
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
    pub fn stream(&self) -> &str {
        &self.stream
    }

    pub fn get(&self, id: &str) -> io::Result<Option<EventEnvelope>> {
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        Ok(history.get(id)?.map(|event| event.as_ref().clone()))
    }

    /// Explicitly requesting all bodies still allocates all returned bodies.
    /// Normal recovery and state queries must use bounded scans instead.
    pub fn replay(&self, from_seq: u64) -> io::Result<Vec<EventEnvelope>> {
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        let start = usize::try_from(from_seq.max(1) - 1)
            .unwrap_or(usize::MAX)
            .min(history.len());
        let mut events = Vec::new();
        let mut bytes = 0;
        let mut cursor = HeaderCursor::new(start, history.len(), false);
        while let Some((header, size)) = cursor.next(&history)? {
            events.push(cursor.load(&history, header.seq)?.as_ref().clone());
            bytes += size;
        }
        self.cost.note(
            events.len(),
            if start == 0 {
                history.byte_len()
            } else {
                bytes
            },
        );
        Ok(events)
    }

    pub fn scan_back<T>(
        &self,
        pick: impl FnMut(&EventEnvelope, Nearby<'_>) -> io::Result<Option<T>>,
    ) -> io::Result<Option<T>> {
        self.scan_back_types(&[], pick)
    }

    /// Filter using the small index before loading bodies, so looking for one
    /// event type does not decode every unrelated model request on the way.
    pub fn scan_back_types<T>(
        &self,
        types: &[&str],
        mut pick: impl FnMut(&EventEnvelope, Nearby<'_>) -> io::Result<Option<T>>,
    ) -> io::Result<Option<T>> {
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        self.cost.note(0, 0);
        let nearby = Nearby { history: &history };
        let mut cursor = HeaderCursor::new(0, history.len(), true);
        while let Some((header, _)) = cursor.next(&history)? {
            if !types.is_empty() && !types.contains(&header.event_type.as_str()) {
                continue;
            }
            let event = cursor.load(&history, header.seq)?;
            if let Some(result) = pick(&event, nearby)? {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    pub fn find_back(
        &self,
        pick: impl Fn(&EventEnvelope) -> bool,
    ) -> io::Result<Option<EventEnvelope>> {
        self.scan_back(|event, _| Ok(pick(event).then(|| event.clone())))
    }

    pub fn any(&self, pred: impl Fn(&EventEnvelope) -> bool) -> io::Result<bool> {
        Ok(self
            .scan_back(|event, _| Ok(pred(event).then_some(())))?
            .is_some())
    }

    pub fn collect_where(
        &self,
        keep: impl Fn(&EventEnvelope) -> bool,
    ) -> io::Result<Vec<EventEnvelope>> {
        let mut taken = Vec::new();
        let history = self
            .history
            .read()
            .map_err(|_| io::Error::other("log history lock poisoned"))?;
        let mut bytes = 0;
        let mut cursor = HeaderCursor::new(0, history.len(), false);
        while let Some((header, size)) = cursor.next(&history)? {
            let event = cursor.load(&history, header.seq)?;
            if keep(&event) {
                bytes += size;
                taken.push(event.as_ref().clone());
            }
        }
        self.cost.note(taken.len(), bytes);
        Ok(taken)
    }

    pub fn cost(&self) -> &super::ReadCost {
        &self.cost
    }
    pub fn len(&self) -> usize {
        self.history
            .read()
            .expect("log history lock poisoned")
            .len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod observation_tests {
    use crate::{EventDraft, EventLog, EventTypeDecl};
    use serde_json::json;

    fn append(log: &mut EventLog) {
        log.append(
            EventDraft::new("fixture.event", &[], json!({"text": "range"})),
            "fixture",
        )
        .unwrap();
    }

    fn memory_log(count: usize) -> EventLog {
        let mut log =
            EventLog::in_memory(vec![EventTypeDecl::new("fixture.event", "range")], "range");
        for _ in 0..count {
            append(&mut log);
        }
        log
    }

    #[test]
    fn range_reads_only_requested_bodies_across_segments() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("range.ledger");
        let types = vec![EventTypeDecl::new("fixture.event", "range")];
        let mut log = EventLog::open_segmented(types.clone(), "range", path.clone(), 1).unwrap();
        for _ in 0..6 {
            append(&mut log);
        }
        drop(log);
        let log = EventLog::open(types, "range", Some(path)).unwrap();
        let reader = log.reader();
        let mut seen = Vec::new();
        reader
            .visit_range(4, 5, |batch| {
                seen.extend(batch.iter().map(|event| event.seq));
                Ok(())
            })
            .unwrap();
        assert_eq!(seen, [4, 5]);
        assert_eq!(reader.memory_stats().unwrap().cache.unwrap().decodes, 2);
        for (from, through) in [(0, 0), (5, 4), (u64::MAX, u64::MAX)] {
            reader
                .visit_range(from, through, |_| panic!("empty range visited a body"))
                .unwrap();
        }
        let mut headers = Vec::new();
        reader
            .visit_header_range(3, 4, |batch| {
                headers.extend(batch.iter().map(|header| header.seq))
            })
            .unwrap();
        assert_eq!(headers, [3, 4]);
        assert_eq!(reader.memory_stats().unwrap().cache.unwrap().decodes, 2);
    }

    #[test]
    fn range_snapshot_does_not_grow_during_callbacks() {
        let mut log = memory_log(130);
        let reader = log.reader();
        let mut seen = Vec::new();
        reader
            .visit_range(0, u64::MAX, |batch| {
                assert!(batch.len() <= 64);
                seen.extend(batch.iter().map(|event| event.seq));
                if seen.len() == 64 {
                    append(&mut log);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(seen, (1..=130).collect::<Vec<_>>());
        let through = reader.snapshot_end();
        let mut headers = Vec::new();
        reader
            .visit_header_range(1, u64::MAX, |batch| {
                headers.extend(batch.iter().map(|header| header.seq));
                if headers.len() == 64 {
                    append(&mut log);
                }
            })
            .unwrap();
        assert_eq!(headers, (1..=through).collect::<Vec<_>>());
    }

    #[test]
    fn header_batches_are_byte_bounded_and_deliver_oversized_records_alone() {
        let mut log = memory_log(0);
        for bytes in [600 * 1024, 600 * 1024, 2 * super::HEADER_BATCH_BYTES, 16] {
            log.append(
                EventDraft::new("fixture.event", &[], json!({})),
                &"s".repeat(bytes),
            )
            .unwrap();
        }
        let mut seen = Vec::new();
        log.reader()
            .visit_header_range(1, 4, |batch| {
                let bytes: usize = batch.iter().map(|header| header.retained_size()).sum();
                assert!(batch.len() == 1 || bytes <= super::HEADER_BATCH_BYTES);
                seen.extend(batch.iter().map(|header| header.seq));
            })
            .unwrap();
        assert_eq!(seen, [1, 2, 3, 4]);
    }

    #[test]
    fn range_stops_at_the_first_callback_error() {
        let log = memory_log(130);
        let mut calls = 0;
        let error = log
            .reader()
            .visit_range(1, 130, |_| {
                calls += 1;
                Err(std::io::Error::other("fixture callback failed"))
            })
            .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error.to_string(), "fixture callback failed");
    }

    #[test]
    fn memory_observation_does_not_wait_for_a_history_writer() {
        let log = crate::EventLog::in_memory(crate::core_events::core_event_decls(), "observation");
        let reader = log.reader();
        let held = reader.history.write().unwrap();
        let other = reader.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || tx.send(other.memory_stats()).unwrap());
        let observed = rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(held);
        worker.join().unwrap();
        assert_eq!(
            observed
                .expect("observation waited for the writer")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
