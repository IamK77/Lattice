//! The frontend view model, as neutral data — no terminal library in sight.
//!
//! A frontend renders the same two things every time: a transcript of what has
//! happened, and the live state around it (what is being typed, whether a turn
//! is running). This module owns that model:
//!
//! - [`Entry`] and [`ToolCard`] — the transcript items, a richer counterpart to
//!   the minimal cross-language [`crate::render_line`] (which the Ink client
//!   mirrors byte-for-byte). Where `render_line` folds an event to one line,
//!   this keeps a tool call as a live card that flips from running to done, and
//!   marks a turn a timer or background job woke.
//! - [`ingest`] — the PURE fold from a ledger event into the transcript. It
//!   lives here, in the library, so it is unit-tested once and reused by any
//!   Rust frontend, rather than re-implemented inside each binary.
//! - [`View`] — the read-only interface a frontend's `draw` reads. The live app
//!   implements it; so could a replay of the ledger, or a per-stream selector.
//!   `draw` depends on this interface, not on any concrete state, which is what
//!   lets an event-sourced UI feed the same renderer from different sources.
//!
//! The display data is neutral and serializable (`serde`): it can cross
//! the same pure-data boundary as the rest of Lattice. The frontend maps these
//! types onto its own palette and layout — parse/model in the core, colors in
//! the frontend, the same discipline as [`crate::richtext`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::components::trust_policy;
use crate::contracts::core_events as ce;
use crate::contracts::event::EventEnvelope;

pub mod expert_panel;
pub mod facts;
pub mod history;
use crate::derived_pages as pages;
pub mod peaks;
mod transcript;
pub use transcript::{TranscriptGroup, TranscriptKind};

/// One item in the transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Entry {
    /// What the user typed
    User(String),
    /// A Markdown reply from the agent
    Agent(String),
    /// A tool call, as a card that moves from running to done
    Tool(ToolCard),
    /// What the model thought before it answered. Folded away by default like
    /// a long tool output — it is the road, not the destination — and keyed by
    /// the model call it belongs to so a frontend can unfold one at a time.
    Thinking(ThinkingCard),
    /// An error to surface
    Error(String),
    /// A background task or timer woke this turn (`source · summary`)
    Wake(String),
    /// A question the human must answer before the turn can go on: the trust
    /// gate holding an admission. Its own kind, not a notice, because a
    /// frontend must be able to make it LOUD — a question that reads like
    /// chatter is a question that gets scrolled past while the turn waits.
    Approval(String),
    /// A message the frontend itself produced — a `/help` listing, an "unknown
    /// command" reply. Not from the ledger, not the agent, never recorded; it
    /// is frontend chatter that disappears on `/clear`.
    Notice(String),
    /// A picture that came with the message above it, by the name the person
    /// gave it.
    ///
    /// Its own entry rather than a field on [`Entry::User`], so that a frontend
    /// which knows nothing about pictures keeps rendering the sentence exactly
    /// as it did — and so the transcript keeps saying what was sent after the
    /// input box has forgotten.
    Attachment(String),
}

/// A tool call as it moves from running to done.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCard {
    /// Provider call id, used to match the completion back to this card
    pub call: Option<String>,
    pub name: String,
    pub args: Value,
    pub status: ToolStatus,
    /// The finished output as lines (result, or the error message), capped;
    /// empty while running. A frontend shows the first line collapsed and the
    /// whole thing when expanded.
    pub output: Vec<String>,
    /// What this call CHANGED, if the tool said so — the path it wrote or
    /// edited. Reading tools leave this empty, and that emptiness is the
    /// useful part: a summary can say "changed nothing" and mean it, without
    /// knowing one tool name or guessing at one command's meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<String>,
    /// File context captured when an edit succeeded; absent on older ledgers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_diff: Option<crate::edit_diff::EditDiff>,
}

/// Material of one kind, and how much of it there is — bytes at the point of
/// measurement, tokens once a consumer has apportioned the provider's total.
pub type Material = Vec<(&'static str, u64)>;

/// One instance in the running assembly:
/// (instance name, component, how it runs, the tools it provides, whether it
/// may be removed, the wires it sits on).
pub type Assembled = (String, String, &'static str, String, bool, Vec<String>);

/// What one model call cost, already reconciled to one meaning across wires.
///
/// `prompt` is the WHOLE prompt including whatever was served from cache — the
/// only denominator a hit rate can honestly use. The wires disagree about that:
/// an OpenAI-shaped `prompt_tokens` already contains the cached part, while
/// Anthropic's `input_tokens` counts only what was not cached. Reconciling at
/// the point of reading means every consumer compares like with like.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt: u64,
    pub cached: u64,
    pub written: u64,
    pub output: u64,
    pub reasoning: u64,
    pub millis: u64,
    pub calls: u64,
}

impl Usage {
    /// How much of the prompt came from cache. `None` when nothing was sent —
    /// a rate over zero is not zero percent, it is no reading at all.
    pub fn hit_rate(&self) -> Option<f64> {
        (self.prompt > 0).then(|| self.cached as f64 / self.prompt as f64)
    }
    pub fn add(&mut self, other: &Usage) {
        self.prompt += other.prompt;
        self.cached += other.cached;
        self.written += other.written;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.millis += other.millis;
        self.calls += other.calls;
    }
}

/// The same question at three scopes, because "what is the cache hit rate"
/// means three different things: how the last prompt did, how this turn is
/// doing, and how the conversation has done overall.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageReport {
    pub call: Usage,
    pub turn: Usage,
    pub session: Usage,
}

/// What the effort palette needs to know: this model's own rungs, and where
/// the dial is currently set.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct EffortView {
    pub rungs: Vec<String>,
    pub now: Option<String>,
}

/// One model the person could switch to, with everything the choice turns on.
///
/// Not just a name: choosing a model is choosing an endpoint, a dialect, a
/// context budget and a key you either have or do not. A picker that showed
/// names alone would be asking someone to choose between things they cannot
/// tell apart.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRow {
    /// The short name it is chosen by
    pub id: String,
    /// The model identifier the endpoint will be asked for
    pub model: String,
    /// The endpoint's host, which is the readable part of a base URL
    pub endpoint: String,
    /// Which dialect speaks to it
    pub dialect: String,
    /// Its context window, when anything describes it
    pub window: Option<u64>,
    /// Its effort rungs — carried here rather than looked up by model name,
    /// because a model may describe itself in the catalog and a lookup would
    /// only find what the binary ships.
    pub rungs: Vec<String>,
    /// The environment variable its key lives in, and whether that variable is
    /// actually set right now
    pub key_env: String,
    pub key_present: bool,
    /// Whether it can read a picture. Carried here for the same reason `rungs`
    /// is: a model described in the user's own catalog may state this in its
    /// own profile, and a lookup by name would only find what the binary ships.
    pub accepts_images: bool,
}

/// Adding a model to the catalog, one field at a time.
///
/// A form rather than a line to type, for the same reason `/help` is a panel:
/// this asks for six things, and half of them are things you have to be TOLD
/// about — that the dialect is a fixed set, that the key can be written here
/// or named here and that those are different decisions.
#[derive(Default, Clone)]
pub struct ModelForm {
    pub values: [String; 6],
    pub at: usize,
    /// What was wrong with the last attempt to save. Kept on the form rather
    /// than flashed away, because the thing it is about is still on screen.
    pub problem: Option<String>,
}

