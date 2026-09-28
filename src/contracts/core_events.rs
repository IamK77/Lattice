//! Core event types (four families: input, model, tool, control).
//! The log records completed states only: streaming fragments are not events —
//! they go through a non-persisted live-notification bypass.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::event::{EventEnvelope, EventTypeDecl};

// ── Type names ──────────────────────────────────────────

pub const USER_MESSAGE: &str = "core.input.user_message";
pub const EXTERNAL_INPUT: &str = "core.input.external";
/// A wake source injected input: a background task finished, a timer fired, a
/// monitor tripped. Same input family as a user message — the loop treats it
/// as one, adapters render it as a user-like turn; `source` names who woke it.
pub const WAKE: &str = "core.input.wake";
pub const MODEL_CALL_STARTED: &str = "core.model.call_started";
pub const MODEL_CALL_COMPLETED: &str = "core.model.call_completed";
pub const TOOL_EXEC_STARTED: &str = "core.tool.exec_started";
pub const TOOL_EXEC_COMPLETED: &str = "core.tool.exec_completed";
pub const INTERRUPTED: &str = "core.control.interrupted";
pub const ERROR: &str = "core.control.error";
pub const OUTPUT_REPLY: &str = "core.output.reply";
pub const TURN_STARTED: &str = "core.control.turn_started";
pub const TURN_COMPLETED: &str = "core.control.turn_completed";
pub const COMPONENT_CRASHED: &str = "core.control.component_crashed";
/// The first event of every stream: what this conversation is, recorded so
/// that reading it does not have to start by inferring it.
///
/// A file header would have been the obvious place, and is wrong here: an
/// event id carries its own line number (`ev_42_…` is line 42), which is the
/// address the agent is given for reaching one event. A header line would put
/// every event one line off its own id.
pub const STREAM_OPENED: &str = "core.stream.opened";
/// A later process picked this stream up again — `eva -c`, a daemon restart.
pub const STREAM_RESUMED: &str = "core.stream.resumed";
pub const COMPONENT_INSTALLED: &str = "core.control.component_installed";
/// A component taken back out. Installing was a one-way door until this
/// existed: a bad install could only be undone by hand-editing the overlay
/// file, and a component that crashed on every start came back on every start.
pub const COMPONENT_REMOVED: &str = "core.control.component_removed";
/// One instance's implementation swapped, every wire it sits on left alone.
///
/// Distinct from a removal followed by an install because the INSTANCE
/// survives: the name stays, the wiring stays, and whatever was addressed to
/// it goes on being addressed to it. That is what changing the model is —
/// the same seat in the assembly, a different occupant.
pub const COMPONENT_REPLACED: &str = "core.control.component_replaced";

/// Parse one of the canon schema files (schemas/payloads/*.json).
/// The canon is language-neutral JSON; this is merely its Rust loading point.
fn canon(source: &str) -> Value {
    serde_json::from_str(source).expect("canon schema files are valid JSON")
}

