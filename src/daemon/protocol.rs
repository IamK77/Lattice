//! The daemon wire protocol: what flows over the socket, as NDJSON (one JSON
//! per line — the same framing as the cross-process bridge, contract 06).
//!
//! It is the Session's two message families (frontend command / render event)
//! plus a stream id and a handshake. A frontend in ANY language speaks this
//! and nothing else — it never touches Rust.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contracts::event::EventEnvelope;

/// Client → daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    /// Attach to a stream (opening it from `template` if it does not exist).
    /// If `derive_from` names a parent stream, a new stream is opened as a
    /// read-only observer of that parent (a /btw sidechannel). The daemon
    /// replies with `Attached` (a bounded tail page and current controls for
    /// history-pages clients), then streams live updates.
    Attach {
        stream: String,
        #[serde(default)]
        template: Option<String>,
        #[serde(default)]
        derive_from: Option<String>,
        /// What this client can do beyond displaying and sending text —
        /// today "authorize" (it renders authorization cards and answers
        /// them). Self-declared at the handshake so the daemon can say OUT
        /// LOUD when the assembly expects a capability nobody attached with,
        /// instead of letting a question hang silently.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    /// Request the next older page from this attachment's fixed prefix.
    History {
        stream: String,
        cursor: HistoryCursor,
    },
    /// The user typed a line into a stream.
    SendText { stream: String, text: String },
    /// The user answered an authorization request (the trust gate's card):
    /// `request` is the id of the trust.authorization_requested event.
    Authorize {
        stream: String,
        request: String,
        approve: bool,
    },
    /// Controls negotiated by this exact attachment, never an arbitrary interface id.
    SetPermission {
        stream: String,
        attachment: String,
        enabled: bool,
    },
    AuthorizeOperation {
        stream: String,
        attachment: String,
        request: String,
        approve: bool,
        scope: ApprovalScope,
    },
    RevokeGrant {
        stream: String,
        attachment: String,
        grant: String,
    },
    /// Manage experts through the same audited provider and authorization
    /// route as the terminal UI. Results arrive as experts.ui.result events.
    ManageExperts {
        stream: String,
        request: String,
        operation: String,
        arguments: Value,
    },
    /// Stop watching a stream (e.g. the frontend closed its tab). The stream
    /// itself stays open in the daemon; only this client's subscription ends.
    Detach { stream: String },
    /// Interrupt a running turn in a stream — reaches it immediately.
    Interrupt { stream: String },
}

pub const PERMISSIONS_CAPABILITY: &str = "operation-permissions-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    Once,
    Flow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationAttachment {
    pub attachment: String,
    pub interface: Option<String>,
    pub interface_service: Option<String>,
    pub operation_service: Option<String>,
    pub through: u64,
    pub grants: crate::components::operation_policy::GrantState,
    /// Current unresolved questions, including those outside the replay page.
    pub pending_authorizations: Vec<EventEnvelope>,
}

/// A cursor belongs to one attachment, not merely to a stream name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryCursor {
    pub stream: String,
    pub generation: u64,
    pub through: u64,
    pub before: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAuthorization {
    pub request: String,
    pub held: Option<String>,
}

/// Current controls at the fixed attachment boundary. Older history pages
/// are display-only and must never restore these controls a second time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamState {
    pub busy: bool,
    pub waiting: bool,
    pub pending_auth: Vec<PendingAuthorization>,
    pub skills: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentHistory {
    pub through: u64,
    pub older: Option<HistoryCursor>,
    pub state: StreamState,
}

/// Daemon → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMessage {
    /// Attach succeeded; `replay` is at most one tail page. `history` carries
    /// complete-prefix controls and an older-page cursor when negotiated. `warnings`
    /// are capability mismatches said out loud at the handshake (e.g. the
    /// assembly may ask for authorization but this client cannot answer);
    /// the attach still succeeds — observers are legitimate.
    Attached {
        stream: String,
        replay: Vec<EventEnvelope>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        history: Option<AttachmentHistory>,
    },
    /// Sent only when operation-permissions-v1 was negotiated. The old strict
    /// history structure remains unchanged for older clients.
    AttachedV2 {
        stream: String,
        replay: Vec<EventEnvelope>,
        warnings: Vec<String>,
        history: Option<AttachmentHistory>,
        authorization: AuthorizationAttachment,
    },
    /// One older page, tied to the exact request cursor.
    HistoryPage {
        stream: String,
        cursor: HistoryCursor,
        replay: Vec<EventEnvelope>,
        older: Option<HistoryCursor>,
    },
    /// A page failed without advancing its cursor; ordinary errors cannot
    /// accidentally clear a different in-flight history request.
    HistoryError {
        stream: String,
        cursor: HistoryCursor,
        message: String,
    },
    /// A completed event was appended to a stream's ledger (render it).
    Appended {
        stream: String,
        event: Box<EventEnvelope>,
    },
    /// A transient streaming fragment (never recorded).
    Notice {
        stream: String,
        source: String,
        payload: Value,
    },
    /// A turn finished; the frontend may re-enable input.
    Quiescent { stream: String },
    /// Something went wrong for this client. `stream` names the stream the
    /// failure belongs to when known (e.g. a failed attach), so a multi-tab
    /// frontend can route it; None for connection-level noise.
    Error {
        #[serde(default)]
        stream: Option<String>,
        message: String,
    },
}
