//! Derived observations of a committed prefix, never replayed handlers.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ce, ContextGate, CONDENSE_PURPOSE, DECISION, MODEL_CHANNEL, SUMMARY};
use crate::kernel::log::Header;
use crate::LogReader;

const KEY: &str = "context-gate-observations";
const VERSION: u32 = 4;

#[cfg(test)]
#[path = "context_gate_recovery_tests.rs"]
mod tests;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observations<P = ce::PendingCalls> {
    dials: super::dials::Dials,
    pub main_completion: Option<String>,
    pub measured_request: Option<String>,
    pub system_request: Option<String>,
    pub forwarded_request: Option<String>,
    summaries: Vec<SummaryRef>,
    pub promoted: HashSet<String>,
    pending: P,
    epoch: u64,
    pub condense_completion: Option<String>,
    #[serde(default)]
    pub condense_failure: Option<String>,
}

impl Default for Observations {
    fn default() -> Self {
        Self {
            dials: Default::default(),
            main_completion: None,
            measured_request: None,
            system_request: None,
            forwarded_request: None,
            summaries: Vec::new(),
            promoted: HashSet::new(),
            pending: ce::PendingCalls::new(ce::MODEL_CALL_STARTED),
            epoch: 0,
            condense_completion: None,
            condense_failure: None,
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryRef {
    id: String,
    native: Option<(Value, Value)>,
}

#[derive(Default)]
pub(super) struct Recovery {
    state: Observations,
    through: u64,
    saved: u64,
    source: Option<crate::kernel::log::ReaderIdentity>,
}

pub(super) fn condense(header: &Header) -> bool {
    matches!(
        header.purpose.as_deref(),
        Some(CONDENSE_PURPOSE | "context.compact.responses")
    )
}

fn current_request(reader: &LogReader, request: Option<&Header>, epoch: u64) -> io::Result<bool> {
    let Some(request) =
        request.filter(|request| request.event_type == ce::MODEL_CALL_STARTED && condense(request))
    else {
        return Ok(false);
    };
    let mut copies = vec![request.clone()];
    let mut seen = HashSet::new();
    while let Some(copy) = copies.pop() {
        if !seen.insert(copy.id.clone()) {
            continue;
        }
        if copy.seq <= epoch {
            return Ok(false);
        }
        for cause in &copy.causes {
            let parent = reader
                .header(cause)?
                .ok_or_else(|| io::Error::other(format!("compaction cause missing: {cause}")))?;
            if parent.event_type == ce::MODEL_CALL_STARTED && parent.purpose == copy.purpose {
                copies.push(parent);
            }
        }
    }
    Ok(true)
}

fn failure_position(reader: &LogReader, id: &str) -> io::Result<u64> {
    let header = reader
        .header(id)?
        .ok_or_else(|| io::Error::other("compaction failure event missing"))?;
    if header.event_type == DECISION {
        let cause = header
            .causes
            .first()
            .ok_or_else(|| io::Error::other("compaction failure has no outcome"))?;
        return reader
            .header(cause)?
            .map(|header| header.seq)
            .ok_or_else(|| io::Error::other("compaction failure outcome missing"));
    }
    Ok(header.seq)
}

fn body(reader: &LogReader, id: &str) -> io::Result<crate::EventEnvelope> {
    reader
        .get(id)
        .map_err(|error| io::Error::new(error.kind(), format!("reading {id}: {error}")))?
        .ok_or_else(|| io::Error::other(format!("indexed recovery event missing: {id}")))
}

impl Observations {
    fn observe(&mut self, reader: &LogReader, header: &Header) -> io::Result<()> {
        // Track only compaction requests, so an ordinary ask that caused a
        // compaction is not mistaken for another copy of the same call.
        if condense(header) || ce::is_outcome(&header.event_type) {
            self.pending.observe(header.relations());
        }
        match header.event_type.as_str() {
            ce::MODEL_CALL_STARTED => {
                if !header.has_purpose {
                    if header.has_system {
                        self.system_request = Some(header.id.clone());
                    }
                    let cause = header
                        .causes
                        .first()
                        .map(|id| reader.header(id))
                        .transpose()?
                        .flatten();
                    if cause.is_some_and(|cause| cause.event_type == ce::MODEL_CALL_STARTED) {
                        self.forwarded_request = Some(header.id.clone());
                    }
                }
            }
            ce::MODEL_CALL_COMPLETED => {
                let request = header
                    .causes
                    .first()
                    .map(|id| reader.header(id))
                    .transpose()?
                    .flatten();
                if !request.as_ref().is_some_and(|request| request.has_purpose) {
                    self.main_completion = Some(header.id.clone());
                }
                if let Some(request) = request.as_ref().filter(|request| !request.has_purpose) {
                    self.measured_request = Some(request.id.clone());
                }
                if current_request(reader, request.as_ref(), self.epoch)? {
                    let event = body(reader, &header.id)?;
                    self.condense_completion = Some(header.id.clone());
                    if event.payload["status"] == "error" {
                        self.condense_failure = Some(header.id.clone());
                    }
                }
            }
            ce::EXTERNAL_INPUT => {
                let event = body(reader, &header.id)?;
                self.dials.observe(&event.payload);
                if event.payload["channel"] == MODEL_CHANNEL {
                    self.epoch = header.seq;
                    self.condense_completion = None;
                    self.condense_failure = None;
                }
            }
            SUMMARY => {
                let event = body(reader, &header.id)?;
                let native = event
                    .payload
                    .get("nativeCompaction")
                    .filter(|v| !v.is_null())
                    .map(|value| (value["model"].clone(), value["baseUrl"].clone()));
                if let Some(completion_id) = header.causes.first() {
                    let completion = reader
                        .header(completion_id)?
                        .ok_or_else(|| io::Error::other("summary outcome missing"))?;
                    let request = completion
                        .causes
                        .first()
                        .map(|id| reader.header(id))
                        .transpose()?
                        .flatten();
                    if completion.event_type == ce::MODEL_CALL_COMPLETED
                        && request.as_ref().is_some_and(condense)
                    {
                        if !current_request(reader, request.as_ref(), self.epoch)?
                            || self.condense_completion.as_deref() != Some(completion_id)
                        {
                            return Ok(());
                        }
                        let newer = self
                            .condense_failure
                            .as_deref()
                            .map(|id| failure_position(reader, id))
                            .transpose()?
                            .is_none_or(|failed| completion.seq >= failed);
                        if newer {
                            self.condense_failure = None;
                        }
                    }
                }
                self.summaries.retain(|summary| summary.native != native);
                self.summaries.push(SummaryRef {
                    id: header.id.clone(),
                    native,
                });
            }
            DECISION => {
                let event = body(reader, &header.id)?;
                if event.payload["action"] == "promote" {
                    if let Some(names) = event.payload["promoted"].as_array() {
                        self.promoted
                            .extend(names.iter().filter_map(Value::as_str).map(str::to_string));
                    }
                } else if event.payload["action"] == "suspend" {
                    if let Some(completion) = event
                        .causes
                        .iter()
                        .find(|id| self.condense_completion.as_ref() == Some(id))
                    {
                        // Provider failures already supply their error object. A
                        // rejected successful response is explained by the decision.
                        if self.condense_failure.as_ref() != Some(completion) {
                            self.condense_failure = Some(event.id.clone());
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn latest_summary(&self, enabled: bool, target: Option<&Value>) -> Option<String> {
        self.summaries
            .iter()
            .rev()
            .find(|summary| match &summary.native {
                None => true,
                Some((model, url)) => {
                    enabled
                        && target.is_none_or(|target| {
                            target["model"] == *model && target["baseUrl"] == *url
                        })
                }
            })
            .map(|summary| summary.id.clone())
    }

    pub fn in_flight(&self) -> bool {
        !self.pending.requests().is_empty()
    }
}

impl ContextGate {
    pub(super) fn restore_dials(&mut self, reader: &LogReader, before: u64) -> io::Result<()> {
        self.restore_dials_through(reader, before.saturating_sub(1))
    }

    pub(super) fn restore_dials_through(
        &mut self,
        reader: &LogReader,
        through: u64,
    ) -> io::Result<()> {
        if !self.restored_dials {
            let dials = self.observe_prefix(reader, through, |state| state.dials.clone())?;
            dials.apply(self);
            self.restored_dials = true;
        }
        Ok(())
    }

    pub(super) fn observation<T>(
        &self,
        reader: &LogReader,
        read: impl FnOnce(&Observations) -> T,
    ) -> Result<T, String> {
        self.observe_prefix(reader, reader.snapshot_end(), read)
            .map_err(|error| error.to_string())
    }

    fn observe_prefix<T>(
        &self,
        reader: &LogReader,
        through: u64,
        read: impl FnOnce(&Observations) -> T,
    ) -> io::Result<T> {
        self.recovery.borrow_mut().read(reader, through, true, read)
    }
}

impl Observations<HashMap<String, ()>> {
    fn upgrade(mut self, reader: &LogReader, through: u64) -> io::Result<Observations> {
        // v2 retained only request IDs, including forwarded roots whose child
        // had already answered. Rebuild only the potentially unresolved tail,
        // not all archived history, using the contract's settlement logic.
        let mut first = None;
        let mut queue: Vec<_> = self.pending.keys().cloned().collect();
        let mut seen = HashSet::new();
        while let Some(id) = queue.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let header = reader.header(&id)?.ok_or_else(|| {
                io::Error::other(format!("pending compaction request missing: {id}"))
            })?;
            if condense(&header) {
                // A legacy cache may retain only a forwarded child after its
                // root was interrupted. Replay from that root, not the child.
                first = Some(first.map_or(header.seq, |seq: u64| seq.min(header.seq)));
                queue.extend(header.causes.iter().cloned());
            }
        }
        let mut pending = ce::PendingCalls::new(ce::MODEL_CALL_STARTED);
        if let Some(first) = first {
            reader.try_visit_header_range(first, through, |headers| {
                for header in headers {
                    if condense(header) || ce::is_outcome(&header.event_type) {
                        pending.observe(header.relations());
                    }
                }
                Ok(())
            })?;
        }
        if let Some(id) = self.condense_completion.as_ref() {
            if body(reader, id)?.payload["status"] == "error" {
                self.condense_failure = Some(id.clone());
            }
        }
        Ok(Observations {
            dials: self.dials,
            main_completion: self.main_completion,
            measured_request: self.measured_request,
            system_request: self.system_request,
            forwarded_request: self.forwarded_request,
            summaries: self.summaries,
            promoted: self.promoted,
            pending,
            epoch: self.epoch,
            condense_completion: self.condense_completion,
            condense_failure: self.condense_failure,
        })
    }
}

impl Recovery {
    pub(super) fn matches(&self, reader: &LogReader) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| source.matches(reader))
    }

    pub(super) fn read<T>(
        &mut self,
        reader: &LogReader,
        through: u64,
        persist: bool,
        read: impl FnOnce(&Observations) -> T,
    ) -> io::Result<T> {
        let recovery = self;
        let cold = recovery
            .source
            .as_ref()
            .is_none_or(|source| !source.matches(reader));
        if cold {
            let mut checkpoint = reader.load_checkpoint::<Observations>(KEY, VERSION, through)?;
            if checkpoint.state.is_none() {
                let legacy =
                    reader.load_checkpoint::<Observations<HashMap<String, ()>>>(KEY, 2, through)?;
                if let Some(state) = legacy.state {
                    checkpoint.state = Some(state.upgrade(reader, legacy.through)?);
                    checkpoint.through = legacy.through;
                    checkpoint.cold_reason = legacy.cold_reason;
                }
            }
            if let Some(reason) = checkpoint.cold_reason {
                if persist && reader.path().is_some() {
                    eprintln!("slow recovery for {KEY}: {reason}");
                }
            }
            recovery.state = checkpoint.state.unwrap_or_default();
            recovery.through = checkpoint.through;
            recovery.saved = checkpoint.through;
            recovery.source = Some(reader.identity());
        }
        if through < recovery.through {
            return Err(io::Error::other(
                "context observation precedes its recovered prefix",
            ));
        }
        reader.try_visit_header_range(recovery.through + 1, through, |batch| {
            for header in batch {
                recovery.state.observe(reader, header)?;
                // Advance only after a successful observation: a later read
                // failure must not make the next query skip unseen records.
                recovery.through = header.seq;
            }
            Ok(())
        })?;
        if persist && (cold || through.saturating_sub(recovery.saved) >= 256) {
            match reader.save_checkpoint(KEY, VERSION, through, &recovery.state) {
                Ok(_) => recovery.saved = through,
                Err(error) => eprintln!("cannot save derived recovery state for {KEY}: {error}"),
            }
        }
        Ok(read(&recovery.state))
    }
}

pub(super) fn empty() -> RefCell<Recovery> {
    RefCell::new(Recovery::default())
}
