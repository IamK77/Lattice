//! Exact transcript recovery, separate from current usage and authorization
//! state. Historical reads materialize cards; they never replay UI actions.

use std::collections::BTreeMap;
use std::io;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::{core_events as ce, EventEnvelope, LogReader};

use super::pages::{Pages, Slot};
use super::{command_job, ingest, Entry, ToolStatus};
use record::{Kind, Record};

pub(super) mod record;

#[cfg(test)]
mod tests;

const CONSUMER: &str = "view-cards";
const VERSION: u32 = 3;
const CHECKPOINT_EVERY: u64 = 256;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    pages: Vec<Slot>,
    count: usize,
    running: BTreeMap<usize, Option<String>>,
    has_user: bool,
    last_handled: bool,
}

/// A stable display ordinal addresses one card, regardless of which page the
/// frontend has loaded. Only live running candidates remain in the in-memory
/// lookup table; completed command identities live in bounded recipe pages.
pub struct History {
    reader: LogReader,
    // A local cleared view starts here; it must never replace the full-history checkpoint.
    base: Option<u64>,
    pages: Pages<Record>,
    running: BTreeMap<usize, Option<String>>,
    has_user: bool,
    last_handled: bool,
    through: u64,
    saved_through: u64,
    failed: bool,
    cold_reason: Option<String>,
}

impl History {
    /// Restore the exact saved prefix, then fold only its committed tail.
    /// An unavailable or incompatible checkpoint is an explicit cold rebuild.
    pub fn recover(reader: LogReader, through: u64) -> io::Result<Self> {
        if through > reader.snapshot_end() {
            return Err(invalid(
                "card recovery boundary is ahead of committed history",
            ));
        }
        let checkpoint = reader.load_checkpoint::<State>(CONSUMER, VERSION, through)?;
        let boundary = checkpoint.through;
        match Self::from_state(
            reader.clone(),
            through,
            None,
            boundary,
            checkpoint.state.unwrap_or_default(),
            checkpoint.cold_reason,
        ) {
            Err(error) if boundary > 0 => Self::from_state(
                reader,
                through,
                None,
                0,
                State::default(),
                Some(error.to_string()),
            ),
            result => result,
        }
    }

    pub fn rebuild(&self, through: u64, reason: String) -> io::Result<Self> {
        let through = through.max(self.through);
        if through > self.reader.snapshot_end() {
            return Err(invalid(
                "card rebuild boundary is ahead of committed history",
            ));
        }
        Self::from_state(
            self.reader.clone(),
            through,
            self.base,
            self.base.unwrap_or(0),
            State::default(),
            Some(reason),
        )
    }

    /// Start a local display at this boundary without changing the canonical
    /// transcript checkpoint. Later unmatched results retain normal ingest behavior.
    pub fn empty_tail(&self) -> io::Result<Self> {
        Self::from_state(
            self.reader.clone(),
            self.through,
            Some(self.through),
            self.through,
            State::default(),
            None,
        )
    }

    fn from_state(
        reader: LogReader,
        through: u64,
        base: Option<u64>,
        mut boundary: u64,
        state: State,
        mut cold_reason: Option<String>,
    ) -> io::Result<Self> {
        let root = reader
            .path()
            .filter(|path| path.is_dir())
            .map(ToOwned::to_owned);
        let mut running = state.running;
        let mut has_user = state.has_user;
        let mut last_handled = state.last_handled;
        let pages = match Pages::open(root.clone(), state.pages, state.count) {
            Ok(pages) if running.keys().all(|index| *index < pages.len()) => pages,
            result => {
                cold_reason = Some(match result {
                    Err(error) => error.to_string(),
                    Ok(_) => "running card is outside recovered history".into(),
                });
                boundary = base.unwrap_or(0);
                running.clear();
                has_user = false;
                last_handled = false;
                Pages::open(root, Vec::new(), 0)?
            }
        };
        let mut history = Self {
            reader,
            base,
            pages,
            running,
            has_user,
            last_handled,
            through: boundary,
            saved_through: boundary,
            failed: false,
            cold_reason,
        };
        history.catch_up(through)?;
        history.save()?;
        Ok(history)
    }

    pub fn cold_reason(&self) -> Option<&str> {
        self.cold_reason.as_deref()
    }

