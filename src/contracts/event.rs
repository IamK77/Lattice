use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Weak cross-stream reference: "this stream was provoked by that event in
/// that stream". The kernel does not validate it against the other stream —
/// the audit trail can be followed across streams, but the hop is unverified
/// by construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamRef {
    pub stream: String,
    pub event: String,
}

/// Current envelope format version, stamped on every appended event.
/// Readers migrate older versions on load; a newer version is refused.
pub const ENVELOPE_VERSION: u32 = 1;

fn envelope_version_default() -> u32 {
    1
}

/// Event envelope — the outer structure shared by every event.
/// The payload (the "letter") is defined by each event type; the kernel never interprets it.
///
/// The top-level container is the stream: a causally self-contained sequence
/// of events with its own numbering, its own beginning and end, archivable as
/// a whole. "Conversation" and "task" are labels that frontends and the
/// scheduling layer put on streams — the kernel knows neither word.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Envelope format version (see [`ENVELOPE_VERSION`])
    #[serde(default = "envelope_version_default")]
    pub v: u32,
    /// Unique id, assigned by the kernel at append time
    pub id: String,
    /// Stream-local sequence number, assigned at append time, starting at 1
    pub seq: u64,
    /// The stream this event belongs to, stamped by the log
    pub stream: String,
    /// Append timestamp, ISO 8601
    pub time: String,
    /// Namespaced event type, e.g. "core.model.call_completed"
    #[serde(rename = "type")]
    pub event_type: String,
    /// Source: the component instance that produced this event
    pub source: String,
    /// Causality: ids of the events that jointly triggered this one, within
    /// the same stream. Empty for root events (e.g. user input). Usually one;
    /// several when results converge, e.g. two tool outcomes together
    /// provoking one model call (a join).
    pub causes: Vec<String>,
    /// Cross-stream provenance, meaningful on root events: which event in
    /// which other stream provoked this stream's work
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<StreamRef>,
    /// Required for decision-class events: one sentence explaining why
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub payload: Value,
}

/// The part a component provides when emitting; id, seq, stream, time and
/// source are filled in by the kernel
#[derive(Debug, Clone)]
pub struct EventDraft {
    pub event_type: String,
    pub causes: Vec<String>,
    pub origin: Option<StreamRef>,
    pub payload: Value,
    pub reason: Option<String>,
}

impl EventDraft {
    pub fn new(event_type: &str, causes: &[&str], payload: Value) -> Self {
        Self {
            event_type: event_type.to_string(),
            causes: causes.iter().map(|c| c.to_string()).collect(),
            origin: None,
            payload,
            reason: None,
        }
    }

    pub fn with_reason(mut self, reason: &str) -> Self {
        self.reason = Some(reason.to_string());
        self
    }

    pub fn with_origin(mut self, stream: &str, event: &str) -> Self {
        self.origin = Some(StreamRef {
            stream: stream.to_string(),
            event: event.to_string(),
        });
        self
    }
}

/// Event type registration entry — the kernel and each component register their own letter types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTypeDecl {
    /// Namespaced type name
    #[serde(rename = "type")]
    pub event_type: String,
    /// Decision-class events must carry a reason; enforced by the log at its entry point
    #[serde(default)]
    pub decision: bool,
    /// One-line description of what this event means
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for this type's payload (the letter). When present, the
    /// log validates payloads at the entry point — enforcement layer four.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    /// Payload fields that hold a DOCUMENT rather than structure: a system
    /// prompt, a tool schema, a command's output — something a reader reads.
    ///
    /// Past a size, these move to a file beside the ledger and the event keeps
    /// a reference (see `contracts::document`). Which fields those are is a
    /// fact about the event type, so it is declared with the type rather than
    /// worked out by the log, which knows nothing about what a payload means.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<String>,
    /// Whether this type carries CONVERSATION — what the person said, what the
    /// model said — and so has secrets hidden in it on the way in.
    ///
    /// Only conversation. A tool's arguments and a tool's result are not
    /// conversation: they are a machine operation, and they have to be exact.
    /// Hiding a secret in them does not hide anything, it CHANGES the
    /// operation — a read stops returning what is on disk, a write stops
    /// writing what it was told to. That is not a theoretical cost: it
    /// destroyed two API keys in a real catalog on 2026-07-29. The agent read
    /// the file, the read result came back with the key replaced, and writing
    /// the file back put the replacement where the key had been.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

impl EventTypeDecl {
    pub fn new(event_type: &str, description: &str) -> Self {
        Self {
            event_type: event_type.to_string(),
            decision: false,
            description: Some(description.to_string()),
            schema: None,
            documents: Vec::new(),
            redacted: false,
        }
    }

    pub fn decision(event_type: &str, description: &str) -> Self {
        Self {
            event_type: event_type.to_string(),
            decision: true,
            description: Some(description.to_string()),
            schema: None,
            documents: Vec::new(),
            redacted: false,
        }
    }

    pub fn with_schema(mut self, schema: Value) -> Self {
        self.schema = Some(schema);
        self
    }

    /// This type carries conversation: hide known secrets in it (see the
    /// field's own note for what is deliberately NOT covered).
    pub fn redacting(mut self) -> Self {
        self.redacted = true;
        self
    }

    /// Name the payload fields that hold documents (see the field's own note).
    pub fn with_documents(mut self, fields: &[&str]) -> Self {
        self.documents = fields.iter().map(|f| f.to_string()).collect();
        self
    }
}