pub fn core_event_decls() -> Vec<EventTypeDecl> {
    let schema = |name: &str, source: &str, description: &str| {
        EventTypeDecl::new(name, description).with_schema(canon(source))
    };
    vec![
        schema(
            USER_MESSAGE,
            include_str!("../../schemas/payloads/user_message.json"),
            "The user said something",
        )
        .redacting(),
        schema(
            EXTERNAL_INPUT,
            include_str!("../../schemas/payloads/external.json"),
            "An external system injected input",
        )
        .redacting(),
        schema(
            WAKE,
            include_str!("../../schemas/payloads/wake.json"),
            "A wake source injected input (background task done, timer, monitor)",
        ),
        schema(
            MODEL_CALL_STARTED,
            include_str!("../../schemas/payloads/model_call_started.json"),
            "A model call started (includes the full material sent to the model)",
        )
        // The two largest things on any ledger, and both are read rather than
        // used: measured across 89 real records, `tools` is 13.7 MB and
        // `system` 5.9 MB of 25.5 MB total. `input` stays inline — a list of
        // event ids is structure, and the reader wants it in place.
        .with_documents(&["system", "tools"]),
        schema(
            MODEL_CALL_COMPLETED,
            include_str!("../../schemas/payloads/model_call_completed.json"),
            "A model call completed (full reply or error)",
        )
        .redacting(),
        schema(
            TOOL_EXEC_STARTED,
            include_str!("../../schemas/payloads/tool_exec_started.json"),
            "A tool execution started",
        ),
        schema(
            TOOL_EXEC_COMPLETED,
            include_str!("../../schemas/payloads/tool_exec_completed.json"),
            "A tool execution completed (result or error)",
        ),
        schema(
            OUTPUT_REPLY,
            include_str!("../../schemas/payloads/output_reply.json"),
            "The agent's outward reply (what frontends display)",
        )
        .redacting(),
        schema(
            INTERRUPTED,
            include_str!("../../schemas/payloads/interrupted.json"),
            "Interrupted",
        ),
        schema(
            ERROR,
            include_str!("../../schemas/payloads/control_error.json"),
            "An error no component caught",
        ),
        schema(
            TURN_STARTED,
            include_str!("../../schemas/payloads/turn_boundary.json"),
            "A turn started",
        ),
        schema(
            TURN_COMPLETED,
            include_str!("../../schemas/payloads/turn_boundary.json"),
            "A turn completed",
        ),
        schema(
            STREAM_OPENED,
            include_str!("../../schemas/payloads/stream_opened.json"),
            "This stream was opened: what runtime, assembly and host it began under",
        ),
        schema(
            STREAM_RESUMED,
            include_str!("../../schemas/payloads/stream_resumed.json"),
            "A later process picked this stream up again",
        ),
        schema(
            COMPONENT_CRASHED,
            include_str!("../../schemas/payloads/component_crashed.json"),
            "A component crashed (recorded by the kernel on its behalf)",
        ),
        EventTypeDecl::decision(
            COMPONENT_INSTALLED,
            "A component was installed into the running assembly",
        )
        .with_schema(canon(include_str!(
            "../../schemas/payloads/component_installed.json"
        ))),
        EventTypeDecl::decision(
            COMPONENT_REMOVED,
            "A component was taken out of the assembly",
        )
        .with_schema(canon(include_str!(
            "../../schemas/payloads/component_removed.json"
        ))),
        EventTypeDecl::decision(
            COMPONENT_REPLACED,
            "One instance's implementation was swapped, its wiring left alone",
        )
        .with_schema(canon(include_str!(
            "../../schemas/payloads/component_replaced.json"
        ))),
    ]
}

// ── Endings ─────────────────────────────────────────────

/// Whether this event type ENDS a call.
///
/// A call has two possible endings, never one: its own completion, or an
/// interruption saying no completion is coming. Both are outcomes. Asking
/// only about completions reads a settled call as still open — and the
/// symptom depends entirely on which way the question was asked. Code that
/// asks "was it answered?" concludes no and does the work again; code that
/// asks "is it still running?" concludes yes and never starts another. The
/// workshop had the first form and re-installed, across a restart, something
/// nobody had approved; the context gate had the second and stopped
/// condensing that stream forever. One definition, so neither can recur.
pub fn is_outcome(event_type: &str) -> bool {
    matches!(
        event_type,
        TOOL_EXEC_COMPLETED | MODEL_CALL_COMPLETED | INTERRUPTED
    )
}

/// Whether THIS event is an ending of the call `started_id` began.
///
/// One event at a time, so that the same rule serves both ways of looking:
/// a caller holding a slice of the ledger, and a caller scanning it under
/// the read lock without copying it.
pub fn ends_call(event: &EventEnvelope, started_id: &str) -> bool {
    relation_ends_call(
        EventRelations {
            id: &event.id,
            event_type: &event.event_type,
            causes: &event.causes,
        },
        started_id,
    )
}

/// The same outcome rule applied to a validated header, without loading a body.
pub fn relation_ends_call(event: EventRelations<'_>, started_id: &str) -> bool {
    is_outcome(event.event_type) && event.causes.iter().any(|c| c == started_id)
}

/// Whether some event in `events` already ended the call `started_id` began.
pub fn has_outcome(events: &[EventEnvelope], started_id: &str) -> bool {
    events.iter().any(|later| ends_call(later, started_id))
}

