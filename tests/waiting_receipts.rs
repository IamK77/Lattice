//! Waiting is a tool-result convention, not a special case for shell commands.
use lattice::components::{minimal_loop, scripted_model, shell_tools, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft, EventEnvelope, Factory,
    Kernel, KernelOptions, Wire,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

struct Receipts;
impl Component for Receipts {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let mut receipt = event.payload["arguments"].clone();
        if receipt["wakeFirst"] == true {
            ctx.emit(
                "wake",
                EventDraft::new(
                    ce::WAKE,
                    &[&event.id],
                    json!({"source":"probe", "summary":"already finished"}),
                ),
            );
        }
        receipt["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], receipt.clone()),
        );
        if receipt["duplicate"] == true {
            ctx.emit(
                "outcome",
                EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], receipt),
            );
        }
    }
}

fn exercise(receipts: Vec<Value>) -> Vec<EventEnvelope> {
    let mut provider = shell_tools::manifest();
    provider.name = "receipts".into();
    provider.entry = "builtin:receipts".into();
    provider.tools = vec![
        json!({"name":"Probe", "description":"Test receipts", "parameters":{"type":"object"}}),
    ];
    provider.concurrency = Some(1);
    let registry = [
        minimal_loop::manifest(),
        scripted_model::manifest(),
        silent_ui::manifest(),
        provider,
    ]
    .into_iter()
    .map(|m| (m.name.clone(), m))
    .collect();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        minimal_loop::NAME.into(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.into(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::default()))),
    );
    factories.insert("receipts".into(), Box::new(|_| Box::new(Receipts)));
    let calls: Vec<_> = receipts
        .into_iter()
        .enumerate()
        .map(|(i, args)| json!({"id":format!("c{i}"),"tool":"Probe","arguments":args}))
        .collect();
    let script =
        json!({"script":[{"status":"ok","toolCalls":calls},{"status":"ok","text":"next"}]});
    let assembly = AssemblyManifest {
        instances: [
            ("ui".into(), ComponentInstance::new(silent_ui::NAME, None)),
            (
                "loop".into(),
                ComponentInstance::new(minimal_loop::NAME, None),
            ),
            (
                "model".into(),
                ComponentInstance::new(scripted_model::NAME, Some(script)),
            ),
            ("tools".into(), ComponentInstance::new("receipts", None)),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("tools.wake", "loop.input"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"go"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    events
}

#[test]
fn only_an_entire_batch_of_successful_waiting_receipts_parks() {
    let wait = json!({"status":"ok","continuation":"wait"});
    for (receipts, parked) in [
        (vec![wait.clone()], true),
        (vec![wait.clone(), wait.clone()], true),
        (
            vec![
                wait.clone(),
                json!({"status":"ok","result":"new information"}),
            ],
            false,
        ),
        (
            vec![
                wait.clone(),
                json!({"status":"error","continuation":"wait"}),
            ],
            false,
        ),
        (
            vec![json!({"status":"cancelled","continuation":"wait"})],
            false,
        ),
        (
            vec![json!({"status":"ok","result":{"continuation":"wait"}})],
            false,
        ),
    ] {
        let events = exercise(receipts);
        assert!(!events.iter().any(|e| e.event_type == ce::ERROR));
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
                .count(),
            if parked { 1 } else { 2 }
        );
        let waiting: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == minimal_loop::WAITING)
            .collect();
        assert_eq!(waiting.len(), usize::from(parked));
        if parked {
            assert!(waiting[0].reason.is_some());
            assert!(!waiting[0].causes.is_empty());
            assert!(!events.iter().any(|e| e.event_type == ce::TURN_COMPLETED));
        }
    }
}

#[test]
fn a_wake_before_its_receipt_is_consumed_without_parking() {
    let events = exercise(vec![
        json!({"status":"ok","continuation":"wait","wakeFirst":true}),
    ]);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
            .count(),
        2
    );
    assert!(!events.iter().any(|e| e.event_type == minimal_loop::WAITING));
    let wake = events.iter().find(|e| e.event_type == ce::WAKE).unwrap();
    let last = events
        .iter()
        .rfind(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .unwrap();
    assert!(last.payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["event"] == wake.id));
}

#[test]
fn duplicate_receipts_neither_wake_the_model_nor_park_twice() {
    let events = exercise(vec![
        json!({"status":"ok","continuation":"wait","duplicate":true}),
    ]);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == minimal_loop::WAITING)
            .count(),
        1
    );
}
