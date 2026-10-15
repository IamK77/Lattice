//! A real loop consumes one outcome per model request, not every late notice.
use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft, EventEnvelope, Factory,
    Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};
use std::collections::HashMap;

#[path = "model_settlement/mixed.rs"]
mod mixed;
#[path = "model_settlement/recovery.rs"]
mod recovery;

struct Passive;
impl Component for Passive {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}
struct Forward;
impl Component for Forward {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let mut payload = event.payload.clone();
        payload["system"] = json!("forwarded by a gate");
        ctx.emit(
            "forward",
            EventDraft::new(ce::MODEL_CALL_STARTED, &[&event.id], payload),
        );
    }
}

fn start(gated: bool) -> Kernel {
    start_with(gated, false, None)
}

fn start_with(gated: bool, sibling: bool, log_file: Option<std::path::PathBuf>) -> Kernel {
    let mut peer = scripted_model::manifest();
    peer.inputs
        .push(PortDecl::new("tool_seen", &[ce::TOOL_EXEC_STARTED]));
    peer.outputs
        .push(PortDecl::new("fault", &[ce::INTERRUPTED]));
    peer.outputs
        .push(PortDecl::new("forward", &[ce::MODEL_CALL_STARTED]));
    peer.implements.clear();
    let mut tools = peer.clone();
    tools.name = "fixture-tools".into();
    tools.inputs = vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])];
    tools.outputs = vec![PortDecl::new("result", &[ce::TOOL_EXEC_COMPLETED])];
    tools.tools = vec![
        json!({"name":"fixture","description":"Controlled test tool",
        "parameters":{"type":"object"},"effects":{"reversible":true}}),
    ];
    let registry = [
        (silent_ui::NAME.into(), silent_ui::manifest()),
        (minimal_loop::NAME.into(), minimal_loop::manifest()),
        (scripted_model::NAME.into(), peer),
        ("fixture-tools".into(), tools),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(silent_ui::NAME.into(), Box::new(|_| Box::new(Passive)));
    factories.insert(
        minimal_loop::NAME.into(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.into(),
        Box::new(|c| {
            if c.is_some_and(|c| c["forward"] == true) {
                Box::new(Forward)
            } else {
                Box::new(Passive)
            }
        }),
    );
    factories.insert("fixture-tools".into(), Box::new(|_| Box::new(Passive)));
    let mut instances: std::collections::BTreeMap<String, ComponentInstance> = [
        ("ui", silent_ui::NAME),
        ("loop", minimal_loop::NAME),
        ("model", scripted_model::NAME),
        ("tools", "fixture-tools"),
    ]
    .into_iter()
    .map(|(name, component)| {
        (
            name.into(),
            ComponentInstance {
                component: component.into(),
                requires: vec![],
                config: None,
            },
        )
    })
    .collect();
    let mut wires = vec![
        Wire::new("ui.user", "loop.input"),
        Wire::new("model.result", "loop.model"),
        Wire::new("model.fault", "loop.faults"),
        Wire::new("loop.run", "tools.execute"),
        Wire::new("loop.run", "model.tool_seen"),
        Wire::new("tools.result", "loop.tools"),
    ];
    if gated {
        for name in ["gate", "gate2"] {
            instances.insert(
                name.into(),
                ComponentInstance {
                    component: scripted_model::NAME.into(),
                    requires: vec![],
                    config: Some(json!({"forward":true})),
                },
            );
            wires.push(Wire::new(&format!("{name}.fault"), "loop.faults"));
        }
        wires.extend([
            Wire::new("loop.ask", "gate.request"),
            Wire::new("gate.forward", "gate2.request"),
            Wire::new("gate2.forward", "model.request"),
        ]);
    } else {
        wires.push(Wire::new("loop.ask", "model.request"));
    }
    if sibling {
        instances.insert(
            "other-loop".into(),
            ComponentInstance {
                component: minimal_loop::NAME.into(),
                requires: vec![],
                config: None,
            },
        );
        wires.extend([
            Wire::new("ui.user", "other-loop.input"),
            Wire::new("other-loop.ask", "model.request"),
            Wire::new("model.result", "other-loop.model"),
            Wire::new("model.fault", "other-loop.faults"),
        ]);
    }
    Kernel::start(
        &AssemblyManifest { instances, wires },
        &registry,
        &mut factories,
        KernelOptions {
            stream: Some("model-settlement-test".into()),
            log_file,
            ..KernelOptions::default()
        },
    )
    .unwrap()
}

fn events(kernel: &Kernel, kind: &str) -> Vec<EventEnvelope> {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == kind)
        .collect()
}
fn latest(kernel: &Kernel, kind: &str) -> EventEnvelope {
    events(kernel, kind).pop().unwrap()
}
fn settle(kernel: &mut Kernel) {
    kernel.run_until_quiescent().unwrap();
    assert!(events(kernel, ce::ERROR).is_empty());
    assert!(events(kernel, ce::COMPONENT_CRASHED).is_empty());
}
fn input(kernel: &mut Kernel, text: &str) -> EventEnvelope {
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":text})),
    );
    settle(kernel);
    latest(kernel, ce::MODEL_CALL_STARTED)
}
fn result(kernel: &mut Kernel, request: &EventEnvelope, payload: Value) -> EventEnvelope {
    kernel.injector("model").emit(
        "result",
        EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&request.id], payload),
    );
    settle(kernel);
    latest(kernel, ce::MODEL_CALL_COMPLETED)
}
fn interrupt(kernel: &mut Kernel, peer: &str, request: &EventEnvelope) {
    kernel.injector(peer).emit(
        "fault",
        EventDraft::new(ce::INTERRUPTED, &[&request.id], json!({"by":"test"})),
    );
    settle(kernel);
}
fn tool_payload() -> Value {
    json!({"status":"ok","toolCalls":[{"id":"t1","tool":"fixture","arguments":{}}]})
}
fn one_turn(kernel: &Kernel) {
    assert_eq!(
        (
            events(kernel, ce::OUTPUT_REPLY).len(),
            events(kernel, ce::TURN_COMPLETED).len()
        ),
        (1, 1)
    );
}

