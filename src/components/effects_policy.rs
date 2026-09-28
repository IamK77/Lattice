//! The first official policy — a gate on the tool-request wire.
//!
//! It judges by the effect surface a tool DECLARED ON THE LEDGER (found by
//! reading back the latest model-call material), never by tool names.
//! Allowed requests are forwarded — a re-emission with a causal link, so the
//! gate's hop is audit-visible. Denied requests get two sibling events, both
//! caused by the request: the gate's own decision (decision-class, reason
//! mandatory) and a completed-state answer the loop understands without any
//! changes. Undeclared surfaces are the most dangerous kind and deny by
//! default.

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "effects-policy";
pub const DECISION: &str = "policy.gate.decision";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("review", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![
            PortDecl::new("forward", &[ce::TOOL_EXEC_STARTED]),
            PortDecl::new("verdict", &[ce::TOOL_EXEC_COMPLETED]),
            // The gate's own letter, beyond the generic profile
            PortDecl::new("decision", &[DECISION]),
        ],
        events: vec![EventTypeDecl::decision(
            DECISION,
            "The gate allowed or denied a tool request",
        )
        .with_schema(json!({
            "type": "object",
            "required": ["verdict", "tool"],
            "properties": {
                "verdict": {"enum": ["deny"]},
                "tool": {"type": "string"},
                "rule": {"type": "string"},
            },
        }))],
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["policy".to_string()],
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

pub struct EffectsPolicy {
    allow_undeclared: bool,
    allow_network: bool,
    allow_executes: bool,
    allow_writes: bool,
}

impl EffectsPolicy {
    pub fn from_config(config: Option<&Value>) -> Self {
        let flag = |key: &str| {
            config
                .and_then(|c| c.get(key))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        Self {
            allow_undeclared: flag("allowUndeclared"),
            allow_network: flag("allowNetwork"),
            allow_executes: flag("allowExecutes"),
            allow_writes: flag("allowWrites"),
        }
    }

    /// The surface this tool was declared with on the model call that asked
    /// for THIS request. The gate judges what was declared on the record —
    /// never the name, and never a declaration belonging to some other
    /// request (see [`ce::declared_effects`]).
    fn declared_effects(
        &self,
        ctx: &Ctx,
        request_id: &str,
        tool: &str,
    ) -> Result<Option<Value>, String> {
        // By id, so the walk touches the request's few ancestors instead of
        // copying the conversation to find them.
        //
        // The tool list is a document: past a size it lives in a file beside
        // the ledger, and a real one is 13 KB, so this is the normal case
        // rather than the exception. Read the declarations, not the reference
        // to them — a gate that cannot see a declaration treats the call as
        // undeclared, which is the safe direction but the wrong answer.
        ce::try_declared_effects(
            |id| {
                let Some(mut event) = ctx.log().get(id).map_err(|e| e.to_string())? else {
                    return Ok(None);
                };
                event.payload["tools"] = ctx.document(&event.payload["tools"])?;
                Ok(Some(event))
            },
            request_id,
            tool,
        )
    }

    /// None = allowed; Some(rule) = the rule that tripped
    fn judge(&self, effects: Option<&Value>) -> Option<String> {
        let Some(effects) = effects else {
            return (!self.allow_undeclared)
                .then(|| "undeclared effect surface (treated as most dangerous)".to_string());
        };
        if effects["executes"] == true && !self.allow_executes {
            return Some("runs external programs".to_string());
        }
        let non_empty = |key: &str| {
            effects[key]
                .as_array()
                .map(|list| !list.is_empty())
                .unwrap_or(false)
        };
        if non_empty("network") && !self.allow_network {
            return Some("reaches the network".to_string());
        }
        if non_empty("writes") && !self.allow_writes {
            return Some("writes outside its sandbox".to_string());
        }
        None
    }
}

impl Component for EffectsPolicy {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("").to_string();
        let effects = match self.declared_effects(ctx, &event.id, &tool) {
            Ok(effects) => effects,
            Err(message) => {
                ctx.emit("verdict", EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], json!({
                    "call": event.payload["call"], "status": "error",
                    "error": {"code": "policy.history_read_failed", "message": message, "blame": "component", "retryable": false}
                })));
                return;
            }
        };
        match self.judge(effects.as_ref()) {
            None => {
                // Allowed: forward unchanged — but as a new event caused by
                // the reviewed one, so the gate's hop stays on the record
                ctx.emit(
                    "forward",
                    EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], event.payload.clone()),
                );
            }
            Some(rule) => {
                // Denied: the decision (with its reason) and the answer are
                // sibling events — an emission cannot know the id its
                // sibling will receive at append time
                ctx.emit(
                    "decision",
                    EventDraft::new(
                        DECISION,
                        &[&event.id],
                        json!({"verdict": "deny", "tool": tool, "rule": rule}),
                    )
                    .with_reason(&format!("denied {tool}: {rule}")),
                );
                ctx.emit(
                    "verdict",
                    EventDraft::new(
                        ce::TOOL_EXEC_COMPLETED,
                        &[&event.id],
                        json!({
                            "call": event.payload["call"],
                            "status": "error",
                            "error": {
                                "code": "policy.denied",
                                "message": format!("blocked by policy: {rule}"),
                                "blame": "request",
                                "retryable": false,
                            },
                        }),
                    ),
                );
            }
        }
    }
}