/// The head of every chain that was still in flight when the process ended.
///
/// A request can sit on the ledger more than once. A gate that forwards it
/// appends its own copy, and the outcome answers the COPY — so asking "does
/// anything end this exact event?" reads every relayed call as hanging. In
/// the product assembly a gate sits on both the model wire and the tool wire,
/// which makes that every call there has ever been: resuming a conversation
/// recorded one false "interrupted" per completed call, and another set on
/// every later resume. Measured on a real 316-event ledger: 97 of them, all
/// false, for calls that had plainly finished.
///
/// So the question is asked of the whole chain — a call is in flight only if
/// nothing ended it OR anything that grew out of it — and answered once per
/// chain, at its outermost link, so a chain that really did hang is settled
/// once instead of once per copy. Already-settled chains answer "not hanging"
/// (an interruption is an outcome), which is what makes reopening idempotent.
///
/// Note what is NOT assumed: that a call caused by another call is a copy of
/// it. The context gate's condensation request is caused by the call whose
/// material got too big, and it is a call in its own right — treating it as a
/// copy would let the main call's completion cover for it, and a condensation
/// that hung would never be settled.
pub fn hanging_chain_heads(events: &[EventEnvelope], started_type: &str) -> Vec<String> {
    let mut ordered: Vec<_> = events.iter().collect();
    ordered.sort_by_key(|event| event.seq);
    hanging_relation_heads(
        ordered.into_iter().map(|event| EventRelations {
            id: &event.id,
            event_type: &event.event_type,
            causes: &event.causes,
        }),
        started_type,
    )
}

/// The relationships needed to settle calls; historical bodies are irrelevant.
/// Storage can retain this index without retaining every parsed payload.
#[derive(Clone, Copy)]
pub struct EventRelations<'a> {
    pub id: &'a str,
    pub event_type: &'a str,
    pub causes: &'a [String],
}

#[cfg(test)]
#[path = "pending_calls_tests.rs"]
mod pending_calls_tests;

/// Exact call-settlement state over a chronological prefix. Completed nodes
/// disappear, while an unfinished sibling keeps its own identity and parents.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingCalls {
    kind: String,
    next_position: u64,
    pending: std::collections::BTreeMap<String, PendingCall>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingCall {
    position: u64,
    parents: Vec<String>,
}

impl PendingCalls {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.into(),
            next_position: 0,
            pending: Default::default(),
        }
    }

    pub fn observe(&mut self, event: EventRelations<'_>) {
        if event.event_type == self.kind {
            self.pending.insert(
                event.id.into(),
                PendingCall {
                    position: self.next_position,
                    parents: event.causes.to_vec(),
                },
            );
            self.next_position += 1;
        } else if is_outcome(event.event_type) {
            // A head interruption also cancels its existing forwarding copies.
            // Only the original outcome targets settle ancestors; a cancelled
            // descendant must not settle its other, independently pending heads.
            let cancelled = if event.event_type == INTERRUPTED {
                self.interrupted_descendants(event.causes)
            } else {
                Default::default()
            };
            let mut queue = event.causes.to_vec();
            while let Some(id) = queue.pop() {
                if let Some(call) = self.pending.remove(&id) {
                    queue.extend(call.parents);
                }
            }
            if !cancelled.is_empty() {
                self.pending.retain(|id, _| !cancelled.contains(id));
            }
        }
    }

    fn interrupted_descendants(&self, causes: &[String]) -> std::collections::BTreeSet<String> {
        let mut children: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for (id, call) in &self.pending {
            for parent in &call.parents {
                children.entry(parent).or_default().push(id);
            }
        }
        let mut cancelled = std::collections::BTreeSet::new();
        let mut queue: Vec<&str> = causes.iter().map(String::as_str).collect();
        while let Some(id) = queue.pop() {
            // Missing references are not request nodes. In particular, never
            // cross a non-request bridge or a previously settled parent.
            if self.pending.contains_key(id) && cancelled.insert(id.to_owned()) {
                queue.extend(children.get(id).into_iter().flatten().copied());
            }
        }
        cancelled
    }

    /// All unfinished copies in ledger order, including forwards. Consumers
    /// still select the copy actually delivered to their current sink.
    pub fn requests(&self) -> Vec<String> {
        let mut requests: Vec<_> = self.pending.iter().collect();
        requests.sort_by_key(|(_, call)| call.position);
        requests.into_iter().map(|(id, _)| id.clone()).collect()
    }

    pub fn heads(&self) -> Vec<String> {
        let mut heads: Vec<_> = self
            .pending
            .iter()
            .filter(|(id, call)| {
                !call
                    .parents
                    .iter()
                    .any(|parent| parent != *id && self.pending.contains_key(parent))
            })
            .collect();
        heads.sort_by_key(|(_, call)| call.position);
        heads.into_iter().map(|(id, _)| id.clone()).collect()
    }
}

