//! The gate: a policy component judges tool requests by declared effect
//! surface, forwards the allowed, answers the denied — and the main loop
//! needs no change to handle a denial (it's an ordinary tool failure).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{effects_policy, minimal_loop, scripted_model, silent_ui};

mod common;
use common::calc_tools;
use lattice::conformance::examine_policy;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, ComponentManifest, EventDraft, Factory, Kernel,
    KernelOptions, Wire,
};

#[test]
fn effects_policy_passes_the_policy_exam() {
    let problems = examine_policy(
        &effects_policy::manifest(),
        Some(Box::new(|c| {
            Box::new(effects_policy::EffectsPolicy::from_config(c))
        })),
    );
    assert_eq!(problems, Vec::<String>::new());
}

fn registry() -> HashMap<String, ComponentManifest> {
    [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
        (effects_policy::NAME.to_string(), effects_policy::manifest()),
    ]
    .into()
}

/// The gate sits on the tool-request wire: loop → policy → tools.
/// `net_tool` decides whether the offered tool declares network access.
fn run_with_policy(net_tool: bool) -> Vec<lattice::EventEnvelope> {
    // The forwarded case asks for a tool the assembly really PROVIDES, since a
    // request nobody provides never reaches a gate's downstream at all. The
    // denied case need not be provided by anybody — it is stopped at the gate,
    // which is the thing under test.
    let tool = if net_tool {
        json!({"name": "reach_out", "description": "get a url",
               "parameters": {}, "effects": {"network": ["*"]}})
    } else {
        json!({"name": "calc", "description": "pure", "parameters": {}, "effects": {"reversible": true}})
    };
    let called = if net_tool { "reach_out" } else { "calc" };
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "t1", "tool": called, "arguments": {"numbers": [1, 2]}}]},
        {"status": "ok", "text": "done"},
    ]});

    let displayed = Arc::new(Mutex::new(Vec::new()));
    let ui_buffer = Arc::clone(&displayed);
    let mut f: HashMap<String, Factory> = HashMap::new();
    f.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&ui_buffer)))),
    );
    f.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    f.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    f.insert(
        calc_tools::NAME.to_string(),
        Box::new(|_| Box::new(calc_tools::CalcTools::from_config(None))),
    );
    f.insert(
        effects_policy::NAME.to_string(),
        Box::new(|c| Box::new(effects_policy::EffectsPolicy::from_config(c))),
    );

    // calc-tools here answers the "fetch" tool by pretending success
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
                    config: Some(json!({"tools": [tool]})),
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
                "policy".to_string(),
                ComponentInstance {
                    component: effects_policy::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "tools".to_string(),
                ComponentInstance {
                    component: calc_tools::NAME.to_string(),
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
            // The gate is spliced onto the tool-request wire
            Wire::new("loop.run", "policy.review"),
            Wire::new("policy.forward", "tools.execute"),
            Wire::new("policy.verdict", "loop.tools"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };

    let mut kernel =
        Kernel::start(&assembly, &registry(), &mut f, KernelOptions::default()).unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    kernel.run_until_quiescent().unwrap();
    kernel.log().replay(1).unwrap()
}

#[test]
fn a_pure_tool_is_forwarded_through_the_gate() {
    let events = run_with_policy(false);
    // No denial decision; the tool actually ran (calc-tools answered)
    assert!(!events
        .iter()
        .any(|e| e.event_type == effects_policy::DECISION));
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "tools"));
}

#[test]
fn a_networking_tool_is_denied_and_the_loop_recovers() {
    let events = run_with_policy(true);

    // The gate recorded a reasoned denial decision
    let decision = events
        .iter()
        .find(|e| e.event_type == effects_policy::DECISION)
        .expect("a denial decision was recorded");
    assert_eq!(decision.source, "policy");
    assert!(decision.reason.as_deref().unwrap().contains("network"));

    // The tool executor never saw the request — the gate stopped it
    assert!(!events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "tools"));

    // The loop got a structured denial it understood without any changes,
    // and finished the turn
    let denial = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "policy")
        .expect("the loop received a verdict");
    assert_eq!(denial.payload["error"]["code"], "policy.denied");
    assert!(events.iter().any(|e| e.event_type == ce::TURN_COMPLETED));
}
