use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;

use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl, ENVELOPE_VERSION};

#[path = "log_checkpoint.rs"]
mod checkpoint;
pub(crate) use super::history::Header;
pub use checkpoint::Checkpoint;

/// Identifies one opened history without keeping its writer lock alive.
pub(crate) struct ReaderIdentity(std::sync::Weak<RwLock<super::history::History>>);

impl ReaderIdentity {
    pub(crate) fn matches(&self, reader: &LogReader) -> bool {
        self.0.ptr_eq(&Arc::downgrade(&reader.history))
    }
}

#[cfg(test)]
#[path = "log_storage_tests.rs"]
mod storage_tests;

/// Audit violation: an event rejected at the log's entry point
#[derive(Debug)]
pub enum AuditViolation {
    /// Unregistered event type
    UnregisteredType(String),
    /// A cause points to a nonexistent event
    UnknownCause(String),
    /// Committed metadata could not be read; this is not a missing cause.
    HistoryRead(String),
    /// Decision-class event missing its reason
    MissingReason(String),
    /// Payload does not conform to the type's registered schema
    InvalidPayload { event_type: String, problem: String },
}

impl fmt::Display for AuditViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnregisteredType(t) => write!(f, "unregistered event type: {t}"),
            Self::UnknownCause(id) => write!(f, "cause points to a nonexistent event: {id}"),
            Self::HistoryRead(problem) => write!(f, "cannot validate ledger history: {problem}"),
            Self::MissingReason(t) => write!(f, "decision-class event requires a reason: {t}"),
            Self::InvalidPayload {
                event_type,
                problem,
            } => write!(
                f,
                "payload rejected by the schema of {event_type}: {problem}"
            ),
        }
    }
}

impl std::error::Error for AuditViolation {}

type Handler = Box<dyn FnMut(&EventEnvelope) + Send>;

type SharedHistory = Arc<RwLock<super::history::History>>;
const HISTORY_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// What looking back at the ledger has cost so far.
///
/// Every look-back copies what it asks for, and what it usually asks for is
/// everything. A turn does that a dozen times over (the context gate several
/// times per question, each gate once per tool request, the kernel once per
/// settling), so the cost of one turn is proportional to the whole
/// conversation before it — and the cost of a conversation to the square of
/// its length. That is a claim about shape; this counts it, so a decision to
/// do something about it can rest on a number instead.
///
/// The counting itself is a few additions per look-back, next to a deep copy
/// of every envelope it is measuring.
#[derive(Default, Debug)]
pub struct ReadCost {
    /// How many times anything looked back
    pub reads: AtomicU64,
    /// How many envelopes were copied, summed over those reads
    pub events: AtomicU64,
    /// How many bytes those envelopes hold, as they are written to disk
    pub bytes: AtomicU64,
}

impl ReadCost {
    fn note(&self, events: usize, bytes: u64) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.events.fetch_add(events as u64, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// A line for a person deciding whether this is worth fixing.
    pub fn summary(&self) -> String {
        let reads = self.reads.load(Ordering::Relaxed);
        let events = self.events.load(Ordering::Relaxed);
        let bytes = self.bytes.load(Ordering::Relaxed);
        format!(
            "ledger look-backs: {reads}, envelopes copied: {events}, bytes copied: {:.1} MiB",
            bytes as f64 / (1024.0 * 1024.0)
        )
    }
}

/// The rest of the ledger, as seen from inside a [`LogReader::scan_back`].
///
/// The index is already locked. Loaded bodies have shared ownership and
/// survive cache eviction only while their callers still need them.
#[derive(Clone, Copy)]
pub struct Nearby<'a> {
    history: &'a super::history::History,
}

impl Nearby<'_> {
    pub fn get(&self, id: &str) -> std::io::Result<Option<Arc<EventEnvelope>>> {
        self.history.get(id)
    }

    pub fn has_outcome(&self, started_id: &str) -> std::io::Result<bool> {
        self.history.has_outcome(started_id)
    }
}