#[test]
fn interruption_then_late_results_neither_reply_again_nor_start_tools() {
    for late in [
        json!({"status":"cancelled"}),
        json!({"status":"ok","text":"late"}),
        tool_payload(),
    ] {
        let mut kernel = start(false);
        let request = input(&mut kernel, "first");
        interrupt(&mut kernel, "model", &request);
        result(&mut kernel, &request, late);
        one_turn(&kernel);
        assert!(events(&kernel, ce::TOOL_EXEC_STARTED).is_empty());
        assert_eq!(
            events(&kernel, ce::MODEL_CALL_COMPLETED).len(),
            1,
            "late results stay audited"
        );
    }
}

#[test]
fn completion_then_duplicate_results_or_interruptions_end_only_once() {
    let mut kernel = start(false);
    let request = input(&mut kernel, "first");
    result(
        &mut kernel,
        &request,
        json!({"status":"ok","text":"first answer"}),
    );
    interrupt(&mut kernel, "model", &request);
    result(&mut kernel, &request, tool_payload());
    one_turn(&kernel);
    assert_eq!(
        latest(&kernel, ce::OUTPUT_REPLY).payload["text"],
        "first answer"
    );
    assert!(events(&kernel, ce::TOOL_EXEC_STARTED).is_empty());
    assert_eq!(events(&kernel, ce::MODEL_CALL_COMPLETED).len(), 2);
}

