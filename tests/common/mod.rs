//! Shared test scaffolding. Not part of the product: components here exist
//! only so integration tests can drive the kernel deterministically.
#![allow(dead_code)]

/// The retired calc-tools component, kept as test scaffolding: the simplest
/// deterministic in-process tool runner (one "calc" tool that sums numbers).
/// It left the official component set — a real model needs no adding machine —
/// but tests still need a tool with no side effects, no configuration and no
/// clock, and that is exactly what it is.
pub mod calc_tools {
    use serde_json::{json, Value};

    use lattice::core_events as ce;
    use lattice::{
        Component, ComponentManifest, Ctx, EventDraft, EventEnvelope, PortDecl, RuntimeKind,
    };

    pub const NAME: &str = "calc-tools";

    pub fn manifest() -> ComponentManifest {
        ComponentManifest {
            name: NAME.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            runtime: RuntimeKind::Inproc,
            entry: format!("builtin:{NAME}"),
            inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
            outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
            events: Vec::new(),
            default_wiring: Vec::new(),
            capabilities: None,
            implements: Vec::new(),
            tools: vec![json!({
                "name": "calc",
                "description": "Sum a list of numbers",
                "parameters": {
                    "type": "object",
                    "properties": {"numbers": {"type": "array", "items": {"type": "number"}}},
                    "required": ["numbers"],
                },
                "effects": {"reversible": true},
            })],
            prompt: None,
            handle_timeout_ms: None,
            concurrency: None,
        }
    }

    /// A demo tool runner with a single "calc" tool that sums numbers.
    /// Unknown tools fail — and that failure is an ordinary completed-state
    /// event, which the heartbeat scenario depends on.
    pub struct CalcTools {
        /// When false, foreign tools are met with silence (fan-out convention);
        /// when true (default), they get a structured unknown-tool error.
        exclusive: bool,
    }

    impl CalcTools {
        pub fn from_config(config: Option<&Value>) -> Self {
            Self {
                exclusive: config
                    .and_then(|c| c.get("exclusive"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            }
        }
    }

    impl Component for CalcTools {
        fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
            let tool = event.payload["tool"].as_str().unwrap_or("");
            if tool != "calc" && !self.exclusive {
                return; // someone else's tool; the fan-out convention is silence
            }
            let payload = if tool == "calc" {
                let sum: f64 = event.payload["arguments"]["numbers"]
                    .as_array()
                    .map(|numbers| numbers.iter().filter_map(Value::as_f64).sum())
                    .unwrap_or(0.0);
                json!({"status": "ok", "result": sum})
            } else {
                json!({"status": "error", "error": {
                    "code": "tool.unknown",
                    "message": format!("unknown tool: {tool}"),
                    "retryable": false,
                    "blame": "request",
                    "transient": false,
                }})
            };
            let mut payload = payload;
            payload["call"] = event.payload["call"].clone();
            ctx.emit(
                "outcome",
                EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
            );
        }
    }
}