/// Strings that must never be written to the ledger.
///
/// The ledger is permanent and, worse for a secret, it is re-sent: every part
/// of it travels to the model again on the following turn. So a key that gets
/// onto it once is on it forever and leaves for a provider repeatedly. The
/// way it gets there is ordinary — the agent reads a config file, runs `env`,
/// prints a stack trace — and none of those are mistakes anyone would think
/// to prevent.
///
/// Values, not field names. A key does not only arrive under a key-shaped
/// name: it arrives inside a tool result, in a command's output, quoted in
/// an error. Matching what the secret IS catches those; matching where it
/// usually sits does not.
///
/// The kernel is told the strings and nothing else — not what they mean, not
/// where they came from. Which strings are secret is the assembler's
/// knowledge, exactly as with the variables withheld from subprocesses.
#[derive(Default, Debug)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    /// Very short strings are refused: they would match everywhere and turn
    /// the ledger into `[redacted]`. A real key is long.
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            secrets: secrets.into_iter().filter(|s| s.len() >= 12).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Replace every occurrence, anywhere in the value — string leaves at any
    /// depth, and object KEYS too (a secret can end up as a map key when
    /// something indexes by it).
    pub fn apply(&self, value: &mut Value) {
        if self.secrets.is_empty() {
            return;
        }
        match value {
            Value::String(text) => {
                for secret in &self.secrets {
                    if text.contains(secret.as_str()) {
                        *text = text.replace(secret.as_str(), REDACTED);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.apply(item)),
            Value::Object(fields) => {
                let renamed: Vec<String> = fields
                    .keys()
                    .filter(|k| self.secrets.iter().any(|s| k.contains(s.as_str())))
                    .cloned()
                    .collect();
                for key in renamed {
                    let mut clean = key.clone();
                    for secret in &self.secrets {
                        clean = clean.replace(secret.as_str(), REDACTED);
                    }
                    if let Some(v) = fields.remove(&key) {
                        fields.insert(clean, v);
                    }
                }
                fields.values_mut().for_each(|v| self.apply(v));
            }
            _ => {}
        }
    }

    /// The same for a plain string (a reason line).
    pub fn apply_text(&self, text: &mut String) {
        for secret in &self.secrets {
            if text.contains(secret.as_str()) {
                *text = text.replace(secret.as_str(), REDACTED);
            }
        }
    }
}

/// Read-only handle onto one stream's ledger. Cloneable and thread-safe:
/// components use it to dereference material pointers and to look back at
/// history ("按编号回查流水原文"). Reading never blocks appends for long —
/// reads take snapshots (clones) of what they ask for.
#[derive(Clone, Debug)]
pub struct LogReader {
    stream: String,
    path: Option<PathBuf>,
    history: SharedHistory,
    pending: Arc<Mutex<checkpoint::PendingCache>>,
    cost: Arc<ReadCost>,
}

#[path = "log_reader.rs"]
mod reader;

/// Event log — kernel duty #1.
/// The only way in is `append`, which enforces the law at the entry point:
/// the type must be registered; every cause must point to a real event;
/// decision-class events must carry a reason. Then it assigns the sequence
/// number, stamps the time, persists (JSONL append + per-event fsync) and
/// notifies subscribers.
///
/// Invariant: delivery never precedes recording (the event is fsynced before
/// `append` returns). Group commit can become a performance knob later; a
/// failed write crashes the kernel — fail-stop, the log is the source of
/// truth.
pub struct EventLog {
    /// The stream this log is the ledger of — one log, one stream
    stream: String,
    path: Option<PathBuf>,
    history: SharedHistory,
    pending: Arc<Mutex<checkpoint::PendingCache>>,
    types: HashMap<String, EventTypeDecl>,
    /// Compiled payload validators — enforcement layer four (the letter)
    validators: HashMap<String, jsonschema::Validator>,
    handlers: Vec<(u64, Handler)>,
    next_handler_id: u64,
    /// Byte position of the next append in the sole backing writer.
    writer_position: u64,
    /// Applied to every event on its way in (see [`Redactor`])
    redactor: Redactor,
    /// What looking back at this ledger has cost (see [`ReadCost`])
    cost: Arc<ReadCost>,
    /// Kept open for the log's lifetime; one open at startup, not one per append
    writer: Option<File>,
    segmented: Option<Arc<Mutex<super::segmented::Ledger>>>,
    /// Where this ledger's documents go, and which ones are already there.
    ///
    /// Only a ledger with a file has them: a document is a file a reader can
    /// open, and an in-memory ledger has nowhere to put one. The map is
    /// content digest -> file name, so the same prompt sent on ninety turns is
    /// written once and referenced ninety times.
    documents: Option<Documents>,
}

impl Drop for EventLog {
    fn drop(&mut self) {
        // Historical readers may outlive the runtime. They keep its immutable
        // prefix readable, not permission to prevent a new writer from opening.
        if let Some(ledger) = &self.segmented {
            ledger
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .release_writer();
        }
    }
}

pub(crate) use super::documents::Documents;

/// Move a payload's documents out to files, leaving references. Answers with
/// how many moved.
///
/// Which fields are documents is declared with the event type — nothing here
/// knows what `system` or `result` mean. Only fields past the threshold move;
/// small ones stay where a reader expects them.
///
/// A write that fails leaves the field inline. The ledger is the audit record
/// and losing content to a full disk would be far worse than a long line; what
/// is at stake here is only how pleasant the record is to read.
pub(crate) fn move_documents(
    payload: &mut Value,
    fields: &[String],
    seq: u64,
    store: &mut Documents,
) -> usize {
    let Some(object) = payload.as_object_mut() else {
        return 0;
    };
    let mut moved = 0;
    for field in fields {
        let Some(value) = object.get(field) else {
            continue;
        };
        // A string is prose and keeps its own newlines; anything else is
        // structure and is written expanded, so that it too has lines to
        // search — a tool list is 1 line minified and 594 expanded.
        let (text, extension) = match value {
            Value::String(text) => (text.clone(), "txt"),
            Value::Null => continue,
            other => (
                serde_json::to_string_pretty(other).unwrap_or_default(),
                "json",
            ),
        };
        if text.len() < DOCUMENT_THRESHOLD {
            continue;
        }
        let name = format!("ev_{seq}-{field}.{extension}");
        let Ok(file) = store.put(&name, &text) else {
            continue;
        };
        let preview: String = text.chars().take(PREVIEW_CHARS).collect();
        let mut reference = serde_json::Map::new();
        reference.insert("file".to_string(), Value::String(file));
        reference.insert("bytes".to_string(), Value::from(text.len()));
        reference.insert(
            "lines".to_string(),
            Value::from(text.lines().count().max(1)),
        );
        reference.insert("preview".to_string(), Value::String(preview));
        object.insert(field.clone(), Value::Object(reference));
        moved += 1;
    }
    moved
}

/// What a redacted secret is replaced with, everywhere.
///
/// One constant because two places need to agree on it: the redactor writes
/// it, and the model catalog refuses to treat it as a key. That second reader
/// exists because this string has a way of ending up where a key belongs —
/// the agent reads a file holding one, sees this instead, and writing back
/// what it read overwrites the key with the placeholder.
pub const REDACTED: &str = "[redacted]";

/// A payload field big enough to be worth a file of its own.
///
/// Below this it stays inline: a document costs a file and a jump to read, and
/// neither is worth paying for a few hundred bytes.
const DOCUMENT_THRESHOLD: usize = 4096;

/// How much of a document the reference shows without opening it.
const PREVIEW_CHARS: usize = 200;

impl EventLog {
    /// Open the ledger of one stream; if the file exists, load its events (JSONL)
    pub fn open(
        types: Vec<EventTypeDecl>,
        stream: impl Into<String>,
        file: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let segmented = file.as_ref().is_some_and(|path| {
            path.is_dir() || path.extension().is_some_and(|ext| ext == "ledger")
        });
        Self::open_storage(
            types,
            stream.into(),
            file,
            segmented.then_some(64 * 1024 * 1024),
            false,
        )
    }

    /// Exclusively create a segmented ledger and retain its writer lease.
    /// Only a nonempty `.ledger` path is supported; occupied paths are never
    /// opened, repaired, or adopted. Later failures may leave creation evidence.
    pub fn create_new(
        types: Vec<EventTypeDecl>,
        stream: impl Into<String>,
        path: PathBuf,
    ) -> std::io::Result<Self> {
        if path.as_os_str().is_empty() || path.extension().is_none_or(|ext| ext != "ledger") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "exclusive creation requires a nonempty .ledger path: {}",
                    path.display()
                ),
            ));
        }
        Self::open_storage(
            types,
            stream.into(),
            Some(path.clone()),
            Some(64 * 1024 * 1024),
            true,
        )
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "could not create new ledger {}: {error}; partial initialization may remain",
                    path.display()
                ),
            )
        })
    }

    /// Reserve a fresh, empty logical ledger. An existing path is never
    /// adopted, even when it contains an otherwise valid ledger.
    pub fn initialize_segmented(path: &std::path::Path, stream: &str) -> std::io::Result<()> {
        super::segmented::Ledger::create(path, stream, 64 * 1024 * 1024).map(|_| ())
    }

    /// Open a segmented ledger with a soft per-segment byte limit.
    pub fn open_segmented(
        types: Vec<EventTypeDecl>,
        stream: impl Into<String>,
        path: PathBuf,
        segment_bytes: u64,
    ) -> std::io::Result<Self> {
        Self::open_storage(types, stream.into(), Some(path), Some(segment_bytes), false)
    }

    fn open_storage(
        types: Vec<EventTypeDecl>,
        stream: String,
        file: Option<PathBuf>,
        segment_bytes: Option<u64>,
        create_new: bool,
    ) -> std::io::Result<Self> {
        let mut validators = HashMap::new();
        for decl in &types {
            if let Some(schema) = &decl.schema {
                let validator = jsonschema::validator_for(schema).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid payload schema for {}: {e}", decl.event_type),
                    )
                })?;
                validators.insert(decl.event_type.clone(), validator);
            }
        }
        let segmented = match (file.as_ref(), segment_bytes) {
            (Some(path), Some(limit)) => {
                let ledger = if create_new {
                    super::segmented::Ledger::create(path, &stream, limit)?
                } else if path.exists() {
                    super::segmented::Ledger::open_for(path, limit, Some(&stream))?
                } else {
                    super::segmented::Ledger::create(path, &stream, limit)?
                };
                if ledger.recovery.discarded_tail_bytes > 0 {
                    eprintln!(
                        "warning: {} discarded {} incomplete tail bytes",
                        path.display(),
                        ledger.recovery.discarded_tail_bytes
                    );
                }
                if ledger.recovery.rebuilt_indexes > 0 {
                    eprintln!("warning: {} rebuilt {} sealed indexes from verified source ({} body bytes)",
                        path.display(), ledger.recovery.rebuilt_indexes, ledger.open_stats.sealed_body_bytes);
                }
                Some(Arc::new(Mutex::new(ledger)))
            }
            _ => None,
        };
        let backing = file
            .as_ref()
            .filter(|_| segmented.is_none())
            .map(|path| {
                super::migrate::refuse_original_write(path)?;
                OpenOptions::new()
                    .read(true)
                    .append(true)
                    .create(true)
                    .open(path)
            })
            .transpose()?;
        // Every handle refers to this one open, even if the path is replaced.
        // Body reads use positional I/O and do not move the writer's cursor.
        let mut writer = backing.as_ref().map(File::try_clone).transpose()?;
        let (mut history, segmented) = match segmented {
            Some(ledger) => (
                super::history::History::segmented(Arc::clone(&ledger), HISTORY_CACHE_BYTES)?,
                Some(ledger),
            ),
            None => (
                super::history::History::new(backing, HISTORY_CACHE_BYTES),
                None,
            ),
        };
        let mut writer_position = 0;
        let mut documents = file
            .as_ref()
            .filter(|_| segmented.is_some())
            .map(|path| Documents::beside(path));
        // Byte length of the good prefix, when the file ends mid-write.
        let mut truncated: Option<u64> = None;
        if let (Some(path), Some(output)) = (&file, writer.as_mut()) {
            // Validate one line at a time, preserving byte offsets and
            // torn UTF-8 tail recovery without retaining the entire file.
            let mut input = BufReader::new(output.try_clone()?);
            let mut bytes = Vec::new();
            let mut ended_with_newline = true;
            let mut start = 0u64;
            loop {
                bytes.clear();
                let count = input.read_until(b'\n', &mut bytes)?;
                if count == 0 {
                    break;
                }
                let terminated = bytes.last() == Some(&b'\n');
                ended_with_newline = terminated;
                let raw = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
                let line_bytes = raw.strip_suffix(b"\r").unwrap_or(raw);
                if line_bytes.iter().all(u8::is_ascii_whitespace) {
                    start += count as u64;
                    continue;
                }
                let line = match std::str::from_utf8(line_bytes) {
                    Ok(line) => line,
                    Err(e) if !terminated && e.error_len().is_none() => {
                        eprintln!(
                            "warning: {} ends in an incomplete UTF-8 event ({e}); \
                                 it was dropped — a write that never finished",
                            path.display()
                        );
                        truncated = Some(start);
                        break;
                    }
                    Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                };
                let event: EventEnvelope = match serde_json::from_str(line) {
                    Ok(event) => event,
                    // Only an unterminated final line can be a torn write.
                    // A newline commits the record boundary; damage before
                    // one must be surfaced rather than guessed away.
                    Err(e) if !terminated && super::record::incomplete(line_bytes, &e) => {
                        eprintln!(
                            "warning: {} ends in an incomplete event ({e}); \
                                 it was dropped — a write that never finished",
                            path.display()
                        );
                        truncated = Some(start);
                        break;
                    }
                    Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                };
                if event.v > ENVELOPE_VERSION {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "ledger written by a newer envelope version ({} > {})",
                            event.v, ENVELOPE_VERSION
                        ),
                    ));
                }
                if event.stream != stream {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "event of stream {} found in the ledger of stream {} — \
                                 a ledger belongs to the stream that wrote it, so \
                                 reopening one means opening it under ITS id (see \
                                 `EventLog::stream_of`)",
                            event.stream, stream
                        ),
                    ));
                }
                let expected = history.len() as u64 + 1;
                if event.seq != expected || history.position(&event.id)?.is_some() {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                            format!("invalid ledger identity or sequence at byte {start}: expected sequence {expected}, got {} ({})", event.seq, event.id)));
                }
                history.push(&event, start, line_bytes)?;
                start += count as u64;
            }
            // A fully serialized final event is committed data even when
            // its delimiter was the last byte lost. Repair the delimiter
            // before append mode can glue the next event to it.
            if truncated.is_none() && start > 0 && !ended_with_newline {
                output.write_all(b"\n")?;
                output.sync_all()?;
            }
            // Cut the half-written tail off before appending, or the next
            // event lands glued to it and the damage becomes permanent.
            if let Some(good) = truncated {
                output.set_len(good)?;
                output.sync_all()?;
            }
            writer_position = output.metadata()?.len();
            documents = Some(Documents::beside(path));
        }
        Ok(Self {
            stream,
            path: file.map(|path| std::path::absolute(&path).unwrap_or(path)),
            history: Arc::new(RwLock::new(history)),
            types: types
                .into_iter()
                .map(|t| (t.event_type.clone(), t))
                .collect(),
            validators,
            handlers: Vec::new(),
            next_handler_id: 0,
            writer_position,
            redactor: Redactor::default(),
            cost: Arc::default(),
            pending: Arc::default(),
            writer,
            segmented,
            documents,
        })
    }

    /// Whose stream a ledger file already belongs to, read from its first
    /// event — `None` for a file that is missing, empty or unreadable.
    ///
    /// A stream id is stamped on every event and checked on the way back in,
    /// so reopening a ledger means opening it under the id it was written
    /// with. Anyone continuing a conversation has to ask this first; a fresh
    /// id would be refused by the check above, and correctly — the ledger IS
    /// the stream, and two ids in one file would make its numbering a lie.
    pub fn stream_of(path: &std::path::Path) -> Option<String> {
        if path.is_dir() {
            return super::segmented::Ledger::stream_of(path).ok();
        }
        let file = File::open(path).ok()?;
        let first = BufReader::new(file).lines().next()?.ok()?;
        let event: EventEnvelope = serde_json::from_str(&first).ok()?;
        Some(event.stream)
    }

    /// Original JSONL files in logical order, from a read-only catalog snapshot.
    /// This never repairs files, builds caches, or acquires a writer. It does
    /// not verify source seals; callers needing that use the verification API.
    pub fn source_paths(path: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
        if path.is_dir() {
            super::segmented::Ledger::source_paths(path)
        } else if path.metadata()?.is_file() {
            Ok(vec![path.to_owned()])
        } else {
            Err(std::io::Error::other(
                "ledger is not a file or segmented directory",
            ))
        }
    }

    /// Memory only (for tests)
    pub fn in_memory(types: Vec<EventTypeDecl>, stream: impl Into<String>) -> Self {
        Self::open(types, stream, None).expect("in-memory log cannot fail on IO")
    }

    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// Verify an offline segmented ledger without repairing or starting it.
    pub fn verify_segmented_path(path: &std::path::Path) -> std::io::Result<()> {
        super::segmented::Ledger::verify_path(path)
    }

    /// Explicitly verify every segment of an open segmented ledger, including
    /// the active prefix. This performs no repair or cache rewrite.
    pub fn verify_segments(&self) -> std::io::Result<()> {
        let ledger = self.segmented.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "this ledger does not use segmented storage",
            )
        })?;
        ledger
            .lock()
            .map_err(|_| std::io::Error::other("segmented ledger lock poisoned"))?
            .verify()
    }

    /// Register additional letter types after opening (hot install).
    /// Conflicts are inspection's job; this only compiles the validators.
    pub fn register_types(&mut self, types: &[EventTypeDecl]) -> std::io::Result<()> {
        for decl in types {
            if let Some(schema) = &decl.schema {
                let validator = jsonschema::validator_for(schema).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid payload schema for {}: {e}", decl.event_type),
                    )
                })?;
                self.validators.insert(decl.event_type.clone(), validator);
            }
            self.types.insert(decl.event_type.clone(), decl.clone());
        }
        Ok(())
    }

    /// A read-only, shareable handle onto this ledger
    pub fn reader(&self) -> LogReader {
        LogReader {
            stream: self.stream.clone(),
            path: self.path.clone(),
            history: Arc::clone(&self.history),
            pending: Arc::clone(&self.pending),
            cost: Arc::clone(&self.cost),
        }
    }

    /// Move this payload's documents out to files, leaving references.
    ///
    /// Which fields are documents is declared with the event type — the log
    /// knows nothing about what `system` or `result` mean. Only fields past
    /// the threshold move; small ones stay where a reader expects them.
    ///
    /// A ledger with no file does nothing here. A document is a file someone
    /// opens, and an in-memory ledger has nowhere to put one — so the shapes
    /// differ between a stored ledger and a transient one, which is right:
    /// this is a storage decision, not a change to what an event means.
    ///
    /// A write that fails leaves the field inline. The ledger is the audit
    /// record and losing content to a full disk would be worse than a long
    /// line; the only thing at stake here is how pleasant it is to read.
    fn extract_documents(&mut self, payload: &mut Value, fields: &[String], seq: u64) {
        if let Some(documents) = &mut self.documents {
            move_documents(payload, fields, seq, documents);
        }
    }

    /// Entry-point enforcement + persistence. Returns the completed envelope.
    pub fn append(
        &mut self,
        mut draft: EventDraft,
        source: &str,
    ) -> Result<EventEnvelope, AuditViolation> {
        let decl = self
            .types
            .get(&draft.event_type)
            .ok_or_else(|| AuditViolation::UnregisteredType(draft.event_type.clone()))?;
        // Conversation only, and only if the type says it is conversation.
        //
        // Before anything else looks at it, and well before it is written or
        // handed to a subscriber: what is redacted here was never on the
        // ledger at all, rather than being on it and hidden afterwards.
        // Validation therefore judges what will actually be stored.
        //
        // A tool's arguments and a tool's result are deliberately NOT covered
        // (see `EventTypeDecl::redacted`). The kernel appends before it
        // routes, so anything changed here is also what the component
        // receives — hiding a secret in a tool request does not hide it, it
        // changes the operation. A read stops returning what is on disk and a
        // write stops writing what it was told to write.
        if decl.redacted && !self.redactor.is_empty() {
            self.redactor.apply(&mut draft.payload);
            if let Some(reason) = &mut draft.reason {
                self.redactor.apply_text(reason);
            }
        }
        let decl = &self.types[&draft.event_type];
        // After redaction, so a secret never reaches a document file — those
        // are outside the redactor's reach once written. Before validation,
        // so what the schema judges is what will actually be stored.
        let seq = self.len() as u64 + 1;
        let documents = decl.documents.clone();
        self.extract_documents(&mut draft.payload, &documents, seq);
        let decl = &self.types[&draft.event_type];
        {
            let history = self.history.read().expect("log history lock poisoned");
            for cause in &draft.causes {
                if history
                    .position(cause)
                    .map_err(|error| AuditViolation::HistoryRead(error.to_string()))?
                    .is_none()
                {
                    return Err(AuditViolation::UnknownCause(cause.clone()));
                }
            }
        }
        if decl.decision && draft.reason.as_deref().is_none_or(|r| r.trim().is_empty()) {
            return Err(AuditViolation::MissingReason(draft.event_type.clone()));
        }
        if let Some(validator) = self.validators.get(&draft.event_type) {
            if let Some(error) = validator.iter_errors(&draft.payload).next() {
                return Err(AuditViolation::InvalidPayload {
                    event_type: draft.event_type.clone(),
                    problem: error.to_string(),
                });
            }
        }

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before 1970")
            .subsec_nanos();
        let event = EventEnvelope {
            v: ENVELOPE_VERSION,
            id: format!("ev_{seq}_{nanos:08x}"),
            seq,
            stream: self.stream.clone(),
            time: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            event_type: draft.event_type,
            source: source.to_string(),
            causes: draft.causes,
            origin: draft.origin,
            reason: draft.reason,
            payload: draft.payload,
        };

        // Serialized once, whether or not there is a file: the length is what
        // tells a look-back how much it copied, and computing it later would
        // cost more than the thing being measured.
        let line = serde_json::to_string(&event).expect("envelope serialization cannot fail");
        if let Some(ledger) = &self.segmented {
            let mut ledger = ledger.lock().map_err(|_| {
                AuditViolation::HistoryRead("segmented ledger lock poisoned".into())
            })?;
            ledger
                .validate_append(&event)
                .map_err(|error| AuditViolation::HistoryRead(error.to_string()))?;
            ledger
                .append_validated_line(&event, line.as_bytes())
                .expect("durable segmented ledger append failed");
        }
        if let Some(writer) = &mut self.writer {
            // External writers are unsupported. Refuse a displaced append
            // before publishing an event with an incorrect body location.
            let expected_end = self.writer_position + line.len() as u64 + 1;
            let commit = (|| -> std::io::Result<()> {
                if writer.metadata()?.len() != self.writer_position {
                    return Err(std::io::Error::other(
                        "ledger length changed outside this writer",
                    ));
                }
                writeln!(writer, "{line}")?;
                // Durability before delivery: fsync per event.
                writer.sync_data()?;
                if writer.metadata()?.len() != expected_end {
                    return Err(std::io::Error::other("ledger length changed during append"));
                }
                Ok(())
            })();
            commit.expect("failed to commit the log at its indexed position");
        }

        self.history
            .write()
            .expect("log history lock poisoned")
            .push(&event, self.writer_position, line.as_bytes())
            .expect("failed to index the committed event");
        self.writer_position += line.len() as u64 + 1;
        for (_, handler) in &mut self.handlers {
            handler(&event);
        }
        Ok(event)
    }

    pub fn get(&self, id: &str) -> std::io::Result<Option<EventEnvelope>> {
        self.reader().get(id)
    }

    /// Explicitly materialize a snapshot; prefer narrow reads for bookkeeping.
    pub fn replay(&self, from_seq: u64) -> std::io::Result<Vec<EventEnvelope>> {
        self.reader().replay(from_seq)
    }

    pub fn contains_id(&self, id: &str) -> std::io::Result<bool> {
        self.reader().contains_id(id)
    }

    pub(super) fn header(&self, id: &str) -> std::io::Result<Option<super::history::Header>> {
        self.reader().header(id)
    }

    pub fn has_outcome(&self, started_id: &str) -> std::io::Result<bool> {
        self.history
            .read()
            .expect("log history lock poisoned")
            .has_outcome(started_id)
    }

    pub(super) fn hanging(&self, started_type: &str) -> std::io::Result<Vec<String>> {
        self.reader().pending_heads(started_type)
    }

    /// What looking back at this ledger has cost so far (see [`ReadCost`]).
    pub fn cost(&self) -> &ReadCost {
        &self.cost
    }

    /// Strings this ledger must never record (see [`Redactor`]). Set before
    /// the first append; the ledger cannot clean up what it already wrote.
    pub fn redact(&mut self, redactor: Redactor) {
        self.redactor = redactor;
    }

    // The narrow look-backs, forwarded so that a holder of the log asks the
    // same way a holder of a reader does. See [`LogReader::scan_back`] for
    // the one rule the closures must keep.

    pub fn scan_back<T>(
        &self,
        pick: impl FnMut(&EventEnvelope, Nearby<'_>) -> std::io::Result<Option<T>>,
    ) -> std::io::Result<Option<T>> {
        self.reader().scan_back(pick)
    }

    pub fn find_back(
        &self,
        pick: impl Fn(&EventEnvelope) -> bool,
    ) -> std::io::Result<Option<EventEnvelope>> {
        self.reader().find_back(pick)
    }

    pub fn any(&self, pred: impl Fn(&EventEnvelope) -> bool) -> std::io::Result<bool> {
        self.reader().any(pred)
    }

    pub fn collect_where(
        &self,
        keep: impl Fn(&EventEnvelope) -> bool,
    ) -> std::io::Result<Vec<EventEnvelope>> {
        self.reader().collect_where(keep)
    }

    /// Subscribe to events appended after this point; returns a subscription id
    pub fn subscribe(&mut self, handler: impl FnMut(&EventEnvelope) + Send + 'static) -> u64 {
        let id = self.next_handler_id;
        self.next_handler_id += 1;
        self.handlers.push((id, Box::new(handler)));
        id
    }

    pub fn unsubscribe(&mut self, subscription: u64) {
        self.handlers.retain(|(id, _)| *id != subscription);
    }

    /// Audit primitive: collect every causal ancestor of an event.
    /// With multi-cause events the ancestry is a graph, not a chain: this
    /// walks it breadth-first, visits each ancestor once, and returns the
    /// result sorted by descending seq — which is a topological order, since
    /// a cause always precedes its effect in the stream.
    pub fn trace_back(&self, id: &str) -> std::io::Result<Vec<EventEnvelope>> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut ancestors = Vec::new();
        queue.push_back(id.to_string());
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(event) = self.get(&current)? {
                for cause in &event.causes {
                    queue.push_back(cause.clone());
                }
                ancestors.push(event);
            }
        }
        ancestors.sort_by_key(|e| std::cmp::Reverse(e.seq));
        Ok(ancestors)
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