impl ModelForm {
    /// Label, and what the field is for. The order is the order it is asked in.
    pub const FIELDS: [(&'static str, &'static str); 6] = [
        ("short name", "what you pick it by"),
        ("dialect", "openai · anthropic · responses"),
        ("model", "what the endpoint calls it"),
        ("endpoint", "https://…"),
        ("api key", "the key itself, kept in this file"),
        ("key variable", "the NAME of a variable holding it"),
    ];

    /// The entry as the catalog file spells it, or what is wrong with it.
    ///
    /// Checked here rather than at the next start, because a catalog is
    /// hand-shaped and the failure mode it must not have is "the model I added
    /// did not appear".
    pub fn entry(&self) -> Result<(String, serde_json::Value), String> {
        let [id, adapter, model, base_url, api_key, key_env] = &self.values;
        let (id, adapter, model) = (id.trim(), adapter.trim(), model.trim());
        if model.is_empty() {
            return Err("the endpoint has to be told which model to run".to_string());
        }
        if !["openai", "anthropic", "responses", "scripted"].contains(&adapter) {
            return Err(format!(
                "no dialect called {adapter:?} — use openai for Chat Completions, \
                 responses for Responses, or anthropic for Messages"
            ));
        }
        let (api_key, key_env) = (api_key.trim(), key_env.trim());
        if adapter != "scripted" {
            if base_url.trim().is_empty() {
                return Err("an endpoint with no address cannot be reached".to_string());
            }
            match (api_key.is_empty(), key_env.is_empty()) {
                (true, true) => {
                    return Err("fill in one of the two key fields: the key itself, or the \
                     name of a variable holding it"
                        .to_string())
                }
                (false, false) => {
                    return Err(
                        "fill in ONE of the two key fields — writing both leaves it \
                     unsaid which one is meant"
                            .to_string(),
                    )
                }
                _ => {}
            }
        }
        let mut spec = serde_json::json!({"adapter": adapter, "model": model});
        if !base_url.trim().is_empty() {
            spec["baseUrl"] = serde_json::json!(base_url.trim());
        }
        if !api_key.is_empty() {
            spec["apiKey"] = serde_json::json!(api_key);
        }
        if !key_env.is_empty() {
            spec["apiKeyEnv"] = serde_json::json!(key_env);
        }
        Ok((id.to_string(), spec))
    }
}

/// What the model picker draws: every configured model, and which one is
/// running.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct ModelView {
    pub rows: Vec<ModelRow>,
    /// Index of the model currently in force, when it is one of the rows
    pub now: Option<usize>,
}

impl ModelView {
    pub fn current(&self) -> Option<&ModelRow> {
        self.now.and_then(|at| self.rows.get(at))
    }
}

/// One turn's thinking, as a foldable card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThinkingCard {
    /// The model call event this thinking belongs to — the fold key
    pub call: String,
    /// The readable thinking, as lines. Sealed blocks contribute nothing:
    /// there is nothing in them to show.
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Background,
    Unknown,
    Ok,
    Failed,
    Cancelled,
}

/// The read-only interface a frontend's `draw` reads. Keeping `draw` behind an
/// interface (rather than a concrete state struct) is the render seam: the live
/// One thing happening away from the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    /// "expert" | "command" | "timer" | "watch"
    pub kind: &'static str,
    /// The key the ledger names it by, e.g. "expert:1"
    pub key: String,
    /// What it is, in the fewest words that identify it
    pub label: String,
    /// True for something that waits (a timer, a watch); false for something
    /// that is working (a command, an expert).
    pub standing: bool,
    /// How many times it has reported back
    pub fires: usize,
    /// The tick it started at, so the panel can say how long
    pub since: usize,
    /// Where its own record is, when it keeps one. An expert does; a command
    /// does not — which is why the two show different columns.
    pub ledger: Option<String>,
    /// Tool calls it has made, read from that record
    pub tools: usize,
    /// Tokens it has spent, read from that record
    pub tokens: u64,
    /// How much of the record has been read, so a growing file is only
    /// re-counted when it actually grew.
    pub read_len: u64,
}

/// A content address for scrolling, independent of terminal width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TranscriptBlock {
    Welcome,
    /// Stable display-card ordinal, not a ledger event sequence.
    Entries(usize),
    Streaming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptPosition {
    pub block: TranscriptBlock,
    pub line: usize,
    pub byte: usize,
}

/// A human choice, not an authorization grant or service decision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorizationChoice {
    Once,
    Flow,
    Permanent,
    #[default]
    Refuse,
}

/// A pending question and its local, non-persistent presentation state.
pub struct AuthorizationPrompt {
    pub request: String,
    pub description: String,
    pub choices: Vec<AuthorizationChoice>,
    pub selected: AuthorizationChoice,
    pub detail_line: usize,
}

/// Data-only projection of the live flow-grant manager, not permission authority.
#[derive(Default)]
pub struct GrantPanel {
    pub state: crate::components::operation_policy::GrantState,
    pub selected: Option<String>,
    pub confirming: Option<String>,
    pub pending: Option<String>,
    pub problem: Option<String>,
}

