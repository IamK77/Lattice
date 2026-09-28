use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::event::EventTypeDecl;

/// The shared effect vocabulary: what a tool call or a whole component may
/// touch. Used at tool level (per-call effects on `ToolDecl`) and at
/// component level (process-wide `capabilities`); policies reason over this,
/// never over names. Undeclared = treated as the most dangerous.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectSurface {
    /// Paths/scopes it may read
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    /// Paths/scopes it may write
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    /// Network hosts it may reach ("*" = any)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<String>,
    /// Whether it runs external programs
    #[serde(default)]
    pub executes: bool,
    /// Whether its effects can be undone
    #[serde(default)]
    pub reversible: bool,
    /// What this call ADMITS into the runtime — new code, new instructions —
    /// when it admits anything. The marker the trust gate keys on, so that it
    /// never has to key on a tool's name.
    ///
    /// Present in the canon from the start and missing from this type, which
    /// meant the typed path silently dropped it: a component whose
    /// `capabilities` said it admits code came back through an overlay
    /// round-trip saying it admits nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admits: Option<String>,
}

/// Port: where a component exchanges events with the outside.
/// An input port's events declare "which event types I accept"; `["*"]` means
/// accept-all (e.g. audit observers). An output port's events declare "which
/// event types I may emit"; `"*"` is not allowed — you cannot promise everything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDecl {
    pub name: String,
    pub events: Vec<String>,
}

impl PortDecl {
    pub fn new(name: &str, events: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            events: events.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// One wire in a default-wiring suggestion; "self" refers to this component's future instance name
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSuggestion {
    pub from: String,
    pub to: String,
}

/// Form: loaded in-process, or a separate process (an executable in any language).
/// Direction (to be refined): untrusted components compiled to WASM, running in an
/// in-process sandbox with per-capability grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Inproc,
    Process,
}

/// Component manifest — the self-description of a component as a distribution unit.
/// The basis of "download = install": the installer reads it and merges the
/// default wiring into the assembly manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentManifest {
    pub name: String,
    pub version: String,
    pub runtime: RuntimeKind,
    /// Entry: module path or executable path
    pub entry: String,
    #[serde(default)]
    pub inputs: Vec<PortDecl>,
    #[serde(default)]
    pub outputs: Vec<PortDecl>,
    /// Letter types this component brings (registered into the log at assembly time)
    #[serde(default)]
    pub events: Vec<EventTypeDecl>,
    /// Default wiring suggestions applied at install time
    #[serde(default)]
    pub default_wiring: Vec<WireSuggestion>,
    /// Declared effect surface of the whole component (trust model: a trust
    /// record = content fingerprint + the granted surface). None = declares
    /// nothing = treated as the most dangerous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<EffectSurface>,
    /// Standard port profiles this component claims to implement (see
    /// contracts/profile.rs). Inspection verifies claims structurally;
    /// conformance exams verify them behaviorally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
    /// Per-delivery processing deadline in milliseconds. When it expires the
    /// kernel auto-cancels the delivery (the "watchman" trigger) — no human
    /// needed. None = no deadline; long-running components should set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_timeout_ms: Option<u64>,
    /// How many deliveries this component may handle AT ONCE. One (the
    /// default) is the mailbox as it has always been: a queue, drained in
    /// order, one at a time, so a component may keep mutable state and block
    /// inside `handle` without thinking about anyone else.
    ///
    /// More than one turns the mailbox into a work queue with that many
    /// workers, each holding its own instance of the component. It is what a
    /// tool provider wants: when a model asks for three commands in one turn
    /// they should run together, and with a single worker the second waits for
    /// the first to finish — which a real session showed costing 90 seconds
    /// for three 30-second commands. Only for components that hold no state
    /// between deliveries, and it gives up FIFO: results come back in whatever
    /// order they finish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    /// One passage merged into the system prompt whenever this component is
    /// assembled — the component telling the model how to use it. Installed
    /// = the sentence appears; removed = it disappears. The assembly may
    /// override per instance (config key "prompt": a string replaces it,
    /// null suppresses it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The tools this component provides, as neutral declarations
    /// ({name, description, parameters, effects}). The kernel collects them
    /// (stamping each with its provider instance) and the loop offers the
    /// collected list to the model — declarations travel WITH their
    /// implementation, never hand-copied into assembly config. Inspection
    /// rejects duplicate tool names and providers no wire can reach.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
}

/// The resident dispatcher through which DEFERRED tools are invoked.
///
/// Providers only accept calls to tools declared in the request schema, so
/// a tool kept OUT of the schema (to preserve the prompt-cache prefix) needs
/// one stable doorway: this dispatcher is declared once, never changes, and
/// carries the deferred call inside its arguments. The loop unwraps such
/// calls mechanically. The model learns a deferred tool's name and shape
/// from the conversation itself (it usually built the tool).
pub const DEFERRED_DISPATCHER: &str = "UseDeferredTool";

/// The dispatcher's declaration — deliberately static: its text never
/// changes, so its presence never invalidates the schema prefix.
pub fn deferred_dispatcher_decl() -> Value {
    serde_json::json!({
        "name": DEFERRED_DISPATCHER,
        "description": "Invoke a deferred (recently installed) tool that is not \
            in this schema yet. Pass the tool's name and its arguments; you know \
            both from the conversation in which it was installed.",
        "parameters": {
            "type": "object",
            "properties": {
                "tool": {"type": "string"},
                "arguments": {"type": "object"},
            },
            "required": ["tool", "arguments"],
        },
        "effects": {"reversible": false},
    })
}