#[test]
fn late_old_outcomes_cannot_finish_or_replace_a_new_pending_call() {
    for interrupted in [false, true] {
        let mut kernel = start(false);
        let old = input(&mut kernel, "first");
        if interrupted {
            interrupt(&mut kernel, "model", &old);
        } else {
            result(
                &mut kernel,
                &old,
                json!({"status":"ok","text":"first answer"}),
            );
        }
        let current = input(&mut kernel, "second");
        interrupt(&mut kernel, "model", &old);
        let late_tool = result(&mut kernel, &old, tool_payload());
        let late_text = result(
            &mut kernel,
            &old,
            json!({"status":"ok","text":"late old answer"}),
        );
        let still_current = input(&mut kernel, "third, while second is pending");
        assert_eq!(
            still_current.id, current.id,
            "late outcomes must not clear the active wait"
        );
        one_turn(&kernel);
        assert!(events(&kernel, ce::TOOL_EXEC_STARTED).is_empty());
        result(
            &mut kernel,
            &current,
            json!({"status":"ok","text":"second answer"}),
        );
        let next = latest(&kernel, ce::MODEL_CALL_STARTED);
        assert_ne!(next.id, current.id);
        for stale in [late_tool.id, late_text.id] {
            assert!(
                !next.payload["input"]["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|part| part["event"] == stale),
                "late output leaked into live material"
            );
        }
        result(
            &mut kernel,
            &next,
            json!({"status":"ok","text":"third answer"}),
        );
        assert_eq!(events(&kernel, ce::OUTPUT_REPLY).len(), 3);
        assert_eq!(events(&kernel, ce::TURN_COMPLETED).len(), 3);
    }
}

#[test]
fn duplicate_model_outcomes_do_not_clear_an_active_tool_round() {
    let mut kernel = start(false);
    let request = input(&mut kernel, "first");
    result(&mut kernel, &request, tool_payload());
    let tool = latest(&kernel, ce::TOOL_EXEC_STARTED);
    interrupt(&mut kernel, "model", &request);
    result(&mut kernel, &request, tool_payload());
    assert_eq!(events(&kernel, ce::TOOL_EXEC_STARTED).len(), 1);
    assert!(events(&kernel, ce::TURN_COMPLETED).is_empty());
    kernel.injector("tools").emit(
        "result",
        EventDraft::new(
            ce::TOOL_EXEC_COMPLETED,
            &[&tool.id],
            json!({"call":"t1","status":"ok","result":{}}),
        ),
    );
    settle(&mut kernel);
    let next = latest(&kernel, ce::MODEL_CALL_STARTED);
    assert_ne!(
        next.id, request.id,
        "the tool completion must still advance its own round"
    );
    result(&mut kernel, &next, json!({"status":"ok","text":"finished"}));
    one_turn(&kernel);
}

#[test]
fn forwarding_copies_share_one_outcome_even_when_the_root_is_interrupted() {
    for interrupted_first in [false, true] {
        let mut kernel = start(true);
        let forwarded = input(&mut kernel, "through two gates");
        let root = events(&kernel, ce::MODEL_CALL_STARTED)
            .into_iter()
            .find(|e| e.source == "loop")
            .unwrap();
        assert_ne!(forwarded.id, root.id);
        if interrupted_first {
            interrupt(&mut kernel, "gate", &root);
            result(&mut kernel, &forwarded, tool_payload());
        } else {
            result(
                &mut kernel,
                &forwarded,
                json!({"status":"ok","text":"done"}),
            );
            interrupt(&mut kernel, "gate", &root);
        }
        one_turn(&kernel);
        assert!(events(&kernel, ce::TOOL_EXEC_STARTED).is_empty());
        let next = input(&mut kernel, "a new round after forwarding");
        result(&mut kernel, &forwarded, tool_payload());
        one_turn(&kernel);
        result(
            &mut kernel,
            &next,
            json!({"status":"ok","text":"new answer"}),
        );
        assert_eq!(events(&kernel, ce::TURN_COMPLETED).len(), 2);
        assert!(events(&kernel, ce::TOOL_EXEC_STARTED).is_empty());
    }
}