/// An app, a ledger replay, or a per-stream selector can feed the same draw.
pub trait View {
    /// The title bar text (model, endpoint, …)
    fn title(&self) -> &str;
    /// Conversation seats and their live state; empty outside a multi-seat UI.
    fn tabs(&self) -> &str {
        ""
    }
    /// Where the tools are working — the confined workspace, or the directory
    /// the run started in. Empty means there is nothing to say about it, which
    /// is what a replay or a headless view reports.
    fn workspace(&self) -> &str {
        ""
    }
    /// Which readouts the status bar carries, in order, by name. Empty leaves
    /// the choice to the frontend, which is what every view but a live TUI
    /// wants.
    fn status_bar(&self) -> &[String] {
        &[]
    }
    /// Resident entries for static views. Paged renderers use the group
    /// interface instead; a reader-backed view need not retain this slice.
    fn entries(&self) -> &[Entry];
    fn entry_count(&self) -> usize {
        self.entries().len()
    }
    /// Whether the final displayed card is a tool still running.
    fn last_tool_running(&self) -> bool {
        matches!(self.entries().last(), Some(Entry::Tool(card)) if matches!(card.status, ToolStatus::Running))
    }
    /// Load one complete display group, or the empty tail at `entry_count()`.
    fn transcript_group(&self, index: usize) -> std::io::Result<TranscriptGroup> {
        transcript::from_entries(self.entries(), index)
    }
    /// The in-progress reply text streaming in (empty when not streaming)
    fn streaming(&self) -> &str;
    /// The in-progress THINKING streaming in, shown above the reply while the
    /// model works. It has to be live: the finished thought only reaches the
    /// ledger when the whole call ends, so a frontend that waited for the
    /// event would always show the thinking after the answer it preceded.
    /// Default: empty — a frontend that does not surface thinking shows none.
    fn thinking(&self) -> &str {
        ""
    }
    /// What the user is currently typing, AS SHOWN.
    ///
    /// Borrowed unless the frontend is displaying something in place of what
    /// will be sent — a big paste folded to "[pasted 342 lines]", say. That is
    /// the whole reason this is not a `&str`: what is on screen and what goes
    /// out are allowed to differ, and a type that could only borrow forced them
    /// to be the same string.
    fn input(&self) -> std::borrow::Cow<'_, str>;
    /// Byte offset of the text cursor within `input()` — within what is SHOWN,
    /// so the caret lands where the character is. Defaults to the end: a
    /// frontend without cursor movement (or a ledger replay) need not track it.
    fn cursor(&self) -> usize {
        self.input().len()
    }
    /// Whether a turn is running (input disabled, spinner shown)
    fn busy(&self) -> bool;
    /// The loop yielded for external progress; this is not a completed turn.
    fn waiting(&self) -> bool {
        false
    }
    /// A monotonically advancing counter that drives animations (the spinner)
    fn tick(&self) -> usize;
    /// How many lines the transcript is scrolled up from the bottom (0 = pinned
    /// to the latest). Defaults to pinned — a replay or a static view need not
    /// track it.
    fn scroll(&self) -> usize {
        0
    }
    /// A stable content location plus a pending screen-row movement (down is
    /// positive). Absent locations retain the distance-from-bottom interface.
    fn transcript_position(&self) -> Option<(TranscriptPosition, isize)> {
        None
    }
    /// Which item is highlighted in the input's completion menu (the slash
    /// candidate list). Defaults to the first — a frontend without a selectable
    /// menu (or a ledger replay) need not implement it.
    fn hint_sel(&self) -> usize {
        0
    }
    /// Whether the tool card with this call id is expanded (its full output
    /// shown). Defaults to collapsed.
    fn tool_expanded(&self, _call: Option<&str>) -> bool {
        false
    }
    /// The `tick` at which the last turn finished, if one has — used to animate
    /// the "thinking" line settling into "Done" after a reply lands. `None`
    /// before any turn completes (or while one is running). Defaults to `None`:
    /// a replay or a static view shows no such line.
    /// An open authorization request awaiting a human decision: its event id.
    fn pending_auth(&self) -> Option<&str> {
        None
    }
    fn grant_panel(&self) -> Option<&GrantPanel> {
        None
    }
    /// Current live binding only. Historical permission never lights this indicator.
    fn interface_permission(&self) -> bool {
        false
    }
    /// Resolve the currently answerable question, never a newer transcript card.
    fn authorization_prompt(&self) -> std::io::Result<Option<AuthorizationPrompt>> {
        Ok(self.pending_auth().map(|request| AuthorizationPrompt {
            request: request.to_owned(),
            description: String::new(),
            choices: vec![AuthorizationChoice::Once, AuthorizationChoice::Refuse],
            selected: AuthorizationChoice::Refuse,
            detail_line: 0,
        }))
    }
    /// Lines the user has said that the model has NOT been shown yet — typed
    /// while a question was already out, and riding along on the next one.
    /// They are on the ledger (they were really said), so the transcript
    /// carries them; this is the separate fact that they have not landed in
    /// front of the model yet, which only a live view can know. Default: none.
    fn unseen(&self) -> &[String] {
        &[]
    }
    /// The frontend's ACKNOWLEDGEMENT of the command just typed — "thinking set
    /// to high", "unknown command". It rides beside the input box and goes away
    /// on the next keystroke or the next turn.
    ///
    /// Separate from the transcript because it is not part of the conversation:
    /// as an ordinary entry it sat flush against the end of the agent's answer
    /// and read as its last sentence. A LISTING is different — `/help`, the
    /// effort ladder — those you may want to scroll back to, so they stay
    /// entries. Default: none.
    fn flash(&self) -> Option<&str> {
        None
    }
    /// The reference panel, when open: which panel, and which of its tabs.
    ///
    /// A frontend command that only shows you something should SHOW it, not
    /// narrate it into the conversation — the transcript is the record of what
    /// was said, and a keyboard map is not something anyone said. Default: closed.
    fn panel(&self) -> Option<(usize, usize)> {
        None
    }
    /// Which row the panel has highlighted. Default: the first.
    fn panel_sel(&self) -> usize {
        0
    }
    /// How far the panel body is scrolled, in lines. Default: the top.
    fn panel_scroll(&self) -> usize {
        0
    }
    /// Whether the highlighted row's detail is unfolded. Default: folded.
    fn panel_open(&self) -> bool {
        false
    }
    /// What the last foreground model call cost, in tokens: input, output,
    /// cache read, cache write. Off the ledger, not estimated — a context
    /// reading that guessed would be confidently wrong exactly where it
    /// matters, near the ceiling. Default: nothing measured yet.
    fn usage(&self) -> Option<UsageReport> {
        None
    }
    /// What the last prompt was made of, as (kind, bytes), largest first.
    ///
    /// BYTES, because that is what can be measured here: the exact token total
    /// comes from the provider and there is no tokenizer to attribute it per
    /// part. A consumer that wants tokens apportions the real total by these
    /// shares — and should say that it did. Default: nothing measured.
    fn composition(&self) -> Material {
        Vec::new()
    }
    /// What is assembled right now: (instance, component, how it runs, the
    /// tools it provides, whether it can be removed, the wires it sits on).
    ///
    /// Removable is not cosmetic — only what an overlay installed may be taken
    /// out, or "uninstall the thing called trust" would dismantle the gate that
    /// is vetting the request. Default: nothing reported.
    fn components(&self) -> Vec<Assembled> {
        Vec::new()
    }
    /// What each kind of material has ADDED — this turn, and over the whole
    /// conversation. Snapshots cannot be summed (the same tool result is in
    /// every later prompt); growth can, and it survives condensing.
    fn growth(&self) -> (Material, Material) {
        (Vec::new(), Vec::new())
    }
    /// Resident peak values for static views. Paged views provide the exact
    /// display summary instead of materializing their whole history.
    fn history(&self) -> Vec<u64> {
        Vec::new()
    }
    fn history_growth(&self) -> peaks::GrowthSummary {
        peaks::GrowthSummary::from_values(&self.history())
    }
    /// The invokable-skills menu (name + description), folded off the
    /// ledger's skill.listing events — feeds the `/` palette. Default: none
    /// (a frontend without a palette shows commands only).
    /// What is running or armed away from this conversation.
    ///
    /// Two different things, deliberately kept apart. Work IN FLIGHT (a
    /// background command, an expert) is spending money right now and ends
    /// when it answers. A STANDING arrangement (a timer, a watch) is not
    /// running at all — it is waiting, and it ends when it is cancelled or
    /// runs out its fires. Counting them together would report a timer that
    /// sleeps all day as though something were working.
    fn background(&self) -> &[Live] {
        &[]
    }

    fn skills(&self) -> &[(String, String)] {
        &[]
    }

    /// This model's effort rungs and where the dial sits. Default: nothing
    /// known, which a palette draws as "no shape to show" rather than as a
    /// model with no settings.
    fn effort(&self) -> EffortView {
        EffortView::default()
    }

    /// The effort dial while it is open, holding its cursor: 0 is `off`, then
    /// one position per rung. `None` — the usual state — means no dial, and
    /// the input box is an input box. It is a mode rather than a candidate
    /// list, so it keeps its own place and its own keys.
    fn dial(&self) -> Option<usize> {
        None
    }

