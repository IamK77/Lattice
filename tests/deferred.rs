//! Deferred tools: a hot-installed tool must not invalidate the provider's
//! prompt cache. Its declaration stays OUT of the schema while the cache is
//! warm (reachable through the one resident dispatcher — the model knows the
//! tool from the conversation in which it built it), and is promoted into
//! the schema the first time the cache is cold anyway, on the record.
#![cfg(unix)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{context_gate, minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    deferred_dispatcher_decl, AssemblyManifest, ComponentInstance, ComponentManifest, EventDraft,
    Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire, DEFERRED_DISPATCHER,
};

fn hash_tool() -> ComponentManifest {
    ComponentManifest {
        name: "hash-tool".to_string(),
        version: "0.1.0".to_string(),
        runtime: RuntimeKind::Process,
        entry: "python3 examples/components/hash_tool.py".to_string(),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["tool-provider".to_string()],
        tools: vec![json!({
            "name": "sha256",
            "description": "hash a text",
            "parameters": {"type": "object", "properties": {"text": {"type": "string"}},
                           "required": ["text"]},
            "effects": {"reversible": true},
        })],
        prompt: None,
        handle_timeout_ms: Some(10_000),
        concurrency: None,
    }
}

fn start_kernel(nested: bool) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    // Turn 1 ends WARM (cache_read 400). Turn 2: the model reaches the new
    // tool through the dispatcher; its final answer reports a COLD cache.
    // Turn 3: plain answer — by now promotion must have happened.
    let mut script = json!({"script": [
        {"status": "ok", "text": "turn one",
         "usage": {"input_tokens": 100, "cache_read_input_tokens": 400}},
        {"status": "ok",
         "usage": {"input_tokens": 120, "cache_read_input_tokens": 400},
         "toolCalls": [{"id": "d1", "tool": "UseDeferredTool",
                        "arguments": {"tool": "sha256", "arguments": {"text": "lattice"}}}]},
        {"status": "ok", "text": "turn two done",
         "usage": {"input_tokens": 140, "cache_read_input_tokens": 0}},
        {"status": "ok", "text": "turn three",
         "usage": {"input_tokens": 60, "cache_read_input_tokens": 50}},
    ]});
    if nested {
        for reply in script["script"].as_array_mut().unwrap() {
            let cached = reply["usage"]
                .as_object_mut()
                .unwrap()
                .remove("cache_read_input_tokens")
                .unwrap();
            reply["usage"]["input_tokens_details"] = json!({"cached_tokens":cached});
        }
    }
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
                "gate".to_string(),
                ComponentInstance {
                    component: context_gate::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"profile": {
                        "contextWindow": 64000,
                        "usageFields": {"input": "input_tokens",
                                        "cacheRead": if nested { "input_tokens_details.cached_tokens" } else { "cache_read_input_tokens" }},
                    }})),
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
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "gate.ask"),
            Wire::new("gate.forward", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

fn say(kernel: &mut Kernel, text: &str) {
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": text})),
    );
    kernel.run_until_quiescent().unwrap();
}

#[test]
fn a_hot_installed_tool_defers_then_promotes_when_the_cache_is_cold() {
    check_cache_promotion(false);
}

#[test]
fn nested_responses_cache_usage_defers_until_cold() {
    check_cache_promotion(true);
}

fn check_cache_promotion(nested: bool) {
    let mut kernel = start_kernel(nested);
    say(&mut kernel, "turn one"); // ends with a WARM cache on the record

    // Hot install mid-conversation — the moment the cache must NOT break
    kernel
        .install(
            hash_tool(),
            "hashx",
            None,
            &[
                Wire::new("loop.run", "hashx.execute"),
                Wire::new("hashx.outcome", "loop.tools"),
            ],
            "the agent built itself a hashing tool",
            &[],
        )
        .unwrap();

    say(&mut kernel, "hash lattice for me"); // cache warm: deferred
    let events = kernel.log().replay(1).unwrap();

    // The ask right after the install offers the DISPATCHER, not the tool
    let asks: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .collect();
    let warm_ask = &asks[1]; // turn two, first ask
    let tools = warm_ask.payload["tools"].as_array().unwrap();
    assert!(
        !tools.iter().any(|t| t["name"] == "sha256"),
        "a warm cache must keep the new tool out of the schema: {tools:?}"
    );
    assert!(
        tools.iter().any(|t| t["name"] == DEFERRED_DISPATCHER),
        "the resident doorway must be offered instead"
    );
    // …and the dispatcher's declaration is byte-stable by construction
    assert!(tools.contains(&deferred_dispatcher_decl()));

    // The dispatcher call was unwrapped mechanically: the REAL tool ran,
    // under the model's own call id, in its own process
    let started = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .unwrap();
    assert_eq!(started.payload["tool"], "sha256");
    assert_eq!(started.payload["call"], "d1");
    let outcome = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "hashx")
        .expect("the deferred tool actually ran");
    assert_eq!(
        outcome.payload["result"],
        "4cbe09597b76794b5f6b854c1c1c035ede6d241f250f223b61987dce9d2d7a4b"
    );

    say(&mut kernel, "turn three"); // last completion reported a COLD cache
    let events = kernel.log().replay(1).unwrap();

    // Promotion happened, on the record, exactly once
    let promotions: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == context_gate::DECISION && e.payload["action"] == "promote")
        .collect();
    assert_eq!(promotions.len(), 1);
    assert_eq!(promotions[0].payload["promoted"], json!(["sha256"]));
    assert!(promotions[0]
        .reason
        .as_deref()
        .is_some_and(|r| !r.is_empty()));

    // And the final ask carries the tool directly — the doorway is gone
    let last_ask = events
        .iter()
        .rev()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "gate")
        .unwrap();
    let tools = last_ask.payload["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "sha256"));
    assert!(!tools.iter().any(|t| t["name"] == DEFERRED_DISPATCHER));

    kernel.shutdown();
}
