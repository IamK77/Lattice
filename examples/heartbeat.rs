//! Run the heartbeat assembly and print the resulting event flow.
//! Usage: cargo run --example heartbeat

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

/// Demo-only tool runner: one "calc" tool that sums numbers, so the scripted
/// heartbeat has a deterministic tool to call. Not an official component.
struct DemoTools;

const DEMO_TOOLS: &str = "demo-tools";

fn demo_tools_manifest() -> ComponentManifest {
    ComponentManifest {
        name: DEMO_TOOLS.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{DEMO_TOOLS}"),
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

impl Component for DemoTools {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let payload = if event.payload["tool"].as_str() == Some("calc") {
            let sum: f64 = event.payload["arguments"]["numbers"]
                .as_array()
                .map(|ns| ns.iter().filter_map(serde_json::Value::as_f64).sum())
                .unwrap_or(0.0);
            json!({"status": "ok", "result": sum, "call": event.payload["call"]})
        } else {
            json!({"status": "error", "call": event.payload["call"], "error": {
                "code": "tool.unknown",
                "message": format!("unknown tool: {}", event.payload["tool"]),
                "retryable": false,
                "blame": "request",
                "transient": false,
            }})
        };
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}

fn main() {
    let registry: HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (DEMO_TOOLS.to_string(), demo_tools_manifest()),
    ]
    .into();

    let displayed = Arc::new(Mutex::new(Vec::new()));
    let ui_buffer = Arc::clone(&displayed);
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&ui_buffer)))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|config| Box::new(minimal_loop::MinimalLoop::from_config(config))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|config| Box::new(scripted_model::ScriptedModel::from_config(config))),
    );
    factories.insert(DEMO_TOOLS.to_string(), Box::new(|_| Box::new(DemoTools)));

    let script = json!({
        "script": [
            {"status": "ok", "toolCalls": [{"tool": "slowest_test", "arguments": {}}]},
            {"status": "ok", "toolCalls": [{"tool": "calc", "arguments": {"numbers": [4, 7]}}]},
            {"status": "ok", "text": "4 + 7 = 11"},
        ]
    });
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "loop".to_string(),
                ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(script),
                },
            ),
            (
                "tools".to_string(),
                ComponentInstance {
                    component: DEMO_TOOLS.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };

    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("assembly must pass inspection");

    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "add 4 and 7"})),
    );
    kernel
        .run_until_quiescent()
        .expect("the heartbeat run must succeed");

    println!("event flow:");
    for event in kernel.log().replay(1).expect("read the completed ledger") {
        println!(
            "  #{:<2} {:<28} from {}",
            event.seq, event.event_type, event.source
        );
    }
    println!("frontend displayed: {:?}", displayed.lock().unwrap());
}