    /// The configured models and which one is running. Default: nothing
    /// known, which the picker draws as "nothing configured" rather than as
    /// an installation with no models.
    fn models(&self) -> ModelView {
        ModelView::default()
    }

    /// None means this view has not loaded a compaction observation.
    fn compaction_status(&self) -> Option<&crate::components::context_gate::CompactionStatus> {
        None
    }

    /// The add-a-model form while it is open. `None` — the usual state —
    /// means the model panel is a list.
    fn model_form(&self) -> Option<ModelForm> {
        None
    }

    /// Frontend-owned expert management state, not conversation material.
    fn expert_panel(&self) -> Option<expert_panel::Panel> {
        None
    }

    /// A deletion waiting to be confirmed, named so the question can say what
    /// it is about to destroy.
    fn confirm_delete(&self) -> Option<String> {
        None
    }

    /// The model picker while it is open, holding its cursor — one position
    /// per configured model. `None`, the usual state, means no picker and the
    /// input box is an input box. A mode rather than a candidate list, like
    /// the effort dial, and for the same reason: it has its own keys and its
    /// own place on the screen.
    fn picker(&self) -> Option<usize> {
        None
    }

    fn done_at(&self) -> Option<usize> {
        None
    }
    /// How many turns have started — indexes the thinking/working phrase cast so
    /// each turn keeps one phrase for its duration. Defaults to 0.
    fn turn(&self) -> usize {
        0
    }
}

/// Fold one ledger event into the transcript, returning true when it changed
/// the view (so the caller can clear its live-streaming buffer). Pure: the only
/// effect is on the `entries` it is handed.
pub fn ingest(entries: &mut Vec<Entry>, event: &EventEnvelope) -> bool {
    match event.event_type.as_str() {
        ce::USER_MESSAGE => {
            // A CAUSED user message is a re-emission — the expansion
            // station's forward of what the user typed. The causeless
            // original is the one the human should see; rendering both
            // would double every line (and print expanded skill bodies).
            if !event.causes.is_empty() {
                return false;
            }
            if let Some(text) = event.payload["text"].as_str() {
                entries.push(Entry::User(text.to_string()));
                // Beneath the sentence they came with, in the order they were
                // attached — the same order the model was shown them in.
                for image in event.payload["images"].as_array().unwrap_or(&Vec::new()) {
                    let name = image["name"]
                        .as_str()
                        .or_else(|| image["file"].as_str())
                        .unwrap_or("image");
                    entries.push(Entry::Attachment(name.to_string()));
                }
                return true;
            }
        }
        ce::WAKE => {
            if command_wake(entries, &event.payload) {
                return true;
            }
            let source = event.payload["source"].as_str().unwrap_or("wake");
            let text = match event.payload["summary"].as_str() {
                Some(s) => format!("{source} · {s}"),
                None => source.to_string(),
            };
            entries.push(Entry::Wake(text));
            return true;
        }
        ce::OUTPUT_REPLY => {
            if let Some(text) = event.payload["text"].as_str() {
                entries.push(Entry::Agent(text.to_string()));
                return true;
            } else if event.payload["cancelled"] == true {
                entries.push(Entry::Agent("[interrupted]".to_string()));
                return true;
            } else if let Some(msg) = event.payload["error"]["message"].as_str() {
                entries.push(Entry::Error(msg.to_string()));
                return true;
            }
        }
        ce::MODEL_CALL_COMPLETED => {
            // A background call (the condenser) is not the conversation and
            // its thinking is nobody's business but its own.
            if event.payload.get("purpose").is_some() {
                return false;
            }
            let mut shown = false;
            let thinking = ce::reasoning_text(&event.payload);
            if !thinking.trim().is_empty() {
                entries.push(Entry::Thinking(ThinkingCard {
                    call: event.id.clone(),
                    lines: thinking.lines().map(str::to_string).collect(),
                }));
                shown = true;
            }
            // What the agent SAID on the way to calling a tool. It is already
            // on the ledger and the model already reads it back — but only the
            // final answer of a turn becomes a reply event, so everything said
            // in between never reached the screen. The reader watched it
            // stream in and then watched it vanish when the tool card landed.
            let mid_turn = event.payload["toolCalls"]
                .as_array()
                .is_some_and(|calls| !calls.is_empty());
            if mid_turn {
                if let Some(text) = event.payload["text"].as_str() {
                    if !text.trim().is_empty() {
                        entries.push(Entry::Agent(text.to_string()));
                        shown = true;
                    }
                }
            }
            return shown;
        }
        ce::TOOL_EXEC_STARTED => {
            // A gated assembly records one request twice — the loop's
            // emission and the gate's forward, same call id. One card is
            // the truth; a second would spin forever (only one completion).
            let call = event.payload["call"].as_str();
            let already_running = call.is_some()
                && entries.iter().any(|e| {
                    matches!(e, Entry::Tool(card)
                        if card.status == ToolStatus::Running && card.call.as_deref() == call)
                });
            if already_running {
                return false;
            }
            entries.push(Entry::Tool(ToolCard {
                call: call.map(str::to_string),
                name: event.payload["tool"].as_str().unwrap_or("?").to_string(),
                args: event.payload["arguments"].clone(),
                status: ToolStatus::Running,
                output: Vec::new(),
                changed: None,
                edit_diff: None,
            }));
            return true;
        }
        ce::TOOL_EXEC_COMPLETED => {
            let call = event.payload["call"].as_str();
            let status = match event.payload["status"].as_str() {
                Some("ok") => ToolStatus::Ok,
                Some("cancelled") => ToolStatus::Cancelled,
                _ => ToolStatus::Failed,
            };
            let output = tool_output(&event.payload);
            // Flip the latest still-running card, preferring an id match
            for entry in entries.iter_mut().rev() {
                if let Entry::Tool(card) = entry {
                    if card.status != ToolStatus::Running {
                        continue;
                    }
                    let id_match = match (call, card.call.as_deref()) {
                        (Some(a), Some(b)) => a == b,
                        _ => true,
                    };
                    if id_match {
                        card.edit_diff = if status == ToolStatus::Ok {
                            serde_json::from_value(event.payload["result"]["editDiff"].clone()).ok()
                        } else {
                            None
                        };
                        if status == ToolStatus::Ok
                            && card.name == "Run"
                            && event.payload["result"]["background"] == true
                        {
                            card.status = ToolStatus::Background;
                            let pid = event.payload["result"]["pid"].as_u64();
                            card.output = vec![match pid {
                                Some(pid) => format!("Running in background · pid {pid}"),
                                None => "Running in background".to_string(),
                            }];
                        } else {
                            card.status = status;
                            card.output = output;
                        }
                        card.changed = event.payload["result"]["changed"]
                            .as_str()
                            .map(str::to_string);
                        return true;
                    }
                }
            }
        }
        t if t == crate::components::interface_permissions::STATE => {
            // Successful state changes belong in the live status indicator,
            // not in the conversation. The original events remain auditable.
            if event.payload["accepted"] == false {
                entries.push(Entry::Notice(format!(
                    "Permission change refused: {}",
                    event.payload["error"].as_str().unwrap_or("see ledger")
                )));
                return true;
            }
        }
        t if t == crate::components::operation_policy::STATE && !event.causes.is_empty() => {
            entries.push(Entry::Notice(format!(
                "{} — /grants to inspect",
                event.reason.as_deref().unwrap_or("Flow grants changed")
            )));
            return true;
        }
        t if t == crate::components::operation_policy::AUTH_REQUESTED
            || t == trust_policy::AUTH_REQUESTED
            || t == crate::components::browser_tools::AUTH_REQUESTED
            || t == crate::components::expert_definitions::AUTH_REQUESTED =>
        {
            entries.push(Entry::Approval(approval_text(&event.payload)));
            return true;
        }
        t if t == crate::components::browser_tools::DECISION => {
            entries.push(Entry::Notice(format!(
                "browser: {}",
                event.reason.as_deref().unwrap_or("")
            )));
            return true;
        }
        t if t == trust_policy::DECISION => {
            let granted = event.payload["verdict"] == "granted";
            let why = event.reason.as_deref().unwrap_or("");
            entries.push(Entry::Notice(format!(
                "{} trust: {}",
                if granted { "✓" } else { "✗" },
                why
            )));
            return true;
        }
        _ => {}
    }
    false
}