/// Same outcome rule as `hanging_chain_heads`, over a chronological body-free
/// index. Unlike full envelopes, these relations have no sequence to sort by.
pub fn hanging_relation_heads<'a>(
    events: impl IntoIterator<Item = EventRelations<'a>>,
    started_type: &str,
) -> Vec<String> {
    let mut pending = PendingCalls::new(started_type);
    for event in events {
        pending.observe(event);
    }
    pending.heads()
}

/// The effect surface `tool` was declared with, according to the model call
/// that ASKED for `request_id` — found by walking that request's causes back
/// to the call it came out of.
///
/// Policies judge declared surfaces rather than names, so which declaration
/// counts is a security question. Taking the most recent one on the ledger
/// answered it wrongly: nothing said the declaration had to have anything to
/// do with this request. Any component that emits `core.model.call_started`
/// — no wire needed, the kernel appends before it routes — could append a
/// tool list rewriting `Run` as harmless or dropping `admits` off an install
/// tool, and the next request would be judged against that. A request is now
/// judged against the offer it answers, which no one else can substitute for.
/// `look_up` fetches one event by id — a plain function rather than a ledger
/// handle so that this rule stays in the contracts, which know nothing about
/// the kernel. It also makes the walk cheap: the ancestry of a request is a
/// handful of events (the request, the completion it answers, the call that
/// asked), not the conversation.
pub fn declared_effects(
    look_up: impl Fn(&str) -> Option<EventEnvelope>,
    request_id: &str,
    tool: &str,
) -> Option<Value> {
    match try_declared_effects(
        |id| Ok::<_, std::convert::Infallible>(look_up(id)),
        request_id,
        tool,
    ) {
        Ok(effects) => effects,
        Err(never) => match never {},
    }
}

/// Fallible lookup preserves read failures instead of interpreting them as
/// missing declarations. Retain only the best offer, not its material list.
pub fn try_declared_effects<E>(
    look_up: impl Fn(&str) -> Result<Option<EventEnvelope>, E>,
    request_id: &str,
    tool: &str,
) -> Result<Option<Value>, E> {
    let mut seen = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from([request_id.to_string()]);
    let mut best: Option<(u64, Value)> = None;
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(event) = look_up(&id)? else {
            continue;
        };
        queue.extend(event.causes.iter().cloned());
        if event.event_type != MODEL_CALL_STARTED
            || best.as_ref().is_some_and(|(seq, _)| *seq >= event.seq)
        {
            continue;
        }
        if let Some(effects) = event.payload["tools"]
            .as_array()
            .and_then(|tools| tools.iter().find(|decl| decl["name"] == tool))
            .and_then(|decl| decl.get("effects"))
            .filter(|e| !e.is_null())
        {
            best = Some((event.seq, effects.clone()));
        }
    }
    Ok(best.map(|(_, effects)| effects))
}

// ── Reasoning ───────────────────────────────────────────
//
// A thinking model's chain of thought rides on the completed call as
// `reasoning`: an ORDERED list of parts, one part per provider block.
//
// Two kinds. `text` is thinking anybody can read — the frontends show it and
// an agent reading its own trace learns why it did what it did. `hidden` is a
// block the provider encrypted (Anthropic's redacted thinking): it must be
// carried, and cannot be read.
//
// Either kind may carry `opaque`: the part of the block only its own dialect
// understands, kept verbatim so the dialect can hand it straight back.
// Anthropic's per-block signature lives there. It stays INSIDE the part rather
// than in a parallel list precisely so a signature can never be paired with
// the wrong thought.
//
// The core neither reads `opaque` nor decides whether any of this goes back to
// a provider — echoing is dialect law (DeepSeek demands it on tool-call turns,
// and 400s without it), and dialect law lives in adapters.

pub const REASONING_TEXT: &str = "text";
pub const REASONING_HIDDEN: &str = "hidden";

