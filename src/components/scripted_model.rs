use std::collections::VecDeque;

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::EventDraft;
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "scripted-model";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![
            PortDecl::new("request", &[ce::MODEL_CALL_STARTED]),
            // Same assembly shape as the real adapters: an interrupt wire
            // may land here (the event is ignored; cancellation acts through
            // the token). Without this port, swapping a real brain for the
            // scripted one would break the assembly — the opposite of what
            // "interchangeable" promises.
            PortDecl::new("control", &[ce::INTERRUPTED]),
        ],
        outputs: vec![PortDecl::new("result", &[ce::MODEL_CALL_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["model-adapter".to_string()],
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

/// A model adapter that replays scripted replies — the deterministic stand-in
/// for a real provider, so CI can assert an exact event flow.
/// Config: `{"script": [<model_call_completed payload>, ...]}`.
pub struct ScriptedModel {
    script: VecDeque<Value>,
}

impl ScriptedModel {
    pub fn from_config(config: Option<&Value>) -> Self {
        let script = config
            .and_then(|c| c.get("script"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Self {
            script: script.into(),
        }
    }
}

impl Component for ScriptedModel {
    fn handle(
        &mut self,
        port: &str,
        event: &crate::contracts::event::EventEnvelope,
        ctx: &mut Ctx,
    ) {
        if port != "request" {
            return; // a control interrupt is not a request — never consume script
        }
        let payload = self.script.pop_front().unwrap_or_else(|| {
            json!({"status": "error", "error": {
                "code": "script.exhausted",
                "message": "script exhausted",
                "blame": "request",
            }})
        });
        ctx.emit(
            "result",
            EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&event.id], payload),
        );
    }
}