/// Recognize a command completion before looking up its historical card.
fn command_job(payload: &Value) -> Option<&str> {
    let body = &payload["body"];
    let job = body["job"].as_str()?;
    (payload["source"].as_str() == Some(format!("background:{job}").as_str())
        && (body["exit_code"].is_i64()
            || body["interrupted"] == "restart"
            || body["error"].is_string()))
    .then_some(job)
}

/// Fold process completion into its command card without rewriting the ledger.
/// An early wake can settle a running card before its handoff receipt arrives.
fn command_wake(entries: &mut [Entry], payload: &Value) -> bool {
    let Some(job) = command_job(payload) else {
        return false;
    };
    let body = &payload["body"];
    let Some(card) = entries.iter_mut().rev().find_map(|entry| match entry {
        Entry::Tool(card) if card.name == "Run" && card.call.as_deref() == Some(job) => Some(card),
        _ => None,
    }) else {
        // A partial history may not contain the request. Keep its notification.
        return false;
    };
    if !matches!(
        card.status,
        ToolStatus::Running | ToolStatus::Background | ToolStatus::Unknown
    ) {
        return true;
    }
    let code = body["exit_code"].as_i64();
    card.status = match code {
        Some(0) => ToolStatus::Ok,
        Some(_) => ToolStatus::Failed,
        None => ToolStatus::Unknown,
    };
    let mut text = match code {
        Some(0) => "Completed · exit 0".to_string(),
        Some(code) => format!("Failed · exit {code}"),
        None if body["interrupted"] == "restart" => {
            "Outcome unknown after restart · command was not rerun".to_string()
        }
        None => "Outcome unknown".to_string(),
    };
    if ["stdout", "stderr"]
        .iter()
        .any(|key| body[key].as_str().is_some_and(|text| !text.is_empty()))
    {
        let output = readable(body);
        if !output.is_empty() {
            text.push('\n');
            text.push_str(&output);
        }
    }
    for key in ["error", "note"] {
        if let Some(detail) = body[key].as_str() {
            text.push('\n');
            text.push_str(detail);
        }
    }
    card.output = tool_output(&serde_json::json!({"result": text}));
    true
}

/// Bound both line length and line count, including background results.
fn tool_output(payload: &Value) -> Vec<String> {
    const MAX_LINES: usize = 200;
    const MAX_COLS: usize = 400;
    let text = if let Some(msg) = payload["error"]["message"].as_str() {
        msg.to_string()
    } else if payload["result"].is_null() {
        return Vec::new();
    } else {
        readable(&payload["result"])
    };
    text.lines()
        .map(|l| {
            let l = l.replace('\t', "    ");
            if l.chars().count() > MAX_COLS {
                let head: String = l.chars().take(MAX_COLS - 1).collect();
                format!("{head}…")
            } else {
                l
            }
        })
        .take(MAX_LINES)
        .collect()
}

/// What is asking, for what, and what it would then be able to do.
///
/// "Authorization required" alone tells the reader nothing they can decide on.
/// The three things that matter are the ASKER (which tool), the SUBJECT (which
/// source, which component), and the POWER being granted (the effect surface
/// it declared) — a source URL with no idea what it may then do is not a
/// question anybody can answer well.
fn approval_text(payload: &Value) -> String {
    format!(
        "{}\n    y = allow · n = refuse",
        authorization_description(payload)
    )
}

/// Request details without a frontend-specific keyboard hint.
pub fn authorization_description(payload: &Value) -> String {
    let tool = payload["tool"].as_str().unwrap_or("something");
    let summary = payload["summary"].as_str().unwrap_or_default();
    if let Some(grants) = payload["grants"].as_array() {
        let mut details = format!("Approve {tool} operation\n    {summary}");
        if grants.is_empty() {
            details.push_str("\n    This configured rule requires approval each time.");
        } else {
            details.push_str("\n    Proposed flow grant (survives reopening):");
            for grant in grants {
                match grant["kind"].as_str() {
                    Some("command_prefix") => details.push_str(&format!(
                        "\n    {} argv prefix: {}",
                        grant["tool"].as_str().unwrap_or(tool),
                        grant["prefix"]
                    )),
                    Some("exact_arguments") => {
                        details.push_str("\n    Exact arguments of this request only")
                    }
                    _ => details.push_str(&format!("\n    {grant}")),
                }
            }
            details.push_str("\n    Prefixes do not restrict trailing arguments, directory, remote identity, or executable contents.");
        }
        return details;
    }
    if payload["confirmation"] == "expert-delete" {
        return format!("Confirm expert deletion\n    {summary}");
    }
    // The summary usually reads "tool: subject"; the tool is named already
    let subject = summary
        .strip_prefix(tool)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(summary)
        .trim();
    let admits = payload["admits"].as_str().unwrap_or("");
    let mut what = format!(
        "{tool} is asking to admit new {}",
        if admits.is_empty() { "code" } else { admits }
    );
    if !subject.is_empty() {
        what.push_str(&format!("\n    {subject}"));
    }
    let effects = &payload["effects"];
    let mut powers: Vec<&str> = Vec::new();
    if effects["executes"] == true {
        powers.push("run programs");
    }
    for (key, label) in [
        ("writes", "write files"),
        ("network", "reach the network"),
        ("reads", "read files"),
    ] {
        if effects[key].as_array().is_some_and(|list| !list.is_empty()) {
            powers.push(label);
        }
    }
    if !powers.is_empty() {
        what.push_str(&format!("\n    once in, it may {}", powers.join(", ")));
    }
    what
}