/// One readable block of thinking, plus whatever baggage its dialect needs
/// back later.
pub fn reasoning_text_part(text: &str, opaque: Option<Value>) -> Value {
    let mut part = serde_json::json!({"kind": REASONING_TEXT, "text": text});
    if let Some(opaque) = opaque {
        part["opaque"] = opaque;
    }
    part
}

/// One block whose content the provider sealed: unreadable, still carried.
pub fn reasoning_hidden_part(opaque: Value) -> Value {
    serde_json::json!({"kind": REASONING_HIDDEN, "opaque": opaque})
}

/// The readable thinking of a completed call, blocks joined — what a frontend
/// renders and what a reader of the ledger sees. Sealed blocks contribute
/// nothing, by definition.
pub fn reasoning_text(payload: &Value) -> String {
    let Some(parts) = payload["reasoning"].as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter(|part| part["kind"] == REASONING_TEXT)
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

// ── Payloads ────────────────────────────────────────────

/// Who is at fault for an error — one of the judgment fields policies and
/// agents branch on, instead of parsing prose
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Blame {
    /// The request itself was wrong (fix the input, retrying won't help)
    Request,
    /// The counterparty failed (their outage, their limit)
    Provider,
    /// The local environment failed (network, disk, config)
    Environment,
}

/// Structured error: judgment fields first, taxonomy never. Everything a
/// reader needs to decide — retry? wait? give up? whose fault? — is a field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorInfo {
    /// Namespaced machine code, e.g. "provider.rate_limit", "tool.unknown"
    pub code: String,
    /// One human sentence
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub blame: Blame,
    /// Likely to pass on its own (informs wait-vs-abandon)
    #[serde(default)]
    pub transient: bool,
}

/// The agent's outward reply — the proper data channel to frontends.
/// Turn events are pure boundaries again; they carry no content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputReplyPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessagePayload {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalInputPayload {
    /// Identifier of the injecting source (e.g. a webhook, a timer)
    pub channel: String,
    pub data: Value,
}

/// Tool declaration: what the model sees — who I am, what I do, what my
/// parameters look like — plus what I touch (the effect surface policies
/// reason over; undeclared = most dangerous).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    /// Parameter shape, JSON Schema
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<crate::contracts::component::EffectSurface>,
}

/// One piece of the material sent to a model: either a pointer to an event in
/// this stream (the content lives in the log, recorded once) or a small
/// inline value (e.g. an assembled system-prompt fragment).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MaterialPart {
    Event { event: String },
    Inline { inline: Value },
}

/// Material by reference + fingerprint, not by copy: the log stays O(n) in
/// content while audits can still reconstruct exactly what the model saw by
/// dereferencing the parts. The fingerprint (sha256 over the canonical part
/// list) lets an auditor verify the material was not swapped.
/// Materialization (resolving refs to content) is the model adapter's job,
/// via log read-back — lands together with the real adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInput {
    pub parts: Vec<MaterialPart>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallStartedPayload {
    pub model: String,
    /// What the model is given, by reference (see [`ModelInput`])
    pub input: ModelInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDecl>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// Provider-issued call id; travels with the execution events so results
    /// can be correlated back to the exact call
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionStatus {
    Ok,
    Error,
    /// The work was cancelled (human interrupt, deadline, policy...) — an
    /// ordinary completed state like the others
    Cancelled,
}

/// Failure is not an exception; it is an ordinary completed-state event (status: error)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallCompletedPayload {
    pub status: CompletionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRequest>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    /// Usage and cache statistics, per-call level; per-turn and overall
    /// averages are aggregated from the log by observers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecStartedPayload {
    /// The originating call id (see [`ToolCallRequest::id`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecCompletedPayload {
    /// The originating call id (see [`ToolCallRequest::id`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    pub status: CompletionStatus,
    /// The conclusion, for the model's eyes (enters model material)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Rich data for frontends and audit only; never materialized for models
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptedPayload {
    /// Who initiated the interruption
    pub by: String,
}

/// Kernel-recorded faults (core.control.error): a machine code plus a human
/// sentence. Kernel faults are never retryable by the emitter — they signal
/// bugs or violations, so the judgment fields collapse to just the code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// Namespaced machine code, e.g. "core.undeclared_emission"
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentCrashedPayload {
    /// The crashed component instance name
    pub component: String,
    /// Id of the event being processed at crash time (keeps the causal chain unbroken)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing: Option<String>,
}
