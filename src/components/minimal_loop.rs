use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

#[path = "minimal_loop_material.rs"]
mod material_store;
#[path = "minimal_loop_recovery.rs"]
mod recovery;

#[cfg(test)]
#[path = "minimal_loop_paging_tests.rs"]
mod paging_tests;
#[cfg(test)]
#[path = "minimal_loop_tests.rs"]
mod tests;

pub const NAME: &str = "minimal-loop";
pub const WAITING: &str = "loop.waiting";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![
            PortDecl::new("input", &[ce::USER_MESSAGE, ce::WAKE]),
            PortDecl::new("model", &[ce::MODEL_CALL_COMPLETED]),
            PortDecl::new("tools", &[ce::TOOL_EXEC_COMPLETED]),
            // A call this round is waiting on that will never finish: the
            // component holding it died, or nothing provides that tool. The
            // kernel delivers this to whoever witnessed the request, so no
            // wire carries it and no assembly has to remember one.
            PortDecl::new("faults", &[ce::INTERRUPTED]),
        ],
        outputs: vec![
            PortDecl::new("ask", &[ce::MODEL_CALL_STARTED]),
            PortDecl::new("run", &[ce::TOOL_EXEC_STARTED]),
            PortDecl::new("out", &[ce::OUTPUT_REPLY, ce::TURN_COMPLETED]),
            PortDecl::new("state", &[WAITING]),
        ],
        events: vec![
            EventTypeDecl::decision(WAITING, "The loop yields until new input arrives")
                .with_schema(json!({"type": "object"})),
        ],
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

/// The smallest possible agent loop: user message → model call; requested tool
/// calls → tool executions; final text → turn completed. Context assembly is a
/// bare list of event pointers — the real context manager replaces this later.
/// The loop itself is an ordinary, replaceable component: that is the point.
pub struct MinimalLoop {
    model: String,
    /// Tool declarations offered to the model on every call (from config)
    tools: Vec<Value>,
    /// Live material only changes on this instance's actual deliveries.
    parts: material_store::Store,
    /// Independent historical projection for restart, never live input.
    material: recovery::Material,
    material_through: u64,
    /// Historical seed of the live view; repair can republish those immutable
    /// pages without admitting later undelivered input into this instance.
    material_base: Option<u64>,
    material_saved: u64,
    material_source: Option<crate::kernel::log::ReaderIdentity>,
    /// WHICH tool calls the current round still waits for, by call id.
    ///
    /// A count could not tell one call from another, so a duplicate or late
    /// result counted as progress, and — worse — "have all my results
    /// arrived?" was trivially TRUE whenever the count was zero, so a stray
    /// result outside any round fired a fresh model call on top of an
    /// unanswered one.
    pending_calls: std::collections::HashSet<String>,
    /// How many calls in this round carried no id at all. The contract allows
    /// it, and a result for one cannot be matched to anything — so those, and
    /// only those, fall back to being counted.
    pending_unnamed: usize,
    /// Tool-completed event ids gathered this round; they jointly cause the
    /// next model call (a causal join)
    gathered: Vec<String>,
    /// Park only when every tool result is a successful waiting receipt.
    /// Any ordinary result is fresh information and should reach the model.
    only_wait_receipts: bool,
    /// A model call is out and its answer has not come back. Asking again now
    /// would put a second assistant turn on the record before the first was
    /// answered — the shape that makes a conversation unsendable.
    awaiting_model: bool,
    /// Something arrived since the last question went out (a typed line, a
    /// background wake). It is already in `parts`; this remembers that the
    /// model has not been shown it yet, so the round does not end quietly on
    /// top of it.
    unseen_input: bool,
    /// Whether `parts` has been rebuilt from the ledger. The ledger survives
    /// a restart; component memory does not — on the first delivery the loop
    /// re-derives its material list from the record (it IS just pointers, so
    /// the ledger is its natural backing store).
    rebuilt: bool,
}

