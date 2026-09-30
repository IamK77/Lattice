//! Narrow frontend-to-provider bridge. Management is audited, not a model turn.
use super::expert_definitions;
use crate::contracts::event::EventTypeDecl;
use crate::experts::{
    activation::ACTIVATE,
    management::{DELETE, LIST, SAVE},
};
use crate::{
    core_events as ce, Component, ComponentManifest, Ctx, EventDraft, EventEnvelope, PortDecl,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub const NAME: &str = "expert-ui";
pub const CHANNEL: &str = "experts";
pub const RESULT: &str = "experts.ui.result";
pub const PURPOSE: &str = "frontend.experts";

pub fn manifest() -> ComponentManifest {
    let mut manifest = expert_definitions::manifest();
    manifest.name = NAME.into();
    manifest.entry = format!("builtin:{NAME}");
    manifest.inputs = vec![
        PortDecl::new("input", &[ce::EXTERNAL_INPUT]),
        PortDecl::new("completed", &[ce::TOOL_EXEC_COMPLETED, ce::INTERRUPTED]),
    ];
    manifest.outputs = vec![
        PortDecl::new("run", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("result", &[RESULT]),
    ];
    manifest.events = vec![EventTypeDecl::new(
        RESULT,
        "An expert management operation finished for its frontend",
    )
    .with_schema(json!({"type":"object","required":["request","operation","status"]}))];
    manifest.tools.clear();
    manifest.implements.clear();
    manifest.capabilities = None;
    manifest
}

struct Pending {
    input: String,
    request: Value,
    operation: Value,
}
#[derive(Default)]
pub struct ExpertUi {
    pending: HashMap<String, Pending>,
    seen: HashSet<String>,
}

impl ExpertUi {
    fn handle_checked(&mut self, event: &EventEnvelope, ctx: &mut Ctx) -> Result<(), String> {
        if event.event_type == ce::EXTERNAL_INPUT {
            if event.payload["channel"] != CHANNEL || !self.seen.insert(event.id.clone()) {
                return Ok(());
            }
            let operation = event.payload["operation"].as_str().unwrap_or("");
            let tool = match operation {
                "list" => Some(LIST),
                "inspect" => Some(expert_definitions::INSPECT),
                "save" => Some(SAVE),
                "activate" => Some(ACTIVATE),
                "delete" => Some(DELETE),
                _ => None,
            };
            if tool.is_none()
                || !event.payload["arguments"].is_object()
                || event.payload["request"]
                    .as_str()
                    .is_none_or(|id| id.is_empty() || id.len() > 128)
            {
                ctx.emit("result", EventDraft::new(RESULT, &[&event.id], json!({
                    "request":event.payload["request"],"operation":operation,"status":"error",
                    "error":{"message":"Invalid expert management operation"}
                })));
                return Ok(());
            }
            let call = format!("expert-ui:{}", event.id);
            self.pending.insert(
                call.clone(),
                Pending {
                    input: event.id.clone(),
                    request: event.payload["request"].clone(),
                    operation: json!(operation),
                },
            );
            // The external command is the entire causal origin. Attaching an
            // old model turn would borrow its historical tool declarations.
            ctx.emit("run", EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], json!({
                "call":call,"tool":tool.unwrap(),"arguments":event.payload["arguments"],"purpose":PURPOSE
            })));
            return Ok(());
        }
        if !matches!(
            event.event_type.as_str(),
            ce::TOOL_EXEC_COMPLETED | ce::INTERRUPTED
        ) {
            return Ok(());
        }
        let mut call = event.payload["call"].as_str().map(str::to_owned);
        if call
            .as_ref()
            .is_none_or(|call| !self.pending.contains_key(call))
        {
            call = None;
            for cause in &event.causes {
                let Some(request) = ctx.log().header(cause).map_err(|error| error.to_string())?
                else {
                    continue;
                };
                if request.event_type == ce::TOOL_EXEC_STARTED
                    && request
                        .call
                        .as_ref()
                        .is_some_and(|call| self.pending.contains_key(call))
                {
                    call = request.call;
                    break;
                }
            }
        }
        let Some(pending) = call.and_then(|call| self.pending.remove(&call)) else {
            return Ok(());
        };
        let mut payload = event.payload.clone();
        payload["request"] = pending.request;
        payload["operation"] = pending.operation;
        payload["outcome"] = json!(event.id);
        if event.event_type == ce::INTERRUPTED {
            payload["status"] = json!("interrupted");
            payload["error"] = json!({"message":"Operation interrupted; inspect its current state before trying again."});
        }
        // This is a UI receipt referencing the actual outcome, not a second
        // tool response and not an input that wakes the conversation loop.
        ctx.emit(
            "result",
            EventDraft::new(RESULT, &[&event.id, &pending.input], payload),
        );
        Ok(())
    }
}
impl Component for ExpertUi {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if let Err(error) = self.handle_checked(event, ctx) {
            ctx.fail(
                "read expert operation outcome",
                error,
                std::slice::from_ref(&event.id),
            );
        }
    }
}
