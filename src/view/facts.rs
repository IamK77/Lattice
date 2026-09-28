//! Per-event facts for frontend recovery. Sizes and historical request pairing
//! live in paged derived records, not ever-growing frontend hash maps.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{core_events as ce, EventEnvelope, LogReader};

use super::pages::{Item, Pages, Slot};

#[cfg(test)]
mod tests;

const CONSUMER: &str = "view-event-facts";
const VERSION: u32 = 2;
const CHECKPOINT_EVERY: u64 = 256;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MaterialKind {
    User,
    Reply,
    ToolResult,
    ToolCall,
    Wake,
    Other,
}

impl MaterialKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "what you said",
            Self::Reply => "model replies",
            Self::ToolResult => "tool results",
            Self::ToolCall => "tool calls",
            Self::Wake => "wakes",
            Self::Other => "other events",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterialSize {
    pub kind: MaterialKind,
    pub bytes: u64,
    pub thinking: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelStart {
    pub model: String,
    pub millis: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub source: String,
    pub size: MaterialSize,
    pub model_start: Option<ModelStart>,
    /// The request actually paired at this event's own historical boundary.
    /// Looking up the latest request at read time would leak a later call reuse.
    pub related_start: Option<String>,
    lookup_call: Option<String>,
    model_consumed: bool,
}

impl Item for Fact {
    fn lookup_key(&self) -> Option<&str> {
        self.lookup_call.as_deref()
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    pages: Vec<Slot>,
    count: usize,
}

// Cache only immutable material sizes, never mutable pairing state. Bound
// both key length and entry count so imported identities cannot grow it freely.
const SIZE_CACHE_ENTRIES: usize = 64 * 1024;
const SIZE_CACHE_KEY_BYTES: usize = 64;

enum CachedSize {
    Present(u64, MaterialSize),
    Absent(u64),
}

#[derive(Default)]
struct SizeCache {
    entries: HashMap<String, CachedSize>,
    #[cfg(test)]
    lookups: usize,
}

pub struct EventFacts {
    reader: LogReader,
    pages: Pages<Fact>,
    through: u64,
    saved_through: u64,
    failed: bool,
    cold_reason: Option<String>,
    sizes: RefCell<SizeCache>,
}

impl EventFacts {
    pub fn recover(reader: LogReader, through: u64) -> io::Result<Self> {
        if through > reader.snapshot_end() {
            return Err(invalid(
                "event facts boundary is ahead of committed history",
            ));
        }
        let checkpoint = reader.load_checkpoint::<State>(CONSUMER, VERSION, through)?;
        let boundary = checkpoint.through;
        match Self::from_state(
            reader.clone(),
            through,
            boundary,
            checkpoint.state.unwrap_or_default(),
            checkpoint.cold_reason,
        ) {
            Err(error) if boundary > 0 => Self::from_state(
                reader,
                through,
                0,
                State::default(),
                Some(error.to_string()),
            ),
            result => result,
        }
    }

    /// Recompute derived facts only. Original events are read, never executed
    /// or rewritten; an unreadable original still makes this operation fail.
    pub fn rebuild(&self, through: u64, reason: String) -> io::Result<Self> {
        // Never replace a newer published prefix with an older one merely
        // because an early UI replay event discovered a damaged cached page.
        let through = through.max(self.through);
        if through > self.reader.snapshot_end() {
            return Err(invalid(
                "event facts rebuild boundary is ahead of committed history",
            ));
        }
        Self::from_state(
            self.reader.clone(),
            through,
            0,
            State::default(),
            Some(reason),
        )
    }

    fn from_state(
        reader: LogReader,
        through: u64,
        mut boundary: u64,
        state: State,
        mut cold_reason: Option<String>,
    ) -> io::Result<Self> {
        let root = reader
            .path()
            .filter(|path| path.is_dir())
            .map(ToOwned::to_owned);
        let pages = match Pages::open(root.clone(), state.pages, state.count) {
            Ok(pages) if pages.len() as u64 == boundary => pages,
            result => {
                cold_reason = Some(match result {
                    Err(error) => error.to_string(),
                    Ok(_) => "event facts count differs from its prefix".into(),
                });
                boundary = 0;
                Pages::open(root, Vec::new(), 0)?
            }
        };
        let mut facts = Self {
            reader,
            pages,
            through: boundary,
            saved_through: boundary,
            failed: false,
            cold_reason,
            sizes: RefCell::default(),
        };
        facts.catch_up(through)?;
        facts.save()?;
        Ok(facts)
    }

    pub fn cold_reason(&self) -> Option<&str> {
        self.cold_reason.as_deref()
    }
    pub fn through(&self) -> u64 {
        self.through
    }

    fn healthy(&self) -> io::Result<()> {
        if self.failed {
            Err(invalid("event facts require recovery after a failed fold"))
        } else {
            Ok(())
        }
    }

    pub fn catch_up(&mut self, through: u64) -> io::Result<()> {
        self.healthy()?;
        if through < self.through || through > self.reader.snapshot_end() {
            return Err(invalid(
                "event facts catch-up boundary is outside committed tail",
            ));
        }
        let reader = self.reader.clone();
        let result = reader.visit_range(self.through + 1, through, |batch| {
            for event in batch {
                self.fold(event)?;
                self.through = event.seq;
                if self.through - self.saved_through >= CHECKPOINT_EVERY {
                    self.save()?;
                }
            }
            Ok(())
        });
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn fold(&mut self, event: &EventEnvelope) -> io::Result<()> {
        if event.seq != self.through + 1 || event.stream != self.reader.stream() {
            return Err(invalid(
                "event facts are outside their next prefix boundary",
            ));
        }
        let mut fact = Fact {
            source: event.id.clone(),
            size: measure(event),
            model_start: None,
            related_start: None,
            lookup_call: None,
            model_consumed: false,
        };
        if event.event_type == ce::MODEL_CALL_STARTED {
            if let Ok(time) = chrono::DateTime::parse_from_rfc3339(&event.time) {
                fact.model_start = Some(ModelStart {
                    model: event.payload["model"].as_str().unwrap_or_default().into(),
                    millis: time.timestamp_millis(),
                });
            }
        }
        if event.event_type == ce::MODEL_CALL_COMPLETED && event.payload.get("purpose").is_none() {
            // Preserve the original UI pairing: the first not-yet-consumed
            // timestamped start in causes, including a background start if a
            // later foreground completion actually names it.
            for cause in &event.causes {
                let Some(header) = self.reader.header(cause)? else {
                    continue;
                };
                if header.seq == 0 || header.seq > self.through {
                    return Err(invalid(
                        "model completion cause lies beyond its historical prefix",
                    ));
                }
                let index = usize::try_from(header.seq - 1)
                    .map_err(|_| invalid("event ordinal overflow"))?;
                let mut started = self.pages.get(index)?;
                if started.source != *cause {
                    return Err(invalid("model start source identity mismatch"));
                }
                if started.model_start.is_some() && !started.model_consumed {
                    started.model_consumed = true;
                    fact.related_start = Some(cause.clone());
                    self.pages.replace(index, started)?;
                    break;
                }
            }
        }
        if event.event_type == ce::TOOL_EXEC_STARTED {
            fact.lookup_call = event.payload["call"].as_str().map(str::to_owned);
        } else if event.event_type == ce::TOOL_EXEC_COMPLETED {
            let call = event.payload["call"].as_str().unwrap_or_default();
            if let Some(index) = self.pages.find_last(call)? {
                fact.related_start = Some(self.pages.get(index)?.source);
            }
        }
        let index = self.pages.push(fact)?;
        if index as u64 != self.through {
            return Err(invalid("event facts lost sequence alignment"));
        }
        Ok(())
    }

    /// Reuse immutable sizes across model requests while preserving the exact
    /// historical boundary. Absence expires when the recovered prefix grows.
    /// Failed lookups are never cached as absence.
    pub fn size_at(&self, id: &str, through: u64) -> io::Result<Option<(u64, MaterialSize)>> {
        self.healthy()?;
        if through > self.through {
            return Err(invalid("requested facts boundary has not been recovered"));
        }
        if let Some(cached) = self.sizes.borrow().entries.get(id) {
            match cached {
                CachedSize::Present(seq, size) => {
                    return Ok((*seq <= through).then_some((*seq, *size)));
                }
                CachedSize::Absent(boundary) if *boundary == self.through => return Ok(None),
                CachedSize::Absent(_) => {}
            }
        }
        #[cfg(test)]
        {
            self.sizes.borrow_mut().lookups += 1;
        }
        let found = match self.reader.header(id)? {
            // Do not read a future fact's page, or cache it as an absent ID.
            Some(header) if header.seq > through => return Ok(None),
            Some(header) => Some((header.seq, self.fact_at(id, header.seq)?.size)),
            None => None,
        };
        if id.len() <= SIZE_CACHE_KEY_BYTES {
            let mut cache = self.sizes.borrow_mut();
            if cache.entries.len() >= SIZE_CACHE_ENTRIES {
                cache.entries.clear();
            }
            let value = match found {
                Some((seq, size)) => CachedSize::Present(seq, size),
                None => CachedSize::Absent(self.through),
            };
            cache.entries.insert(id.to_owned(), value);
        }
        Ok(found.filter(|(seq, _)| *seq <= through))
    }

    /// Find a fact that was already visible at the requested event boundary.
    /// Unknown or later IDs are absent; damaged source/index pages remain errors.
    pub fn find_at(&self, id: &str, through: u64) -> io::Result<Option<(u64, Fact)>> {
        self.healthy()?;
        if through > self.through {
            return Err(invalid("requested facts boundary has not been recovered"));
        }
        let Some(header) = self.reader.header(id)? else {
            return Ok(None);
        };
        if header.seq > through {
            return Ok(None);
        }
        self.fact_at(id, header.seq)
            .map(|fact| Some((header.seq, fact)))
    }

    pub fn get(&self, id: &str) -> io::Result<Fact> {
        self.healthy()?;
        let header = self
            .reader
            .header(id)?
            .ok_or_else(|| invalid("event facts source is absent"))?;
        self.fact_at(id, header.seq)
    }

    fn fact_at(&self, id: &str, seq: u64) -> io::Result<Fact> {
        if seq == 0 || seq > self.through {
            return Err(invalid(
                "event facts source is outside their recovered prefix",
            ));
        }
        let index = usize::try_from(seq - 1).map_err(|_| invalid("event ordinal overflow"))?;
        let fact = self.pages.get(index)?;
        if fact.source != id {
            return Err(invalid("event facts source identity mismatch"));
        }
        Ok(fact)
    }

    /// Resolve the request selected at this completion's original boundary.
    /// A missing referenced event is a read error, not an unmatched completion.
    pub fn related_event(&self, id: &str) -> io::Result<Option<EventEnvelope>> {
        let fact = self.get(id)?;
        let Some(related) = fact.related_start else {
            return Ok(None);
        };
        let event = self
            .reader
            .get(&related)?
            .ok_or_else(|| invalid("paired request event is absent"))?;
        let completion = self
            .reader
            .header(id)?
            .ok_or_else(|| invalid("completion event is absent"))?;
        if event.seq == 0 || event.seq >= completion.seq {
            return Err(invalid(
                "paired request lies outside the completion's earlier prefix",
            ));
        }
        Ok(Some(event))
    }

    pub fn save(&mut self) -> io::Result<()> {
        self.healthy()?;
        let state = State {
            pages: self.pages.directory()?,
            count: self.pages.len(),
        };
        self.reader
            .save_checkpoint(CONSUMER, VERSION, self.through, &state)?;
        self.saved_through = self.through;
        Ok(())
    }
}

/// Count one content representation, never the model accounting envelope.
/// Native output and normalized fields describe the same reply; prefer native
/// items when present. This remains a byte-weight estimate, not a tokenizer.
pub fn measure(event: &EventEnvelope) -> MaterialSize {
    if event.event_type == ce::MODEL_CALL_COMPLETED {
        let (bytes, thinking) = if let Some(output) = event.payload["responsesOutput"].as_array() {
            output.iter().fold((0, 0), |(reply, thought), item| {
                if item["type"] == "reasoning" {
                    (reply, thought + json_bytes(item))
                } else {
                    (reply + json_bytes(item), thought)
                }
            })
        } else {
            let array_size = |key: &str| {
                event.payload[key]
                    .as_array()
                    .map(|parts| parts.iter().map(json_bytes).sum::<u64>())
                    .unwrap_or(0)
            };
            (
                event.payload["text"]
                    .as_str()
                    .map_or(0, |text| text.len() as u64)
                    + array_size("toolCalls"),
                array_size("reasoning"),
            )
        };
        return MaterialSize {
            kind: MaterialKind::Reply,
            bytes,
            thinking,
        };
    }
    let kind = match event.event_type.as_str() {
        ce::USER_MESSAGE => MaterialKind::User,
        ce::TOOL_EXEC_COMPLETED => MaterialKind::ToolResult,
        ce::TOOL_EXEC_STARTED => MaterialKind::ToolCall,
        ce::WAKE => MaterialKind::Wake,
        _ => MaterialKind::Other,
    };
    MaterialSize {
        kind,
        bytes: json_bytes(&event.payload),
        thinking: 0,
    }
}

/// Compact JSON byte length without allocating encoded strings or containers.
/// Numbers use serde_json's formatter, never a competing float representation.
pub fn json_bytes(value: &Value) -> u64 {
    fn string_bytes(text: &str) -> u64 {
        text.len() as u64
            + 2
            + text
                .bytes()
                .map(|byte| match byte {
                    b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 1,
                    0..=0x1f => 5,
                    _ => 0,
                })
                .sum::<u64>()
    }
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number.to_string().len() as u64,
        Value::String(text) => string_bytes(text),
        Value::Array(items) => {
            2 + items.len().saturating_sub(1) as u64 + items.iter().map(json_bytes).sum::<u64>()
        }
        Value::Object(fields) => {
            2 + fields.len().saturating_sub(1) as u64
                + fields
                    .iter()
                    .map(|(key, value)| string_bytes(key) + 1 + json_bytes(value))
                    .sum::<u64>()
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