impl MinimalLoop {
    pub fn from_config(config: Option<&Value>) -> Self {
        let model = config
            .and_then(|c| c.get("model"))
            .and_then(Value::as_str)
            .unwrap_or("scripted")
            .to_string();
        let tools = config
            .and_then(|c| c.get("tools"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Self {
            model,
            tools,
            parts: material_store::Store::default(),
            material_source: None,
            material: recovery::Material::default(),
            material_through: 0,
            material_base: None,
            material_saved: 0,
            pending_calls: std::collections::HashSet::new(),
            pending_unnamed: 0,
            gathered: Vec::new(),
            only_wait_receipts: false,
            awaiting_model: false,
            unseen_input: false,
            rebuilt: false,
        }
    }

    fn material_input(&self) -> std::io::Result<Value> {
        let mut hasher = Sha256::new();
        let mut parts = Vec::new();
        self.parts.visit(|id| {
            hasher.update(id.as_bytes());
            hasher.update(b"\n");
            parts.push(json!({"event": id}));
        })?;
        Ok(json!({"parts": parts, "fingerprint": format!("sha256:{:x}", hasher.finalize())}))
    }

    /// Whether this round is still waiting on any tool result.
    fn round_open(&self) -> bool {
        !self.pending_calls.is_empty() || self.pending_unnamed > 0
    }

    fn ask(&mut self, causes: &[&str], ctx: &mut Ctx) {
        let input = match self.material_input().or_else(|error| {
            eprintln!("slow recovery for live material: {error}");
            self.repair_material_pages(ctx.log())?;
            self.material_input()
        }) {
            Ok(input) => input,
            Err(error) => {
                let affected: Vec<String> = causes.iter().map(|id| (*id).to_string()).collect();
                ctx.fail("read conversation material", error.to_string(), &affected);
                return;
            }
        };
        self.awaiting_model = true;
        self.unseen_input = false;
        // The offered tools are whatever the assembled providers declare
        // (collected by the kernel, provider-stamped, refreshed on hot
        // install) plus any hand-written extras from this loop's config
        let mut tools = ctx.tool_decls();
        tools.extend(self.tools.iter().cloned());
        ctx.emit(
            "ask",
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                causes,
                json!({
                    "model": self.model,
                    "input": input,
                    "tools": tools,
                }),
            ),
        );
    }
}

impl Component for MinimalLoop {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if let Err(error) = self.accept_material(ctx.log(), event) {
            ctx.fail("restore conversation material", error.to_string(), &[]);
            return;
        }
        match port {
            // MID-TURN INSERTION. A line typed while the turn runs, or a
            // background wake, is not a new turn and must not become a second
            // question on top of an unanswered one — that is the shape both
            // wire formats reject outright. It is already in `parts`, so the
            // NEXT question this loop asks carries it, which is the earliest
            // moment it could be heard. Until then it is merely unseen.
            "input" if self.awaiting_model || self.round_open() => {
                self.unseen_input = true;
            }
            "input" => self.ask(&[&event.id], ctx),
            "model" => {
                self.awaiting_model = false;
                match event.payload["toolCalls"].as_array() {
                    Some(calls) if !calls.is_empty() => {
                        self.pending_calls = calls
                            .iter()
                            .filter_map(|call| call["id"].as_str().map(str::to_string))
                            .collect();
                        self.pending_unnamed = calls.len() - self.pending_calls.len();
                        self.gathered.clear();
                        self.only_wait_receipts = true;
                        for call in calls {
                            // A dispatcher call carries a DEFERRED tool inside:
                            // unwrap mechanically (same call id, no policy) so
                            // routing, gates and results all see a normal call
                            let (tool, arguments) = if call["tool"]
                                == crate::contracts::component::DEFERRED_DISPATCHER
                            {
                                (
                                    call["arguments"]["tool"].clone(),
                                    call["arguments"]["arguments"].clone(),
                                )
                            } else {
                                (call["tool"].clone(), call["arguments"].clone())
                            };
                            ctx.emit(
                            "run",
                            EventDraft::new(
                                ce::TOOL_EXEC_STARTED,
                                &[&event.id],
                                json!({"call": call["id"], "tool": tool, "arguments": arguments}),
                            ),
                        );
                        }
                    }
                    _ => {
                        // The reply is the data channel; the turn event is a pure
                        // boundary again (both caused by the completing call —
                        // sibling causes, since an emission cannot know the id
                        // its sibling will receive at append time)
                        ctx.emit(
                            "out",
                            EventDraft::new(
                                ce::OUTPUT_REPLY,
                                &[&event.id],
                                json!({
                                    "text": event.payload["text"],
                                    "error": event.payload["error"],
                                    "cancelled": event.payload["status"] == "cancelled",
                                }),
                            ),
                        );
                        ctx.emit(
                            "out",
                            EventDraft::new(ce::TURN_COMPLETED, &[&event.id], json!({})),
                        );
                        // Something arrived while that answer was on its way. The
                        // reply above belongs to what was asked before it; this
                        // asks again so the newcomer is answered too, rather than
                        // lying in the material unread until someone types again.
                        if self.unseen_input {
                            self.ask(&[&event.id], ctx);
                        }
                    }
                }
            }
            "faults" => {
                // The chain is closed, just not by a result. Stop waiting on
                // it; the model learns what happened from the material, where
                // the adapters render it as an outcome nobody knows.
                let ended = match event
                    .causes
                    .iter()
                    .map(|cause| ctx.log().get(cause))
                    .collect::<std::io::Result<Vec<_>>>()
                {
                    Ok(events) => events.into_iter().flatten().collect::<Vec<_>>(),
                    Err(error) => {
                        ctx.fail("resolve interrupted calls", error.to_string(), &[]);
                        return;
                    }
                };
                // A MODEL call ending is the end of the round, not of one
                // errand inside it: no answer is coming, so waiting for one
                // is waiting forever. Say so outwardly and close the turn —
                // silence here is a session that looks busy with nothing on
                // its way back. Deliberately no re-ask: the usual reason the
                // adapter did not answer is that it is gone, and asking again
                // would be a loop with no way out of it.
                if ended
                    .iter()
                    .any(|started| started.event_type == ce::MODEL_CALL_STARTED)
                {
                    self.awaiting_model = false;
                    self.pending_calls.clear();
                    self.pending_unnamed = 0;
                    self.gathered.clear();
                    ctx.emit(
                        "out",
                        EventDraft::new(
                            ce::OUTPUT_REPLY,
                            &[&event.id],
                            json!({"text": Value::Null, "error": Value::Null,
                                   "cancelled": true}),
                        ),
                    );
                    ctx.emit(
                        "out",
                        EventDraft::new(ce::TURN_COMPLETED, &[&event.id], json!({})),
                    );
                    return;
                }
                let closed: Vec<Option<String>> = ended
                    .iter()
                    .filter(|started| started.event_type == ce::TOOL_EXEC_STARTED)
                    .map(|started| started.payload["call"].as_str().map(str::to_string))
                    .collect();
                let mut freed = false;
                for call in closed {
                    match call {
                        Some(call) => freed |= self.pending_calls.remove(&call),
                        // A call with no id of its own is only counted, so its
                        // settlement can only be counted too
                        None if self.pending_unnamed > 0 => {
                            self.pending_unnamed -= 1;
                            freed = true;
                        }
                        None => {}
                    }
                }
                if freed && !self.round_open() {
                    let gathered = std::mem::take(&mut self.gathered);
                    let mut causes: Vec<&str> = gathered.iter().map(String::as_str).collect();
                    causes.push(&event.id);
                    self.ask(&causes, ctx);
                }
            }
            "tools" => {
                // Only a result for a call this round is WAITING on counts. A
                // duplicate, or one arriving after the round closed, is not
                // progress — and must not be mistaken for the round finishing.
                let answered = event.payload["call"].as_str().unwrap_or_default();
                if !self.pending_calls.remove(answered) {
                    if self.pending_unnamed == 0 {
                        return; // a duplicate, or one arriving after the round closed
                    }
                    self.pending_unnamed -= 1;
                }
                self.only_wait_receipts &=
                    event.payload["status"] == "ok" && event.payload["continuation"] == "wait";
                self.gathered.push(event.id.clone());
                if !self.round_open() {
                    let gathered = std::mem::take(&mut self.gathered);
                    let causes: Vec<&str> = gathered.iter().map(String::as_str).collect();
                    // A wake can beat its receipt to this component. Likewise,
                    // a person may have spoken while the tools were running.
                    // Never park on top of input the model has not seen.
                    if self.only_wait_receipts && !self.unseen_input {
                        ctx.emit("state", EventDraft::new(WAITING, &causes, json!({}))
                            .with_reason("Only waiting receipts arrived; yield until new input instead of polling"));
                    } else {
                        self.ask(&causes, ctx);
                    }
                }
            }
            _ => {}
        }
    }
}
