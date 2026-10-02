//! Work provenance follows consumed inputs, not all conversation material.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft, EventEnvelope, Factory,
    Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};

struct Passive;
impl Component for Passive {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}

fn start() -> Kernel {
    let mut tools = scripted_model::manifest();
    tools.name = "fixture-tools".into();
    tools.inputs = vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])];
    tools.outputs = vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])];
    tools.tools = vec![
        json!({"name":"fixture","description":"Controlled test tool","parameters":{"type":"object"},"effects":{"reversible":true}}),
    ];
    tools.implements.clear();
    let registry = [
        (silent_ui::NAME.into(), silent_ui::manifest()),
        (minimal_loop::NAME.into(), minimal_loop::manifest()),
        (scripted_model::NAME.into(), scripted_model::manifest()),
        ("fixture-tools".into(), tools),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(vec![]))))),
    );
    factories.insert(
        minimal_loop::NAME.into(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(scripted_model::NAME.into(), Box::new(|_| Box::new(Passive)));
    factories.insert("fixture-tools".into(), Box::new(|_| Box::new(Passive)));
    let assembly = AssemblyManifest {
        instances: [
            ("ui", silent_ui::NAME),
            ("loop", minimal_loop::NAME),
            ("model", scripted_model::NAME),
            ("tools", "fixture-tools"),
        ]
        .into_iter()
        .map(|(id, component)| {
            (
                id.into(),
                ComponentInstance {
                    component: component.into(),
                    requires: vec![],
                    config: None,
                },
            )
        })
        .collect(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
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

fn latest(kernel: &Kernel, kind: &str) -> EventEnvelope {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rev()
        .find(|e| e.event_type == kind)
        .unwrap()
}
fn settle(kernel: &mut Kernel) {
    kernel.run_until_quiescent().unwrap();
    let errors: Vec<_> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::ERROR)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}
fn input(kernel: &mut Kernel, interface: &str) -> EventEnvelope {
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text":interface,"interface":interface}),
        ),
    );
    settle(kernel);
    latest(kernel, ce::USER_MESSAGE)
}
fn model_result(kernel: &mut Kernel, request: &EventEnvelope, payload: Value) {
    kernel.injector("model").emit(
        "result",
        EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&request.id], payload),
    );
    settle(kernel);
}
fn tool_result(kernel: &mut Kernel, continuation: bool) {
    let request = latest(kernel, ce::TOOL_EXEC_STARTED);
    let mut payload = json!({"call":request.payload["call"],"status":"ok","result":{}});
    if continuation {
        payload["continuation"] = json!("wait");
    }
    kernel.injector("tools").emit(
        "outcome",
        EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&request.id], payload),
    );
    settle(kernel);
}

#[test]
fn an_interjection_contributes_only_after_the_model_consumes_it() {
    let mut kernel = start();
    let a = input(&mut kernel, "a");
    let q1 = latest(&kernel, ce::MODEL_CALL_STARTED);
    let b = input(&mut kernel, "b");
    assert_eq!(latest(&kernel, ce::MODEL_CALL_STARTED).id, q1.id);
    assert_eq!(q1.payload["workInputs"], json!([a.id]));
    model_result(
        &mut kernel,
        &q1,
        json!({"status":"ok","toolCalls":[{"id":"t1","tool":"fixture","arguments":{}}]}),
    );
    // These tools came from q1: the later input must not retroactively grant them permission.
    let t1 = latest(&kernel, ce::TOOL_EXEC_STARTED);
    assert_eq!(
        t1.causes,
        vec![latest(&kernel, ce::MODEL_CALL_COMPLETED).id]
    );
    tool_result(&mut kernel, false);
    let q2 = latest(&kernel, ce::MODEL_CALL_STARTED);
    assert_eq!(q2.payload["workInputs"], json!([a.id, b.id]));
    assert!(
        q2.causes.contains(&b.id),
        "newly consumed input must also be an audited cause"
    );
}

#[test]
fn final_reply_ends_old_work_even_when_a_queued_input_starts_the_next_request() {
    let mut kernel = start();
    let a = input(&mut kernel, "a");
    let q1 = latest(&kernel, ce::MODEL_CALL_STARTED);
    let b = input(&mut kernel, "b");
    model_result(&mut kernel, &q1, json!({"status":"ok","text":"finished a"}));
    let q2 = latest(&kernel, ce::MODEL_CALL_STARTED);
    assert_ne!(q1.id, q2.id);
    assert_eq!(q2.payload["workInputs"], json!([b.id]));
    assert!(!q2.payload["workInputs"]
        .as_array()
        .unwrap()
        .contains(&json!(a.id)));
}

#[test]
fn waiting_and_new_turns_do_not_borrow_old_work_sources() {
    let mut kernel = start();
    input(&mut kernel, "a");
    let q1 = latest(&kernel, ce::MODEL_CALL_STARTED);
    model_result(
        &mut kernel,
        &q1,
        json!({"status":"ok","toolCalls":[{"id":"t1","tool":"fixture","arguments":{}}]}),
    );
    tool_result(&mut kernel, true);
    assert_eq!(latest(&kernel, ce::MODEL_CALL_STARTED).id, q1.id);
    let b = input(&mut kernel, "b");
    let q2 = latest(&kernel, ce::MODEL_CALL_STARTED);
    assert_eq!(q2.payload["workInputs"], json!([b.id]));
    model_result(&mut kernel, &q2, json!({"status":"ok","text":"finished b"}));
    let c = input(&mut kernel, "c");
    assert_eq!(
        latest(&kernel, ce::MODEL_CALL_STARTED).payload["workInputs"],
        json!([c.id])
    );
}
