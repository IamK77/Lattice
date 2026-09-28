//! The context gate — the thin pipe on the ask wire (loop → gate → model),
//! the same posture as a policy gate on the tool wire: the loop keeps
//! naively sending its full pointer list; this gate runs the SCALE and the
//! TRIM, then forwards.
//!
//! v1 (mechanical): the window scale — the provider's own count of the LAST
//! call's input tokens (read through the model profile's usage-field
//! mapping) against contextWindow × ratio; over it, old tool results become
//! digest parts. No tokenizer, no model, deterministic.
//!
//! v2 (semantic): when the scale fires, the gate ALSO condenses in the
//! background — it asks its own compactor model instance (assembly-isolated;
//! the main loop never sees this traffic) to squeeze the old segment into a
//! four-section summary (Done / State / Open / Facts). The summary lands on
//! the ledger as the gate's own event (reusable, never recomputed per call);
//! from the NEXT ask on, covered originals are substituted by ONE digest
//! part carrying the summary text — input weight drops, the scale stops
//! firing: the loop converges. The user's turn is never delayed: this turn
//! rides on mechanical trims while the condenser works behind it. Whether a
//! condensation is already in flight is read off the ledger, so a restart
//! cannot double-condense.
//!
//! Every compaction move lands a reasoned decision event. Views are never
//! events: the rewritten material rides inside the forwarded call, so audit
//! sees exactly what the model saw without ledger spam. Without a profile in
//! its config the gate is inert; unplugging it restores the bare list.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::components::model_common::material_fingerprint;
use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

#[path = "context_gate_dials.rs"]
mod dials;
#[path = "context_gate_manual.rs"]
mod manual;
#[path = "context_gate_status.rs"]
mod status;
pub use status::{CompactionFailure, CompactionObserver, CompactionStatus};
#[path = "context_gate_recovery.rs"]
mod recovery;

#[cfg(test)]
mod audit_tests;
#[cfg(test)]
mod excerpt_tests;
#[cfg(test)]
mod metadata_tests;

/// Keep model tool invocations and all their answers in the same view.
/// Shrinking coverage retains originals; it never invents summary coverage.
fn complete_exchange_coverage(
    parts: &[Value],
    mut covered: HashSet<String>,
    log: &crate::kernel::log::LogReader,
) -> Result<HashSet<String>, String> {
    let mut answers = std::collections::HashMap::<String, Vec<&Value>>::new();
    for part in parts {
        if let Some(call) = crate::components::model_common::answered_call(part, log)? {
            answers.entry(call).or_default().push(part);
        }
    }
    let mut groups = Vec::new();
    for id in parts.iter().filter_map(|part| part["event"].as_str()) {
        let Some(event) = log.header(id).map_err(|e| e.to_string())? else {
            continue;
        };
        if event.event_type != ce::MODEL_CALL_COMPLETED || event.tool_calls.is_empty() {
            continue;
        }
        let mut group = vec![event.id.clone()];
        for call in &event.tool_calls {
            if let Some(answer_parts) = call.as_ref().and_then(|id| answers.get(id)) {
                for part in answer_parts {
                    if let Some(id) = part["event"]
                        .as_str()
                        .or_else(|| part["digest"]["of"].as_str())
                    {
                        group.push(id.to_string());
                    }
                }
            } else {
                // An unanswered invocation must survive until its answer.
                covered.remove(&event.id);
            }
        }
        groups.push(group);
    }
    loop {
        let before = covered.len();
        for group in &groups {
            if !group.iter().all(|id| covered.contains(id)) {
                for id in group {
                    covered.remove(id);
                }
            }
        }
        if covered.len() == before {
            return Ok(covered);
        }
    }
}

pub const NAME: &str = "context-gate";
/// The `channel` an external-input event carries to turn the thinking dial.
/// Named like the trust gate's authorization channel and for the same reason:
/// the kernel routes by event type and knows nothing about what a channel
/// means, so the name is an agreement between the frontend and this component
/// alone.
pub const EFFORT_CHANNEL: &str = "model.effort";
/// The `channel` an external-input event carries to tell this gate that the
/// model itself changed — its name, its window, and what its provider calls
/// the usage numbers.
///
/// It rides the same port and the same event type as the effort dial, sorted
/// out by channel, because the kernel routes by type and knows nothing about
/// what a channel means. Sending it as an EVENT rather than rebuilding this
/// component is deliberate twice over: the change lands on the ledger beside
/// the calls it affects, and a gate rebuilt from config would lose the effort
/// a person turned five minutes ago, which lives only in memory.
pub const MODEL_CHANNEL: &str = "model.profile";
/// An explicit, one-shot compaction request. It never changes model settings.
pub const COMPACT_CHANNEL: &str = "context.compact";

pub const DECISION: &str = "context.compaction";
pub const SUMMARY: &str = "context.summary";
/// Marker on the gate's own model calls, so they are distinguishable from
/// forwarded asks on the ledger (both carry the gate as source)
pub const CONDENSE_PURPOSE: &str = "context.condense";

enum CondenseTrigger<'a> {
    Window {
        ask: &'a EventEnvelope,
        measured: u64,
        threshold: u64,
    },
    Manual {
        command: &'a EventEnvelope,
        assembled: &'a Value,
    },
}