    pub fn through(&self) -> u64 {
        self.through
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn has_user(&self) -> bool {
        self.has_user
    }

    pub fn last_handled(&self) -> bool {
        self.last_handled
    }

    pub fn last_is_thinking(&self) -> io::Result<bool> {
        self.healthy()?;
        match self.len().checked_sub(1) {
            Some(index) => Ok(self.pages.get(index)?.identity.kind == Kind::Thinking),
            None => Ok(false),
        }
    }

    pub fn last_tool_running(&self) -> bool {
        self.len()
            .checked_sub(1)
            .is_some_and(|index| self.running.contains_key(&index))
    }

    /// Spacing needs only the recipe's kind, never a preceding card's body.
    pub fn transcript_kind(&self, index: usize) -> io::Result<super::TranscriptKind> {
        self.healthy()?;
        Ok(match self.pages.get(index)?.identity.kind {
            Kind::User | Kind::Anchor => super::TranscriptKind::Anchor,
            Kind::Tool | Kind::Thinking => super::TranscriptKind::Work,
            Kind::Other => super::TranscriptKind::Reply,
        })
    }

    /// A failed incremental fold must be recovered again, never retried on its
    /// partially changed working copy or published as an exact checkpoint.
    fn healthy(&self) -> io::Result<()> {
        if self.failed {
            Err(invalid(
                "card projection requires recovery after a failed fold",
            ))
        } else {
            Ok(())
        }
    }

    pub fn catch_up(&mut self, through: u64) -> io::Result<bool> {
        self.healthy()?;
        if through < self.through || through > self.reader.snapshot_end() {
            return Err(invalid("card catch-up boundary is outside committed tail"));
        }
        let reader = self.reader.clone();
        let mut handled = false;
        reader.visit_range(self.through + 1, through, |batch| {
            for event in batch {
                handled = self.observe(event)?;
            }
            Ok(())
        })?;
        Ok(handled)
    }

    /// Observe each committed event once. The returned flag has the existing
    /// ingest meaning (handled), including swallowed duplicate command wakes.
    fn observe(&mut self, event: &EventEnvelope) -> io::Result<bool> {
        self.healthy()?;
        if event.seq != self.through + 1 || event.stream != self.reader.stream() {
            return Err(invalid(
                "card projection event is outside its next prefix boundary",
            ));
        }
        let result = self.fold(event);
        match result {
            Ok(handled) => {
                self.last_handled = handled;
                self.has_user |= event.event_type == ce::USER_MESSAGE && event.causes.is_empty();
                self.through = event.seq;
                if self.through - self.saved_through >= CHECKPOINT_EVERY {
                    self.save()?;
                }
                Ok(handled)
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn fold(&mut self, event: &EventEnvelope) -> io::Result<bool> {
        if event.event_type == ce::TOOL_EXEC_STARTED {
            if let Some(call) = event.payload["call"].as_str() {
                if self
                    .running
                    .values()
                    .any(|candidate| candidate.as_deref() == Some(call))
                {
                    return Ok(false);
                }
            }
        }
        if event.event_type == ce::TOOL_EXEC_COMPLETED {
            let call = event.payload["call"].as_str();
            let target = self.running.iter().rev().find_map(|(index, candidate)| {
                let matches = match (call, candidate.as_deref()) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                };
                matches.then_some(*index)
            });
            let Some(index) = target else {
                return Ok(false);
            };
            let mut record = self.pages.get(index)?;
            let mut entries = vec![self.materialize(&record)?];
            if !ingest(&mut entries, event) || entries.len() != 1 {
                return Err(invalid("running card does not accept its completion"));
            }
            record.completion = Some(event.id.clone());
            self.pages.replace(index, record)?;
            self.running.remove(&index);
            return Ok(true);
        }
        if event.event_type == ce::WAKE {
            if let Some(job) = command_job(&event.payload) {
                if let Some(index) = self.pages.find_last(job)? {
                    let mut record = self.pages.get(index)?;
                    let entry = self.materialize(&record)?;
                    let Entry::Tool(card) = &entry else {
                        return Err(invalid("command index addresses a non-tool card"));
                    };
                    if !matches!(
                        card.status,
                        ToolStatus::Running | ToolStatus::Background | ToolStatus::Unknown
                    ) {
                        return Ok(true);
                    }
                    let mut entries = vec![entry];
                    if !ingest(&mut entries, event) || entries.len() != 1 {
                        return Err(invalid("command card does not accept its wake"));
                    }
                    record.wake = Some(event.id.clone());
                    self.pages.replace(index, record)?;
                    self.running.remove(&index);
                    return Ok(true);
                }
            }
        }
        let mut entries = Vec::new();
        let handled = ingest(&mut entries, event);
        for (ordinal, entry) in entries.iter().enumerate() {
            let record = Record::new(&event.id, ordinal, entry);
            let index = self.pages.push(record)?;
            if let Entry::Tool(card) = entry {
                if card.status == ToolStatus::Running {
                    self.running.insert(index, card.call.clone());
                }
            }
        }
        Ok(handled)
    }

    fn materialize(&self, record: &Record) -> io::Result<Entry> {
        record.materialize(|id| {
            let event = self
                .reader
                .get(id)?
                .ok_or_else(|| invalid("card source event is absent"))?;
            if event.seq > self.through {
                return Err(invalid("card source lies beyond the recovered prefix"));
            }
            Ok(event)
        })
    }

    pub fn get(&self, index: usize) -> io::Result<Entry> {
        self.healthy()?;
        self.materialize(&self.pages.get(index)?)
    }

    pub fn load(&self, range: Range<usize>) -> io::Result<Vec<Entry>> {
        self.healthy()?;
        if range.start > range.end || range.end > self.len() {
            return Err(invalid("card page request is outside history"));
        }
        range.map(|index| self.get(index)).collect()
    }

    /// Locate complete display groups without materializing their text.
    pub fn group(&self, index: usize) -> io::Result<Range<usize>> {
        self.healthy()?;
        let kind = self.pages.get(index)?.identity.kind;
        let joins = |other| match kind {
            Kind::User => other == Kind::User,
            Kind::Tool | Kind::Thinking => matches!(other, Kind::Tool | Kind::Thinking),
            Kind::Anchor | Kind::Other => false,
        };
        let mut first = index;
        while first > 0 && joins(self.pages.get(first - 1)?.identity.kind) {
            first -= 1;
        }
        let mut end = index + 1;
        while end < self.len() && joins(self.pages.get(end)?.identity.kind) {
            end += 1;
        }
        Ok(first..end)
    }

    pub fn save(&mut self) -> io::Result<()> {
        self.healthy()?;
        if self.base.is_some() {
            self.saved_through = self.through;
            return Ok(());
        }
        let state = State {
            pages: self.pages.directory()?,
            count: self.pages.len(),
            running: self.running.clone(),
            has_user: self.has_user,
            last_handled: self.last_handled,
        };
        self.reader
            .save_checkpoint(CONSUMER, VERSION, self.through, &state)?;
        self.saved_through = self.through;
        Ok(())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