/// Turn a tool's structured result into human-readable text: a string as-is
/// (real newlines), a scalar as its value, and an object by pulling out the
/// field a human wants to read — a shell's `stdout` (+`stderr`), a file's
/// `content`, a page's `body`, a listing's `entries` — falling back to pretty
/// JSON when the shape is unfamiliar. This keeps the transcript readable instead
/// of dumping `{"exit_code":0,"stdout":"…\n…"}` with escaped newlines.
fn readable(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Object(map) => {
            let field = |k: &str| map.get(k).and_then(Value::as_str).unwrap_or("");
            if map.contains_key("stdout") || map.contains_key("stderr") {
                let mut text = field("stdout").to_string();
                let err = field("stderr");
                if !err.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(err);
                }
                if text.is_empty() {
                    if let Some(code) = map.get("exit_code").and_then(Value::as_i64) {
                        text = format!("(exit {code}, no output)");
                    }
                }
                return text;
            }
            // `preview` first: a tool that took the trouble to say what its
            // change LOOKED like has said the most useful thing it can, and
            // the alternative here is printing its bookkeeping as JSON.
            for key in ["preview", "content", "body", "output", "text"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    return s.to_string();
                }
            }
            if let Some(entries) = map.get("entries").and_then(Value::as_array) {
                return entries
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
        }
        _ => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal envelope; `ingest` only reads `type` and `payload`.
    fn ev(event_type: &str, payload: Value) -> EventEnvelope {
        EventEnvelope {
            v: 1,
            id: "e1".to_string(),
            seq: 1,
            stream: "s".to_string(),
            time: "t".to_string(),
            event_type: event_type.to_string(),
            source: "src".to_string(),
            causes: vec![],
            origin: None,
            reason: None,
            payload,
        }
    }

    #[test]
    fn a_gated_double_start_makes_one_card_and_one_completion_settles_it() {
        let mut entries = Vec::new();
        // The loop's emission and the gate's forward: same call, twice
        let start = json!({"call": "c1", "tool": "Run", "arguments": {"command": "ls"}});
        ingest(&mut entries, &ev(ce::TOOL_EXEC_STARTED, start.clone()));
        ingest(&mut entries, &ev(ce::TOOL_EXEC_STARTED, start));
        let cards = entries
            .iter()
            .filter(|e| matches!(e, Entry::Tool(_)))
            .count();
        assert_eq!(
            cards, 1,
            "one call, one card — the forward is the same call"
        );
        ingest(
            &mut entries,
            &ev(
                ce::TOOL_EXEC_COMPLETED,
                json!({"call": "c1", "status": "ok", "result": "x"}),
            ),
        );
        assert!(
            entries
                .iter()
                .all(|e| !matches!(e, Entry::Tool(c) if c.status == ToolStatus::Running)),
            "nothing keeps spinning after the completion"
        );
    }

    #[test]
    fn operation_details_show_the_proposed_scope_and_its_limits() {
        let details =
            authorization_description(&json!({"tool":"Run", "summary":"git push origin main",
            "grants":[{"kind":"command_prefix","tool":"Run","prefix":["git","push","origin"]}]}));
        assert!(details.contains("[\"git\",\"push\",\"origin\"]"));
        assert!(details.contains("survives reopening"));
        assert!(details.contains("do not restrict trailing arguments"));
        assert!(!details.contains("admit new"));
        let forced = authorization_description(&json!({"tool":"Run","grants":[]}));
        assert!(forced.contains("requires approval each time"));
        assert!(!forced.contains("Proposed flow grant"));
    }

    #[test]
    fn operation_questions_are_visible_as_approval_cards() {
        let mut entries = Vec::new();
        let question = ev(
            crate::components::operation_policy::AUTH_REQUESTED,
            json!({"tool":"Run","summary":"Execute git push origin main","grants":[]}),
        );
        assert!(ingest(&mut entries, &question));
        assert!(
            matches!(entries.first(), Some(Entry::Approval(text)) if text.contains("git push origin main"))
        );
        assert!(crate::session::render_line(&question).is_some());
    }

    #[test]
    fn an_approval_question_says_who_asks_for_what_and_what_it_may_then_do() {
        let mut entries = Vec::new();
        ingest(
            &mut entries,
            &ev(
                trust_policy::AUTH_REQUESTED,
                json!({"request": "e0", "key": "k", "tool": "InstallComponentFrom",
                       "admits": "components",
                       "summary": "install_component_from: https://example.com/thing",
                       "effects": {"executes": true, "writes": ["*"], "admits": "components"}}),
            ),
        );
        // Its own kind, so a frontend can make it loud — a question styled as
        // chatter is a question that gets scrolled past while the turn waits
        let Some(Entry::Approval(text)) = entries.first() else {
            panic!("expected an approval question, got {entries:?}");
        };
        assert!(
            text.contains("install_component_from"),
            "who is asking: {text}"
        );
        assert!(text.contains("new components"), "what it admits: {text}");
        assert!(
            text.contains("https://example.com/thing"),
            "the subject: {text}"
        );
        assert!(
            text.contains("run programs") && text.contains("write files"),
            "the power being granted — the part a source URL alone cannot tell you: {text}"
        );
        assert!(text.contains("y = allow"), "and how to answer: {text}");

        let mut decision = ev(
            trust_policy::DECISION,
            json!({"verdict": "granted", "key": "k"}),
        );
        decision.reason = Some("the user authorized this admission".to_string());
        ingest(&mut entries, &decision);
        assert!(matches!(entries[1], Entry::Notice(ref t) if t.contains("✓")));
    }

    /// A turn that calls tools says things along the way — "let me check the
    /// config first" — and only its LAST words became a reply event. The rest
    /// was on the ledger, and the model read it back, but the human watched it
    /// stream in and then watched it vanish the moment the tool card landed.
    #[test]
    fn what_the_agent_says_on_the_way_to_a_tool_is_not_swallowed() {
        let mut entries = Vec::new();
        let mid = ev(
            ce::MODEL_CALL_COMPLETED,
            json!({"status": "ok", "text": "let me look at the config first",
                   "toolCalls": [{"id": "c1", "tool": "Read", "arguments": {}}]}),
        );
        assert!(ingest(&mut entries, &mid));
        assert_eq!(
            entries,
            vec![Entry::Agent("let me look at the config first".to_string())]
        );

        // The FINAL words still arrive as a reply, and must not be doubled:
        // a completion with no tool calls is the turn's answer, and the loop
        // turns that one into a reply event of its own.
        let mut entries = Vec::new();
        let last = ev(
            ce::MODEL_CALL_COMPLETED,
            json!({"status": "ok", "text": "here is what I found"}),
        );
        assert!(!ingest(&mut entries, &last), "no entry from the final call");
        assert!(ingest(
            &mut entries,
            &ev(ce::OUTPUT_REPLY, json!({"text": "here is what I found"}))
        ));
        assert_eq!(
            entries,
            vec![Entry::Agent("here is what I found".to_string())],
            "said once, not twice"
        );
    }

    /// Thinking becomes its own card, keyed by the call it belongs to, and
    /// carries only what is readable — a sealed block has nothing to show.
    /// A background call's thinking is not the conversation's and never
    /// appears; a call that did not think leaves no card at all.
    #[test]
    fn thinking_becomes_a_card_unless_it_is_background_or_absent() {
        let mut entries = Vec::new();
        let thought = ev(
            ce::MODEL_CALL_COMPLETED,
            json!({"status": "ok", "text": "11", "reasoning": [
                {"kind": "text", "text": "four plus seven\nis eleven"},
                {"kind": "hidden", "opaque": {"data": "sealed"}},
            ]}),
        );
        assert!(ingest(&mut entries, &thought));
        assert_eq!(
            entries,
            vec![Entry::Thinking(ThinkingCard {
                call: thought.id.clone(),
                lines: vec!["four plus seven".to_string(), "is eleven".to_string()],
            })]
        );

        let mut entries = Vec::new();
        assert!(!ingest(
            &mut entries,
            &ev(
                ce::MODEL_CALL_COMPLETED,
                json!({"status": "ok", "purpose": "context.condense",
                       "reasoning": [{"kind": "text", "text": "summarizing"}]}),
            )
        ));
        assert!(!ingest(
            &mut entries,
            &ev(
                ce::MODEL_CALL_COMPLETED,
                json!({"status": "ok", "text": "hi"})
            )
        ));
        assert!(entries.is_empty());
    }

    #[test]
    fn a_user_message_becomes_a_user_entry() {
        let mut entries = Vec::new();
        let changed = ingest(&mut entries, &ev(ce::USER_MESSAGE, json!({"text": "hi"})));
        assert!(changed);
        assert_eq!(entries, vec![Entry::User("hi".to_string())]);
    }

    /// A CAUSED user message is a station's re-emission (the skill expansion
    /// forward) — rendering it would double every line and print expanded
    /// bodies. Only the causeless typed original shows.
    #[test]
    fn a_forwarded_user_message_renders_nothing() {
        let mut entries = Vec::new();
        let mut forwarded = ev(ce::USER_MESSAGE, json!({"text": "expanded skill body"}));
        forwarded.causes = vec!["ev_1_original".to_string()];
        let changed = ingest(&mut entries, &forwarded);
        assert!(!changed);
        assert!(entries.is_empty(), "{entries:?}");
    }

    #[test]
    fn background_command_cards_replace_receipts_and_notifications_with_results() {
        let start = ev(
            ce::TOOL_EXEC_STARTED,
            json!({"call":"job", "tool":"Run", "arguments":{"command":"build"}}),
        );
        let ack = ev(
            ce::TOOL_EXEC_COMPLETED,
            json!({"call":"job", "status":"ok", "result":{"background":true,"pid":123,"job":"job"}}),
        );
        for (body, status, expected) in [
            (
                json!({"job":"job","exit_code":0,"stdout":"built\n","stderr":""}),
                ToolStatus::Ok,
                "Completed · exit 0",
            ),
            (
                json!({"job":"job","exit_code":7,"stdout":"","stderr":"bad build"}),
                ToolStatus::Failed,
                "Failed · exit 7",
            ),
            (
                json!({"job":"job","interrupted":"restart"}),
                ToolStatus::Unknown,
                "Outcome unknown after restart",
            ),
            (
                json!({"job":"job","error":"wait failed"}),
                ToolStatus::Unknown,
                "wait failed",
            ),
        ] {
            let wake = ev(
                ce::WAKE,
                json!({"source":"background:job","summary":"verbose internal notification", "body":body}),
            );
            let mut entries = Vec::new();
            ingest(&mut entries, &start);
            ingest(&mut entries, &ack);
            let Entry::Tool(card) = &entries[0] else {
                panic!("missing card")
            };
            assert_eq!(card.status, ToolStatus::Background);
            assert_eq!(card.output, ["Running in background · pid 123"]);
            ingest(&mut entries, &wake);
            assert_eq!(
                entries.len(),
                1,
                "completion must not add a duplicate notice"
            );
            let Entry::Tool(card) = &entries[0] else {
                panic!("missing card")
            };
            assert_eq!(card.status, status);
            assert!(card.output.join("\n").contains(expected));
            if body["exit_code"] == 0 {
                assert!(card.output.iter().any(|line| line == "built"));
            }
            if body["exit_code"] == 7 {
                assert!(card.output.iter().any(|line| line == "bad build"));
            }
            let final_output = card.output.clone();
            ingest(&mut entries, &ack);
            ingest(&mut entries, &wake);
            let Entry::Tool(card) = &entries[0] else {
                panic!("missing card")
            };
            assert_eq!(card.output, final_output);
            assert_eq!(entries.len(), 1);

            let mut early = Vec::new();
            for event in [&start, &wake, &ack] {
                ingest(&mut early, event);
            }
            let Entry::Tool(card) = &early[0] else {
                panic!("missing card")
            };
            assert_eq!(card.status, status);
            assert_eq!(
                card.output, final_output,
                "late receipt must not replace completion"
            );
            assert_eq!(early.len(), 1);
        }
    }

    #[test]
    fn background_cards_do_not_swallow_progress_or_other_sources_and_empty_results_are_concise() {
        let mut entries = Vec::new();
        ingest(
            &mut entries,
            &ev(ce::TOOL_EXEC_STARTED, json!({"call":"job","tool":"Run"})),
        );
        for payload in [
            json!({"source":"background:job","summary":"progress", "body":{"job":"job","stdout":"still running"}}),
            json!({"source":"expert:job","summary":"expert finished", "body":{"job":"job","exit_code":0}}),
        ] {
            ingest(&mut entries, &ev(ce::WAKE, payload));
            assert!(matches!(&entries[0], Entry::Tool(card) if card.status == ToolStatus::Running));
            assert!(matches!(entries.last(), Some(Entry::Wake(_))));
        }
        ingest(
            &mut entries,
            &ev(
                ce::WAKE,
                json!({"source":"background:job","body":{"job":"job","exit_code":0,"stdout":"","stderr":""}}),
            ),
        );
        let Entry::Tool(card) = &entries[0] else {
            panic!("missing card")
        };
        assert_eq!(card.output, ["Completed · exit 0"]);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn unmatched_background_wakes_remain_visible_and_output_is_bounded() {
        let wake = ev(
            ce::WAKE,
            json!({"source":"background:job","summary":"finished", "body":{"job":"job","exit_code":0,"stdout":"界".repeat(500)+"\n"+&"line\n".repeat(300)}}),
        );
        let mut entries = Vec::new();
        ingest(&mut entries, &wake);
        assert!(matches!(&entries[0], Entry::Wake(text) if text.contains("finished")));
        entries.clear();
        ingest(
            &mut entries,
            &ev(ce::TOOL_EXEC_STARTED, json!({"call":"job","tool":"Run"})),
        );
        ingest(&mut entries, &wake);
        let Entry::Tool(card) = &entries[0] else {
            panic!("missing card")
        };
        assert_eq!(card.output.len(), 200);
        assert!(card.output.iter().all(|line| line.chars().count() <= 400));
        assert!(card.output[1].ends_with('…'));
    }

    #[test]
    fn a_wake_shows_source_and_summary() {
        let mut entries = Vec::new();
        ingest(
            &mut entries,
            &ev(
                ce::WAKE,
                json!({"source": "timer", "summary": "reminder #1"}),
            ),
        );
        assert_eq!(
            entries,
            vec![Entry::Wake("timer · reminder #1".to_string())]
        );
    }

    #[test]
    fn a_completion_flips_the_matching_running_card() {
        let mut entries = Vec::new();
        ingest(
            &mut entries,
            &ev(
                ce::TOOL_EXEC_STARTED,
                json!({"call": "c1", "tool": "read_file", "arguments": {"path": "a.rs"}}),
            ),
        );
        assert!(matches!(
            &entries[0],
            Entry::Tool(c) if c.status == ToolStatus::Running && c.output.is_empty()
        ));

        let changed = ingest(
            &mut entries,
            &ev(
                ce::TOOL_EXEC_COMPLETED,
                json!({"call": "c1", "status": "ok", "result": "fn main() {}\nsecond line"}),
            ),
        );
        assert!(changed);
        let Entry::Tool(card) = &entries[0] else {
            panic!("expected a tool card");
        };
        assert_eq!(card.status, ToolStatus::Ok);
        assert_eq!(card.output, vec!["fn main() {}", "second line"]);
    }

    #[test]
    fn a_shell_result_shows_stdout_and_stderr_as_lines() {
        let mut entries = Vec::new();
        ingest(
            &mut entries,
            &ev(ce::TOOL_EXEC_STARTED, json!({"call": "c1", "tool": "Run"})),
        );
        ingest(
            &mut entries,
            &ev(
                ce::TOOL_EXEC_COMPLETED,
                json!({"call": "c1", "status": "ok", "result": {
                    "exit_code": 0, "stdout": "line one\nline two", "stderr": ""
                }}),
            ),
        );
        let Entry::Tool(card) = &entries[0] else {
            panic!("expected a tool card");
        };
        // real newlines, not the raw {"exit_code":0,"stdout":"…"} JSON
        assert_eq!(card.output, vec!["line one", "line two"]);
    }

    #[test]
    fn other_result_shapes_read_cleanly() {
        // a file's content, a listing's entries, a bare scalar
        assert_eq!(readable(&json!({"path": "a", "content": "x\ny"})), "x\ny");
        assert_eq!(
            readable(&json!({"path": "d", "entries": ["one", "two"]})),
            "one\ntwo"
        );
        assert_eq!(readable(&json!(42)), "42");
        // an unfamiliar object falls back to pretty JSON (multi-line, not one blob)
        assert!(readable(&json!({"weird": {"a": 1}})).contains('\n'));
    }

    #[test]
    fn a_failed_completion_summarizes_the_error() {
        let mut entries = Vec::new();
        ingest(
            &mut entries,
            &ev(
                ce::TOOL_EXEC_STARTED,
                json!({"tool": "Run", "arguments": {}}),
            ),
        );
        ingest(
            &mut entries,
            &ev(
                ce::TOOL_EXEC_COMPLETED,
                json!({"status": "error", "error": {"message": "boom"}}),
            ),
        );
        let Entry::Tool(card) = &entries[0] else {
            panic!("expected a tool card");
        };
        assert_eq!(card.status, ToolStatus::Failed);
        assert_eq!(card.output, vec!["boom"]);
    }

    #[test]
    fn a_completion_without_a_running_card_is_ignored() {
        let mut entries = vec![Entry::User("hi".to_string())];
        let changed = ingest(
            &mut entries,
            &ev(ce::TOOL_EXEC_COMPLETED, json!({"status": "ok"})),
        );
        assert!(!changed);
        assert_eq!(entries, vec![Entry::User("hi".to_string())]);
    }

    // ── The add-a-model form ──────────────────────────────────────────────
    //
    // This is the only place a person hands the catalog a new entry, and the
    // failure it exists to prevent is "the model I added did not appear". It
    // had no tests at all, and nothing in the suite ever ran it.

    fn form(values: [&str; 6]) -> ModelForm {
        ModelForm {
            values: values.map(str::to_string),
            at: 0,
            problem: None,
        }
    }

    /// A key written down, and a key named. They are different decisions —
    /// one puts the secret in a file the agent can read, the other puts only
    /// a variable name there — so the form takes one or the other, never both.
    #[test]
    fn a_key_is_either_written_down_or_named_but_not_both() {
        let (id, spec) = form(["d", "openai", "deepseek-chat", "https://x", "sk-live", ""])
            .entry()
            .expect("a key written down is a complete entry");
        assert_eq!(id, "d");
        assert_eq!(spec["apiKey"], "sk-live");
        assert!(
            spec["apiKeyEnv"].is_null(),
            "only one of the two is written"
        );

        let (_, spec) = form(["d", "openai", "deepseek-chat", "https://x", "", "DS_KEY"])
            .entry()
            .expect("a key named is a complete entry too");
        assert_eq!(spec["apiKeyEnv"], "DS_KEY");
        assert!(spec["apiKey"].is_null());

        assert!(
            form(["d", "openai", "m", "https://x", "sk-live", "DS_KEY"])
                .entry()
                .is_err(),
            "both filled leaves it unsaid which one is meant"
        );
        assert!(
            form(["d", "openai", "m", "https://x", "", ""])
                .entry()
                .is_err(),
            "neither filled cannot reach the endpoint"
        );
    }

    #[test]
    fn what_the_form_refuses_to_save() {
        assert!(
            form(["d", "openai", "  ", "https://x", "k", ""])
                .entry()
                .is_err(),
            "an endpoint has to be told which model to run"
        );
        assert!(
            form(["d", "claude", "m", "https://x", "k", ""])
                .entry()
                .is_err(),
            "the dialect is a fixed set, and a typo in it is not a new dialect"
        );
        assert!(
            form(["d", "openai", "m", "   ", "k", ""]).entry().is_err(),
            "an endpoint with no address cannot be reached"
        );
    }

    /// The one dialect that talks to nothing: no address to reach and no key
    /// to reach it with, so neither is asked for.
    #[test]
    fn the_scripted_dialect_needs_no_endpoint_and_no_key() {
        let (id, spec) = form(["fake", "scripted", "m", "", "", ""])
            .entry()
            .expect("a scripted entry is complete with the two names alone");
        assert_eq!(id, "fake");
        assert_eq!(spec["adapter"], "scripted");
        assert!(spec["baseUrl"].is_null());
    }

    /// Surrounding space is typing, not content — otherwise a trailing space
    /// makes a second model with the same name.
    #[test]
    fn surrounding_space_is_not_part_of_what_was_typed() {
        let (id, spec) = form([" d ", " openai ", " m ", " https://x ", " k ", ""])
            .entry()
            .expect("padded fields are the same fields");
        assert_eq!(id, "d");
        assert_eq!(spec["adapter"], "openai");
        assert_eq!(spec["model"], "m");
        assert_eq!(spec["baseUrl"], "https://x");
        assert_eq!(spec["apiKey"], "k");
    }

    /// These messages are the whole of what the form can say when it refuses,
    /// and they are read on a screen, one line each. A run of blank columns in
    /// the middle of one is not something anybody wrote.
    #[test]
    fn a_refusal_reads_as_a_sentence() {
        let refusals = [
            form(["d", "claude", "m", "https://x", "k", ""]),
            form(["d", "openai", "m", "https://x", "", ""]),
            form(["d", "openai", "m", "https://x", "k", "K"]),
            form(["d", "openai", "", "https://x", "k", ""]),
            form(["d", "openai", "m", "", "k", ""]),
        ];
        for refused in refusals {
            let problem = refused.entry().expect_err("these are all refusals");
            assert!(
                !problem.contains("  "),
                "the form shows this to a person: {problem:?}"
            );
        }
    }

    /// Reading the form does not change it: the same six fields answer the
    /// same way however often they are asked.
    #[test]
    fn asking_the_form_twice_gives_the_same_answer() {
        for values in [
            ["d", "openai", "m", "https://x", "k", ""],
            ["d", "claude", "m", "https://x", "k", ""],
            ["", "scripted", "m", "", "", ""],
        ] {
            let subject = form(values);
            assert_eq!(subject.entry(), subject.entry());
        }
    }
}