impl CondenseTrigger<'_> {
    fn manual(&self) -> bool {
        matches!(self, Self::Manual { .. })
    }
    fn cause(&self) -> &str {
        match self {
            Self::Window { ask, .. } => &ask.id,
            Self::Manual { command, .. } => &command.id,
        }
    }
    fn position(&self) -> u64 {
        match self {
            Self::Window { ask, .. } => ask.seq,
            Self::Manual { command, .. } => command.seq,
        }
    }
    fn assembled(&self) -> &Value {
        match self {
            Self::Window { ask, .. } => &ask.payload,
            Self::Manual { assembled, .. } => assembled,
        }
    }
    fn decision(&self, parts: &[String]) -> (Value, String) {
        let count = parts.len();
        match self {
            Self::Window { measured, threshold, .. } => (
                json!({"action":"condense","digested":parts,"scale":"window","measured":measured,"threshold":threshold}),
                format!("window scale fired ({measured} > {threshold}); condensing {count} old parts in the background"),
            ),
            Self::Manual { .. } => (
                json!({"action":"condense","digested":parts,"scale":"manual"}),
                format!("explicit compaction requested; condensing {count} old parts once"),
            ),
        }
    }
}

enum CondenseOutcome {
    Started,
    Skipped(&'static str),
}
/// What a provider is assumed to call its usage numbers when nothing says.
/// Anthropic's names, because those are what this gate was first written
/// against; a model whose profile names them says so and this is not used.
pub const DEFAULT_USAGE_INPUT: &str = "input_tokens";
pub const DEFAULT_USAGE_CACHE_READ: &str = "cache_read_input_tokens";
/// The four sections a summary must carry; the gate refuses to record one
/// that lacks any (fixed fields make compaction quality checkable)
pub const SUMMARY_MARKERS: [&str; 4] = ["Done:", "State:", "Open:", "Facts:"];

/// Stitch the system prompt: the assembly's base, then every assembled
/// component's own fragment in instance order, then the standing rules — and
/// finally `{model}`, wherever it appears, replaced by the model that will
/// actually receive this.
///
/// The substitution happens HERE, at the last moment, rather than being baked
/// into the base text at startup, because the model can be changed mid
/// conversation. A name filled in once would go on saying the old one, and an
/// agent that cannot say what it is running on guesses.
///
/// Public and shared so that `lattice prompt` shows exactly what a call
/// carries. The alternative — a second copy for inspection — would drift, and
/// a prompt you can inspect but not trust is worse than one you cannot see.
pub fn assemble_system(
    base: Option<&str>,
    fragments_heading: Option<&str>,
    fragments: &[(String, String)],
    tail: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    if let Some(base) = base {
        sections.push(base.to_string());
    }
    if !fragments.is_empty() {
        if let Some(heading) = fragments_heading {
            sections.push(heading.to_string());
        }
    }
    for (_instance, text) in fragments {
        sections.push(text.clone());
    }
    if let Some(tail) = tail {
        sections.push(tail.to_string());
    }
    if sections.is_empty() {
        return None;
    }
    let text = sections.join("\n\n");
    // No model named = leave the placeholder alone. Substituting something
    // vague ("the model") would read as a fact and be one the agent then
    // repeats; an unreplaced placeholder is at least visibly unfilled.
    Some(match model {
        Some(name) => text.replace("{model}", name),
        None => text,
    })
}

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![
            PortDecl::new("ask", &[ce::MODEL_CALL_STARTED]),
            // The condenser's answers come back here
            PortDecl::new("condensed", &[ce::MODEL_CALL_COMPLETED]),
            // Settings a person turns mid-conversation. They arrive as
            // ordinary external input, so the act of changing one is on the
            // ledger like everything else, and the kernel needs no notion of
            // "a setting" at all.
            PortDecl::new("dial", &[ce::EXTERNAL_INPUT]),
        ],
        outputs: vec![
            PortDecl::new("forward", &[ce::MODEL_CALL_STARTED]),
            // The gate's own condenser calls (wire to a dedicated model
            // instance; the main loop never sees this traffic)
            PortDecl::new("condense", &[ce::MODEL_CALL_STARTED]),
            // The gate's own letters, beyond the generic profile
            PortDecl::new("decision", &[DECISION]),
            PortDecl::new("summary", &[SUMMARY]),
        ],
        events: vec![
            EventTypeDecl::decision(
                DECISION,
                "The gate compacted the material for one model call",
            )
            .with_schema(json!({
                "type": "object",
                "required": ["scale", "action"],
                "properties": {
                    "scale": {"enum": ["window", "cache", "manual"]},
                    "action": {"enum": ["digest", "condense", "promote", "refresh_system", "suspend", "compact_skipped"]},
                    "measured": {"type": "integer"},
                    "threshold": {"type": "integer"},
                    "digested": {"type": "array", "items": {"type": "string"}},
                    "promoted": {"type": "array", "items": {"type": "string"}},
                },
            })),
            EventTypeDecl::new(
                SUMMARY,
                "A condensed four-section summary of a conversation segment",
            )
            .with_schema(json!({
                "type": "object",
                "required": ["covers", "text"],
                "properties": {
                    "covers": {"type": "array", "items": {"type": "string"}},
                    "text": {"type": "string"},
                },
            })),
        ],
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["context-manager".to_string()],
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

pub struct ContextGate {
    recovery: std::cell::RefCell<recovery::Recovery>,
    restored_dials: bool,
    /// In-process handoff protection, never part of a committed checkpoint.
    submission: Option<manual::Submission>,
    /// From the model profile; None = the gate is inert (pure pass-through)
    context_window: Option<u64>,
    /// Which native usage field counts input tokens for this model (from
    /// the profile's usageFields mapping)
    usage_input_field: String,
    /// Which native usage field counts cached input tokens (profile mapping)
    usage_cache_field: String,
    /// Trigger at contextWindow × ratio
    ratio: f64,
    /// How many of the newest tool results keep their originals
    keep_recent_tools: usize,
    /// Semantic condensation on/off (needs the condense wire in the assembly)
    condense: bool,
    /// Native format requested from the condenser, when configured by assembly.
    native_compaction: bool,
    native_target: Option<Value>,
    /// How many of the newest parts are never condensed away
    keep_recent_parts: usize,
    /// Don't bother condensing segments smaller than this
    min_condense: usize,
    /// Observe foreign streams (a sidechannel's parent) by prepending a
    /// transcript digest to every forwarded call. On by default: an observer
    /// that cannot see is not an observer.
    observe_foreign: bool,
    /// The base system prompt this assembly opens with; component fragments
    /// (ctx.prompt_fragments) are appended after it. None + no fragments =
    /// no system on the forwarded call, and the adapter's config fallback
    /// applies.
    system_base: Option<String>,
    /// A heading to put above the appended fragments. A base prompt written in
    /// sections needs this: without it the fragments read as more of whatever
    /// section the base happened to end on. None = append them bare.
    fragments_heading: Option<String>,
    /// Text to put AFTER the fragments. The base opens the prompt and the
    /// fragments land in the middle, so anything that belongs at the end —
    /// standing rules, the last word — has nowhere else to go.
    system_tail: Option<String>,
    /// The thinking setting a person turned mid-conversation, stamped onto
    /// every ask from then on. `None` until someone turns it — the adapter's
    /// own config is what runs until then, so a session nobody touches
    /// behaves exactly as it did before this port existed.
    thinking: Option<Value>,
    /// Which model this prompt is being assembled for — substituted into
    /// `{model}` wherever the base text mentions it. Follows the model when it
    /// is changed, so the agent's answer to "what are you running on" is the
    /// model actually receiving the question.
    model_name: Option<String>,
    /// Tools kept out of the model's schema for good — not because they are
    /// new, but because the assembly judged them uncommon. They are reachable
    /// by searching the catalogue and calling through the resident dispatcher.
    /// Naming them here rather than in each component's manifest is deliberate:
    /// which tools are common is a property of the deployment, not of the
    /// component that provides them.
    defer_tools: Vec<String>,
}

impl ContextGate {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let profile = get("profile");
        Self {
            recovery: recovery::empty(),
            restored_dials: false,
            submission: None,
            context_window: profile
                .and_then(|p| p.get("contextWindow"))
                .and_then(Value::as_u64),
            usage_input_field: profile
                .and_then(|p| p.pointer("/usageFields/input"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_USAGE_INPUT)
                .to_string(),
            usage_cache_field: profile
                .and_then(|p| p.pointer("/usageFields/cacheRead"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_USAGE_CACHE_READ)
                .to_string(),
            ratio: get("ratio").and_then(Value::as_f64).unwrap_or(0.85),
            keep_recent_tools: get("keepRecentTools").and_then(Value::as_u64).unwrap_or(2) as usize,
            condense: get("condense").and_then(Value::as_bool).unwrap_or(false),
            native_compaction: get("nativeCompaction")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            native_target: get("nativeTarget").cloned(),
            keep_recent_parts: get("keepRecentParts").and_then(Value::as_u64).unwrap_or(6) as usize,
            min_condense: get("minCondense").and_then(Value::as_u64).unwrap_or(3) as usize,
            observe_foreign: get("observeForeign")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            system_base: get("system").and_then(Value::as_str).map(str::to_string),
            fragments_heading: get("fragmentsHeading")
                .and_then(Value::as_str)
                .map(str::to_string),
            thinking: None,
            model_name: get("modelName").and_then(Value::as_str).map(str::to_string),
            system_tail: get("systemTail")
                .and_then(Value::as_str)
                .map(str::to_string),
            defer_tools: get("deferTools")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The provider's count of the last MAIN call's input, read off the
    /// ledger. Side-channel completions (the gate's own condenser answers,
    /// recognizable by the purpose marker on their causing request) never
    /// weigh in: the scale measures the conversation, not the plumbing —
    /// and the two land on the ledger in scheduler order, so trusting
    /// "the last completion" blindly would make the scale nondeterministic.
    fn last_measured_input(&self, ctx: &Ctx) -> Result<u64, String> {
        self.last_main_usage(ctx.log(), &self.usage_input_field)
    }

    /// One number off the last MAIN model completion. Scanned in place: the
    /// side-channel test needs the request each answer answers, and following
    /// that cause inside the scan costs a hash lookup where copying the
    /// conversation to do it outside cost the conversation.
    fn last_main_usage(&self, log: &crate::LogReader, field: &str) -> Result<u64, String> {
        let Some(id) = self.observation(log, |state| state.main_completion.clone())? else {
            return Ok(0);
        };
        let event = log
            .get(&id)
            .map_err(|error| format!("reading {id}: {error}"))?
            .ok_or_else(|| format!("indexed completion {id} missing"))?;
        Ok(field
            .split('.')
            .try_fold(&event.payload["usage"], |value, key| value.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0))
    }

    /// Is a condensation already on its way? Read off the ledger, never from
    /// memory — a restarted gate must not double-condense.
    ///
    /// "On its way" means no ending yet, and an interruption is an ending as
    /// much as a completion is. Counting only completions made one interrupted
    /// condensation — a restart mid-condense, a condense model that died —
    /// look permanently in flight, and semantic condensation for that stream
    /// never ran again, in this process or any later one.
    fn condense_in_flight(&self, log: &crate::LogReader) -> Result<bool, String> {
        if self.submission.is_some() {
            return Ok(true);
        }
        self.observation(log, |state| state.in_flight())
    }

    /// A recorded failure is not permission to repeat the call on every
    /// over-budget turn. Keep the pause on the ledger, including across
    /// restart. Only an adopted summary or an explicit model change clears it.
    /// Cancellation and late answers from an old configuration do not clear it.
    fn condensation_paused(&self, log: &crate::LogReader) -> Result<bool, String> {
        let Some(id) = self.observation(log, |state| state.condense_failure.clone())? else {
            return Ok(false);
        };
        log.get(&id)
            .map_err(|error| format!("reading {id}: {error}"))?
            .ok_or_else(|| format!("indexed compaction failure {id} missing"))?;
        Ok(true)
    }

    /// The cached-input count of the last MAIN call (side-channel
    /// completions skipped, same as the input scale). Zero when the provider
    /// reported none — a cold cache.
    fn last_cache_read(&self, ctx: &Ctx) -> Result<u64, String> {
        self.last_main_usage(ctx.log(), &self.usage_cache_field)
    }

    /// The system text of the last MAIN call this gate forwarded (no purpose
    /// marker, system stamped), read off the ledger — no memory, restart-safe.
    fn last_forwarded_system(&self, ctx: &Ctx) -> Result<Option<String>, String> {
        // The prompt itself, not the reference to it. Past a size it lives in
        // a file beside the ledger, and comparing a reference against the
        // prompt about to be sent would read as "it changed" on every turn.
        // Resolved outside the scan, because the closure runs under the read
        // lock and reading a file there would hold it for no reason.
        let Some(id) = self.observation(ctx.log(), |state| state.system_request.clone())? else {
            return Ok(None);
        };
        let event = ctx
            .log()
            .get(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "indexed request missing".to_string())?;
        Ok(ctx
            .document(&event.payload["system"])?
            .as_str()
            .map(str::to_string))
    }

    /// Deferred tools this gate has already promoted, read off its own
    /// promotion decisions — no memory, restart-safe.
    fn promoted_tools(&self, ctx: &Ctx) -> Result<HashSet<String>, String> {
        self.observation(ctx.log(), |state| state.promoted.clone())
    }

    /// The latest recorded summary, if any: (event id, covered ids, text).
    fn latest_summary(
        &self,
        ctx: &Ctx,
    ) -> Result<Option<(String, HashSet<String>, String)>, String> {
        let Some(id) = self.observation(ctx.log(), |state| {
            state.latest_summary(self.native_compaction, self.native_target.as_ref())
        })?
        else {
            return Ok(None);
        };
        let e = ctx
            .log()
            .get(&id)
            .map_err(|error| format!("reading {id}: {error}"))?
            .ok_or_else(|| format!("indexed summary {id} missing"))?;
        let covers = e.payload["covers"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let text = e.payload["text"].as_str().unwrap_or_default().to_string();
        Ok(Some((id, covers, text)))
    }
}

/// One line standing in for a tool result: what happened, a glimpse of the
/// outcome, and the event id — the note must always say where the full text
/// lives. File-tool instructions resolve the id in either a legacy file or
/// a segmented ledger; an id is not an unconditional physical line number.
fn digest_line(event: &EventEnvelope) -> String {
    let call = event.payload["call"].as_str().unwrap_or("?");
    let status = event.payload["status"].as_str().unwrap_or("?");
    let result = if event.payload["status"] == "error" {
        event.payload["error"]["message"]
            .as_str()
            .unwrap_or("error")
            .to_string()
    } else {
        event.payload["result"].to_string()
    };
    let brief: String = result.chars().take(160).collect();
    let ellipsis = if result.chars().count() > 160 {
        "…"
    } else {
        ""
    };
    format!(
        "tool call {call} ({status}): {brief}{ellipsis} — full text at event {}",
        event.id
    )
}

/// A bounded excerpt, not a semantic summary. Scan borrowed events newest
/// first: a side question must never deep-copy its parent's entire ledger.
const FOREIGN_EXCERPT_CHARS: usize = 8_000;

fn foreign_transcript_digest(
    stream: &str,
    reader: &crate::kernel::log::LogReader,
) -> Result<Option<Value>, String> {
    let mut remaining = FOREIGN_EXCERPT_CHARS;
    let mut lines = Vec::new();
    let mut last_id = None;
    let mut through = None;
    let mut omitted = false;
    reader
        .scan_back_headers(|header, nearby| {
            through.get_or_insert_with(|| header.id.clone());
            let role = match header.event_type.as_str() {
                ce::USER_MESSAGE if header.causes.is_empty() => "user",
                ce::OUTPUT_REPLY => "assistant",
                _ => return Ok(None),
            };
            let event = nearby
                .get(&header.id)?
                .ok_or_else(|| std::io::Error::other("indexed message missing"))?;
            let Some(text) = event.payload["text"].as_str() else {
                return Ok(None);
            };
            let prefix = format!("[{}] {role}: ", event.id);
            let available = remaining.saturating_sub(prefix.chars().count() + 1);
            let mut body: String = text.chars().take(available + 1).collect();
            if body.chars().count() > available {
                omitted = true;
                if !lines.is_empty() {
                    return Ok(Some(())); // keep recent messages whole, omit older ones
                }
                const CUT: &str = "\n[... middle omitted; read this event for the full text ...]\n";
                let room = available.saturating_sub(CUT.chars().count());
                let head: String = text.chars().take(room / 2).collect();
                let tail: String = text
                    .chars()
                    .rev()
                    .take(room - room / 2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                body = format!("{head}{CUT}{tail}");
            }
            last_id.get_or_insert_with(|| event.id.clone());
            let line = format!("{prefix}{body}");
            remaining = remaining.saturating_sub(line.chars().count() + 1);
            lines.push(line);
            Ok(omitted.then_some(()))
        })
        .map_err(|e| e.to_string())?;
    let Some(last_id) = last_id else {
        return Ok(None);
    };
    lines.reverse();
    let location = reader
        .path()
        .map(|path| json!(path).to_string())
        .unwrap_or_else(|| "unavailable (in-memory ledger)".into());
    Ok(Some(json!({"digest": {
        "of": last_id,
        "text": format!(
            "[observing stream {stream:?}; parent ledger: {location}; snapshot through {}]\n\
             This is a mechanical excerpt of original messages, NOT a generated summary or complete memory. \
             It is refreshed before each model request; parent changes do not wake this conversation. \
             Only original user inputs and final assistant text are included; tools, reasoning and attachments are omitted. \
             Each bracketed event id identifies an original record in the parent ledger. \
             Missing text is not evidence that something did not happen.\n{}\n{}\n{}",
            through.expect("an included event implies a snapshot"),
            reader.path().map(super::environment::ledger_reading_note)
                .unwrap_or_else(|| "Original records are not available as local files.".into()),
            if omitted { "[Earlier messages or part of a message omitted by the excerpt budget.]" }
            else { "[All eligible messages fit the excerpt budget.]" },
            lines.join("\n")
        ),
    }})))
}

impl Component for ContextGate {
    fn restore(&mut self, ctx: &mut Ctx) {
        // Prepare the existing derived checkpoint once; observers only read it.
        if let Err(error) = self.restore_dials_through(ctx.log(), ctx.log().snapshot_end()) {
            ctx.fail("restore context observations", error.to_string(), &[]);
        }
    }

    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if let Err(error) = self.restore_dials(ctx.log(), event.seq) {
            ctx.fail("restore context settings", error.to_string(), &[]);
            return;
        }
        if let Err(error) = self.refresh_submission(ctx.log()) {
            ctx.fail("read compaction handoff", error, &[]);
            return;
        }
        let result = match port {
            "ask" => self.on_ask(event, ctx),
            "condensed" => self.on_condensed(event, ctx),
            "dial" if event.payload["channel"] == COMPACT_CHANNEL => {
                self.on_manual_compact(event, ctx)
            }
            "dial" => {
                self.on_dial(event);
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            if port == "condensed" {
                ctx.emit(
                    "decision",
                    EventDraft::new(
                        DECISION,
                        &[&event.id],
                        json!({"scale":"window","action":"compact_skipped"}),
                    )
                    .with_reason(&format!("Compaction could not be applied: {error}")),
                );
            }
            ctx.fail("prepare context view", error, &[]);
        }
    }
}

impl ContextGate {
    fn on_ask(&mut self, event: &EventEnvelope, ctx: &mut Ctx) -> Result<(), String> {
        let original_parts = event.payload["input"]["parts"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        // A recorded summary substitutes on EVERY ask — that is how the loop
        // converges: covered originals stop travelling, input weight drops,
        // the scale stops firing
        let (mut parts, summarized_ids) = self.substitute_summary(&original_parts, ctx)?;

        // Foreign observation: a derived stream (a /btw sidechannel) holds
        // read-only handles onto the streams it observes — its parent. Their
        // conversations ride in front of the material as ONE digest each,
        // refreshed every ask (the parent moves on while we talk). Normal
        // streams hold no handles; nothing is added.
        if self.observe_foreign {
            for stream in ctx.foreign_streams().iter().rev() {
                if let Some(part) = ctx
                    .foreign_log(stream)
                    .map(|reader| foreign_transcript_digest(stream, reader))
                    .transpose()?
                    .flatten()
                {
                    parts.insert(0, part);
                }
            }
        }

        // Carry the previous view's mechanical digests forward. A smaller
        // measured request is not permission to restore the discarded bytes.
        let previous = self.observation(ctx.log(), |state| state.forwarded_request.clone())?;
        let previous_digests = if let Some(id) = previous {
            let e = ctx
                .log()
                .get(&id)
                .map_err(|error| format!("reading {id}: {error}"))?
                .ok_or_else(|| format!("indexed request {id} missing"))?;
            e.payload["input"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    p["digest"]["of"]
                        .as_str()
                        .map(|id| (id.to_owned(), p.clone()))
                })
                .collect::<std::collections::HashMap<_, _>>()
        } else {
            Default::default()
        };
        for part in &mut parts {
            if let Some(id) = part["event"].as_str() {
                if let Some(previous) = previous_digests.get(id) {
                    *part = previous.clone();
                }
            }
        }

        // A new summary must be measured before its predecessor's usage can
        // trigger another condensation. Inspect the answered request, not the
        // completion timestamp: an old request can finish after the summary.
        let summary_id = self.latest_summary(ctx)?.map(|(id, _, _)| id);
        let summary_measured = match summary_id {
            None => true,
            Some(id) => {
                let request =
                    self.observation(ctx.log(), |state| state.measured_request.clone())?;
                match request {
                    None => false,
                    Some(request) => {
                        let request = ctx
                            .log()
                            .get(&request)
                            .map_err(|e| e.to_string())?
                            .ok_or_else(|| "indexed request missing".to_string())?;
                        request.payload["input"]["parts"]
                            .as_array()
                            .is_some_and(|parts| parts.iter().any(|p| p["digest"]["of"] == id))
                    }
                }
            }
        };
        // Only a measurement of the current summary generation can fire.
        let mut condensation_scale = None;
        if let Some(window) = self.context_window {
            let threshold = (window as f64 * self.ratio) as u64;
            let measured = self.last_measured_input(ctx)?;
            if measured > threshold && summary_measured {
                let (trimmed, digested) = self.trim(&parts, ctx)?;
                if !digested.is_empty() {
                    parts = trimmed;
                    ctx.emit(
                        "decision",
                        EventDraft::new(
                            DECISION,
                            &[&event.id],
                            json!({
                                "scale": "window",
                                "action": "digest",
                                "measured": measured,
                                "threshold": threshold,
                                "digested": digested,
                            }),
                        )
                        .with_reason(&format!(
                            "window scale fired: last call used {measured} input tokens, \
                             over the threshold of {threshold} ({window} × {})",
                            self.ratio
                        )),
                    );
                }
                condensation_scale = Some((measured, threshold));
            }
        }

        // Forward — a re-emission with a causal link (the hop stays
        // audit-visible), fingerprint recomputed over what actually goes out
        let mut payload = event.payload.clone();
        payload["input"] = json!({
            "parts": parts,
            "fingerprint": material_fingerprint(&parts),
        });
        // Deferred tools stay OUT of the model-visible schema, because the
        // schema sits in the cached prefix of every call and anything added to
        // it throws away the cache for the whole conversation behind it. Two
        // kinds are deferred, and they end differently:
        //
        //  - PERMANENTLY, by the assembly's judgement that they are uncommon.
        //    These never enter the schema; the model finds them by searching
        //    the catalogue, whose result lands at the TAIL of the conversation
        //    where an append costs nothing.
        //  - NEWLY INSTALLED, until a moment when the cache is cold anyway, at
        //    which point promoting them is free. This needs a profile: no
        //    profile, no cache knowledge, nothing to time it against.
        //
        // Either way they are callable through one resident dispatcher, whose
        // text never changes and so never invalidates anything.
        // The loop's tool list may have moved out to a document; the gate has
        // to see the declarations to narrow them.
        let offered = ctx.document(&payload["tools"]).unwrap_or(Value::Null);
        if let Some(offered) = offered.as_array().cloned() {
            let promoted = self.promoted_tools(ctx)?;
            let measure_cache = self.context_window.is_some();
            let (mut resident, mut waiting, mut hidden) = (Vec::new(), Vec::new(), 0usize);
            for decl in offered {
                let name = decl["name"].as_str().unwrap_or_default().to_string();
                if self.defer_tools.iter().any(|d| d == &name) {
                    hidden += 1;
                    continue;
                }
                if measure_cache && decl["installed"] == true && !promoted.contains(&name) {
                    waiting.push(decl);
                    continue;
                }
                resident.push(decl);
            }
            if !waiting.is_empty() {
                let cache_read = self.last_cache_read(ctx)?;
                if cache_read == 0 {
                    let names: Vec<String> = waiting
                        .iter()
                        .filter_map(|d| d["name"].as_str().map(str::to_string))
                        .collect();
                    ctx.emit(
                        "decision",
                        EventDraft::new(
                            DECISION,
                            &[&event.id],
                            json!({
                                "scale": "cache",
                                "action": "promote",
                                "measured": cache_read,
                                "promoted": names,
                            }),
                        )
                        .with_reason(
                            "the prompt cache is cold anyway; promoting deferred tools \
                             into the schema costs nothing extra now",
                        ),
                    );
                    resident.append(&mut waiting);
                } else {
                    hidden += waiting.len();
                }
            }
            if hidden > 0 {
                // The one stable doorway to everything left out
                resident.push(crate::contracts::component::deferred_dispatcher_decl());
            }
            payload["tools"] = json!(resident);
        }

        // The injection axis: base text + every assembled component's own
        // fragment ("installed = the model is told"), in instance order.
        // Recorded in the forwarded event — the exact system prompt of every
        // call is on the ledger. An upstream-provided system wins untouched.
        if payload["system"].is_null() {
            let fresh = assemble_system(
                self.system_base.as_deref(),
                self.fragments_heading.as_deref(),
                &ctx.prompt_fragments(),
                self.system_tail.as_deref(),
                self.model_name.as_deref(),
            );
            if let Some(fresh) = fresh {
                // Fragments may change at runtime (a skill library refreshing
                // its listing, a hot-installed component's passage). A changed
                // system is adopted on the same terms as a deferred tool:
                // only when the provider cache is cold anyway, on the record.
                // Needs a profile — no profile, no cache knowledge, no
                // deferring.
                let previous_system = if self.context_window.is_some() {
                    self.last_forwarded_system(ctx)?
                } else {
                    None
                };
                let adopted = match previous_system {
                    Some(last) if last != fresh => {
                        let cache_read = self.last_cache_read(ctx)?;
                        if cache_read == 0 {
                            ctx.emit(
                                "decision",
                                EventDraft::new(
                                    DECISION,
                                    &[&event.id],
                                    json!({
                                        "scale": "cache",
                                        "action": "refresh_system",
                                        "measured": cache_read,
                                    }),
                                )
                                .with_reason(
                                    "the prompt cache is cold anyway; adopting the changed \
                                     component fragments into the system prompt costs nothing \
                                     extra now",
                                ),
                            );
                            fresh
                        } else {
                            // Warm: the cached prefix is worth more than the
                            // update — keep forwarding the system on the books
                            last
                        }
                    }
                    _ => fresh,
                };
                payload["system"] = json!(adopted);
            }
        }
        // The thinking axis. Set only once a person has turned it; until then
        // the adapter's own config decides, exactly as before. Stamped on the
        // FORWARDED event, so the effort of every call is on the ledger beside
        // the call it belonged to — nobody has to reconstruct "what was it set
        // to at turn twelve" from a change made ten turns earlier.
        if payload["thinking"].is_null() {
            if let Some(thinking) = &self.thinking {
                payload["thinking"] = thinking.clone();
            }
        }
        if let Some((measured, threshold)) = condensation_scale {
            // Assemble once; both calls see exactly the same adopted prompt
            // and resident tool definitions. The cause remains the input ask.
            let mut assembled_ask = event.clone();
            assembled_ask.payload = payload.clone();
            self.maybe_condense(
                CondenseTrigger::Window {
                    ask: &assembled_ask,
                    measured,
                    threshold,
                },
                &original_parts,
                &summarized_ids,
                ctx,
            )?;
        }
        ctx.emit(
            "forward",
            EventDraft::new(ce::MODEL_CALL_STARTED, &[&event.id], payload),
        );
        Ok(())
    }

    /// A person turned a setting.
    ///
    /// For effort, the value is taken as written and handed to the adapter
    /// untouched: which words mean anything is the dialect's business, and a
    /// gate that vetted them would have to know every wire.
    ///
    /// For a model change, what arrives is the new model's PROFILE — its name,
    /// its window, its usage-field names — because those are what this gate
    /// budgets with, and they belong to the model rather than to the endpoint
    /// or the dialect. Each field is taken only if it was sent: a swap to a
    /// model that ships no profile leaves the previous window in force, which
    /// is the honest fallback (see `crate::profile::context_window`).
    fn on_dial(&mut self, event: &EventEnvelope) {
        let mut dials = dials::Dials::default();
        dials.observe(&event.payload);
        dials.apply(self);
    }

    /// Replace every pointer covered by the latest summary with ONE digest
    /// part carrying the summary text (and the summary event's id for
    /// looked up again). Returns the rewritten parts and the covered ids.
    fn substitute_summary(
        &self,
        parts: &[Value],
        ctx: &Ctx,
    ) -> Result<(Vec<Value>, HashSet<String>), String> {
        let Some((summary_id, covers, text)) = self.latest_summary(ctx)? else {
            return Ok((parts.to_vec(), HashSet::new()));
        };
        // Old ledgers may contain summaries cut through a tool exchange.
        // Restore the uncovered group's original pointers rather than replay
        // an orphan result. The ledger and summary remain untouched.
        let covers = complete_exchange_coverage(parts, covers, ctx.log())?;
        let mut placed = false;
        let rewritten = parts
            .iter()
            .filter_map(|part| match part["event"].as_str() {
                Some(id) if covers.contains(id) => {
                    if placed {
                        None // the whole covered run collapses into one part
                    } else {
                        placed = true;
                        Some(json!({"digest": {
                            "of": summary_id,
                            "text": format!("[condensed summary of earlier conversation] {text}"),
                        }}))
                    }
                }
                _ => Some(part.clone()),
            })
            .collect();
        Ok((rewritten, covers))
    }

    /// Mechanical trim (doctrine level one): every tool-result pointer except
    /// the newest `keep_recent_tools` becomes a digest part. User messages
    /// and model turns are never touched here.
    fn trim(&self, parts: &[Value], ctx: &Ctx) -> Result<(Vec<Value>, Vec<String>), String> {
        let mut tool_positions = Vec::new();
        for (at, part) in parts.iter().enumerate() {
            let Some(id) = part["event"].as_str() else {
                continue;
            };
            if ctx
                .log()
                .event_type(id)
                .map_err(|error| error.to_string())?
                .as_deref()
                == Some(ce::TOOL_EXEC_COMPLETED)
            {
                tool_positions.push(at);
            }
        }
        let cut = tool_positions.len().saturating_sub(self.keep_recent_tools);
        let to_digest: HashSet<usize> = tool_positions[..cut].iter().copied().collect();
        let mut digested = Vec::new();
        let mut rewritten = Vec::with_capacity(parts.len());
        for (at, part) in parts.iter().enumerate() {
            if !to_digest.contains(&at) {
                rewritten.push(part.clone());
                continue;
            }
            let id = part["event"].as_str().expect("tool positions are pointers");
            let event = ctx
                .log()
                .get(id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("indexed tool result is missing: {id}"))?;
            digested.push(id.to_string());
            rewritten.push(json!({"digest": {"of": id, "text": digest_line(&event)}}));
        }
        Ok((rewritten, digested))
    }

    /// Kick off a background condensation of the old segment, unless one is
    /// already in flight or the segment is too small to be worth a call.
    /// This turn is NOT delayed: it rides on the mechanical trim while the
    /// condenser works behind it.
    fn maybe_condense(
        &mut self,
        trigger: CondenseTrigger<'_>,
        original_parts: &[Value],
        already_covered: &HashSet<String>,
        ctx: &mut Ctx,
    ) -> Result<CondenseOutcome, String> {
        if !self.condense {
            return Ok(CondenseOutcome::Skipped(
                "Compaction is not enabled in this assembly",
            ));
        }
        if self.condense_in_flight(ctx.log())? {
            return Ok(CondenseOutcome::Skipped(
                "Compaction is already in progress",
            ));
        }
        if !trigger.manual() && self.condensation_paused(ctx.log())? {
            return Ok(CondenseOutcome::Skipped("Automatic compaction is paused"));
        }
        // The segment: original pointers old enough to give up, not yet
        // covered by a summary. The recent tail stays out.
        let pointers: Vec<&str> = original_parts
            .iter()
            .filter_map(|p| p["event"].as_str())
            .collect();
        let cut = pointers.len().saturating_sub(self.keep_recent_parts);
        let selected = pointers[..cut].iter().map(|id| id.to_string()).collect();
        let selected = complete_exchange_coverage(original_parts, selected, ctx.log())?;
        let segment: Vec<String> = pointers[..cut]
            .iter()
            .filter(|id| selected.contains(**id) && !already_covered.contains(**id))
            .map(|id| id.to_string())
            .collect();
        if segment.len() < self.min_condense {
            return Ok(CondenseOutcome::Skipped(
                "Not enough older context is available to compact safely",
            ));
        }

        let (decision, reason) = trigger.decision(&segment);
        // Rolling condensation: the previous summary rides along as a digest
        // part, so the condenser refreshes ONE full summary instead of
        // fragmenting coverage across generations
        let mut parts: Vec<Value> = Vec::new();
        if let Some((summary_id, _, text)) = self.latest_summary(ctx)? {
            parts.push(json!({"digest": {
                "of": summary_id,
                "text": format!("[previous summary — fold into the new one] {text}"),
            }}));
        }
        parts.extend(segment.iter().map(|id| json!({"event": id})));
        let mut payload = json!({
            "model": "condenser",
            "purpose": if self.native_compaction { "context.compact.responses" } else { CONDENSE_PURPOSE },
            "input": {"parts": parts, "fingerprint": material_fingerprint(&parts)},
        });
        if self.native_compaction {
            // Native state must be built with the same instructions and tool
            // definitions as the conversation it replaces.
            for field in ["system", "tools"] {
                if let Some(value) = trigger.assembled().get(field) {
                    payload[field] = value.clone();
                }
            }
        }
        self.submission = Some(manual::Submission::new(trigger.cause(), trigger.position()));
        ctx.emit(
            "decision",
            EventDraft::new(DECISION, &[trigger.cause()], decision).with_reason(&reason),
        );
        ctx.emit(
            "condense",
            EventDraft::new(ce::MODEL_CALL_STARTED, &[trigger.cause()], payload),
        );
        Ok(CondenseOutcome::Started)
    }

    /// The condenser answered. A summary is recorded only when the call
    /// succeeded AND the text carries all four sections — a summary missing
    /// its fixed fields is not a summary (checkable quality, per doctrine).
    fn on_condensed(&mut self, event: &EventEnvelope, ctx: &mut Ctx) -> Result<(), String> {
        if !self.observation(ctx.log(), |state| {
            state.condense_completion.as_deref() == Some(&event.id)
        })? {
            return self.compact_without_summary(event, false,
                "Compaction result belongs to an older configuration or attempt; no summary was applied", ctx);
        }
        if event.payload["status"] != "ok" {
            return self.compact_without_summary(event, event.payload["status"] == "error",
                if event.payload["status"] == "error" {
                    "Background compaction failed; automatic attempts are paused. Use /compact to request one retry, or change the model"
                } else {
                    "Compaction was cancelled; no summary was applied and any existing pause is unchanged"
                }, ctx);
        }
        if matches!(
            event.payload["stopReason"].as_str(),
            Some("length" | "max_tokens")
        ) {
            return self.compact_without_summary(
                event,
                true,
                "Compaction reached its output limit; the incomplete result was not applied and automatic attempts are paused",
                ctx,
            );
        }
        let native = event.payload.get("nativeCompaction").filter(|value| {
            self.native_compaction
                && value["dialect"] == "responses"
                && crate::components::responses_wire::compact_output(value).is_ok()
        });
        let text = event.payload["text"].as_str().unwrap_or_default();
        if native.is_none() && !SUMMARY_MARKERS.iter().all(|marker| text.contains(marker)) {
            return self.compact_without_summary(event, true, "Compaction returned no valid native result or complete four-section summary; automatic attempts are paused", ctx);
        }
        // What it covers: the pointer parts of the condense request this
        // completion answers — plus, when the request folded a previous
        // summary in (rolling condensation), everything THAT one covered
        let mut covers: Vec<String> = Vec::new();
        if let Some(request) = event
            .causes
            .first()
            .map(|id| ctx.log().get(id))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten()
        {
            for part in request.payload["input"]["parts"]
                .as_array()
                .unwrap_or(&Vec::new())
            {
                if let Some(id) = part["event"].as_str() {
                    covers.push(id.to_string());
                } else if let Some(prev_id) = part["digest"]["of"].as_str() {
                    if let Some(prev) = ctx.log().get(prev_id).map_err(|e| e.to_string())? {
                        if prev.event_type == SUMMARY {
                            covers.extend(
                                prev.payload["covers"]
                                    .as_array()
                                    .unwrap_or(&Vec::new())
                                    .iter()
                                    .filter_map(|v| v.as_str().map(str::to_string)),
                            );
                        }
                    }
                }
            }
        }
        if covers.is_empty() {
            return self.compact_without_summary(
                event,
                true,
                "Compaction result covers no recorded context; automatic attempts are paused",
                ctx,
            );
        }
        ctx.emit(
            "summary",
            EventDraft::new(
                SUMMARY,
                &[&event.id],
                json!({"covers": covers, "text": text, "nativeCompaction": native}),
            ),
        );
        Ok(())
    }
}
