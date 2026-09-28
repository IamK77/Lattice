//! Cancellation travels through the actual component/host/child-kernel path.
//! Channels establish that the child is busy; no sleep establishes correctness.
use std::collections::HashMap;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use lattice::components::{minimal_loop, scripted_model, silent_ui, subagent};
use lattice::subagent_host::{MainAndChildren, SubagentHost};
use lattice::{
    core_events as ce, AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, StreamHost, StreamTemplate, Wire,
};
use serde_json::{json, Value};

struct WaitingModel(mpsc::Sender<()>, bool);
impl Component for WaitingModel {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if event.event_type != ce::MODEL_CALL_STARTED {
            return;
        }
        if self.1 {
            ctx.emit(
                "result",
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&event.id],
                    json!({"status":"ok", "text":"unexpected second call"}),
                ),
            );
            return;
        }
        self.1 = true;
        self.0.send(()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(ctx.cancellation().cancelled());
        // A late result must be retained, but it must not start another tool.
        ctx.emit("result", EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&event.id], json!({
            "status": "ok", "toolCalls": [{"id": "late", "tool": "ask", "arguments": {"prompt": "must not run"}}]
        })));
    }
}

fn template(script: Value) -> StreamTemplate {
    let registry = [
        silent_ui::manifest(),
        minimal_loop::manifest(),
        scripted_model::manifest(),
        subagent::manifest(),
    ]
    .into_iter()
    .map(|m| (m.name.clone(), m))
    .collect();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::default()))),
    );
    factories.insert(
        minimal_loop::NAME.into(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.into(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        subagent::NAME.into(),
        Box::new(|c| Box::new(subagent::Subagent::from_config(c))),
    );
    let assembly = AssemblyManifest {
        instances: [
            ("ui".into(), ComponentInstance::new(silent_ui::NAME, None)),
            (
                "loop".into(),
                ComponentInstance::new(minimal_loop::NAME, None),
            ),
            (
                "model".into(),
                ComponentInstance::new(scripted_model::NAME, Some(json!({"script": script}))),
            ),
            (
                "subagent".into(),
                ComponentInstance::new(
                    subagent::NAME,
                    Some(json!({"experts": [{"name": "worker"}]})),
                ),
            ),
        ]
        .into(),
        // Deliberately NO interrupt wires. Host stop must not depend on them.
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "subagent.execute"),
            Wire::new("subagent.outcome", "loop.tools"),
            Wire::new("subagent.wake", "loop.input"),
        ],
    };
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

fn cancel_call(id: &str, job: u64) -> Value {
    json!({"id": id, "tool": "CancelExpert", "arguments": {"job": job, "reason": "requirement changed"}})
}

#[test]
fn cancelling_own_expert_is_audited_and_late_result_cannot_restart_work() {
    let script = json!([
        {"status":"ok", "toolCalls":[{"id":"ask", "tool":"ask", "arguments":{"expert":"worker", "prompt":"wait"}}]},
        {"status":"ok", "text":"delegated"},
        {"status":"ok", "toolCalls":[cancel_call("cancel", 1), cancel_call("again", 1), cancel_call("unknown", 99)]},
        {"status":"ok", "text":"acknowledged"}
    ]);
    let mut parent = template(script);
    let mut kernel = Kernel::start(
        &parent.assembly,
        &parent.registry,
        &mut parent.factories,
        KernelOptions::default(),
    )
    .unwrap();
    let wake = kernel.take_wake_receiver().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let mut child = template(json!([]));
    child.factories.insert(
        scripted_model::NAME.into(),
        Box::new(move |_| Box::new(WaitingModel(ready_tx.clone(), false))),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let mut children = StreamHost::new(HashMap::from([("worker".into(), child)]))
        .with_ledger_path(move |stream| Some(path.join(format!("{stream}.jsonl"))));
    let mut host = SubagentHost::new();
    let parent_id = "parent".to_string();
    let say = |kernel: &Kernel, text: &str| {
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":text})),
        )
    };
    say(&kernel, "delegate");
    kernel.run_until_quiescent().unwrap();
    host.poll(&mut MainAndChildren {
        main_id: &parent_id,
        main: &mut kernel,
        children: &mut children,
    })
    .unwrap();
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("expert entered model call");
    let mut stranger_template = template(json!([
        {"status":"ok", "toolCalls":[cancel_call("foreign", 1)]},
        {"status":"ok", "text":"done"}
    ]));
    let mut stranger = Kernel::start(
        &stranger_template.assembly,
        &stranger_template.registry,
        &mut stranger_template.factories,
        KernelOptions::default(),
    )
    .unwrap();
    say(&stranger, "cancel someone else's job");
    stranger.run_until_quiescent().unwrap();
    host.poll(&mut MainAndChildren {
        main_id: "stranger",
        main: &mut stranger,
        children: &mut children,
    })
    .unwrap();
    stranger.run_until_quiescent().unwrap();
    assert!(stranger
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|e| e.payload["error"]["code"] == "expert.not_running"));
    stranger.shutdown();
    say(&kernel, "cancel");
    loop {
        kernel.run_until_quiescent().unwrap();
        host.poll(&mut MainAndChildren {
            main_id: &parent_id,
            main: &mut kernel,
            children: &mut children,
        })
        .unwrap();
        let events = kernel.log().replay(1).unwrap();
        let acked = ["cancel", "again", "unknown"].iter().all(|id| {
            events
                .iter()
                .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == *id)
        });
        if acked && events.iter().any(|e| e.event_type == ce::WAKE) {
            break;
        }
        wake.recv_timeout(Duration::from_secs(10))
            .expect("a causal host wake must arrive");
    }
    let events = kernel.log().replay(1).unwrap();
    for id in ["ask", "cancel", "again", "unknown"] {
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == id)
                .count(),
            1
        );
    }
    let outcome = |id: &str| {
        events
            .iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == id)
            .unwrap()
    };
    assert_eq!(
        outcome("cancel").payload["result"]["cancellation"],
        "requested"
    );
    // Completion can win the second request; neither outcome repeats the ending.
    assert!(
        outcome("again").payload["result"]["cancellation"] == "already_requested"
            || outcome("again").payload["error"]["code"] == "expert.not_running"
    );
    assert_eq!(
        outcome("unknown").payload["error"]["code"],
        "expert.not_running"
    );
    let endings: Vec<_> = events.iter().filter(|e| e.event_type == ce::WAKE).collect();
    assert_eq!(endings.len(), 1);
    assert_eq!(endings[0].payload["body"]["interrupted"], "cancelled");
    assert_eq!(
        endings[0].payload["body"]["cancellation"]["dispatchStopped"],
        true
    );
    let child_text = std::fs::read_to_string(dir.path().join("parent-sub-1.jsonl")).unwrap();
    let child_events: Vec<EventEnvelope> = child_text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let stopped = child_events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["scope"] == "stream")
        .unwrap();
    let requested = events
        .iter()
        .find(|e| e.event_type == subagent::CANCEL_REQUESTED && e.payload["call"] == "cancel")
        .unwrap();
    assert_eq!(stopped.origin.as_ref().unwrap().event, requested.id);
    assert!(
        child_events
            .iter()
            .any(|e| e.event_type == ce::MODEL_CALL_COMPLETED),
        "late result remains in the audit"
    );
    assert!(
        !child_events
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_STARTED),
        "late tool call must not execute"
    );
    assert!(ce::hanging_chain_heads(&child_events, ce::MODEL_CALL_STARTED).is_empty());
    assert!(
        !child_events.iter().any(|e| e.event_type == ce::ERROR),
        "{:?}",
        child_events
    );
    drop(host);
    kernel.shutdown();
}

#[test]
fn foreground_cancel_has_one_cancelled_answer_and_no_background_wake() {
    let mut parent = template(json!([
        {"status":"ok", "toolCalls":[
            {"id":"ask", "tool":"ask", "arguments":{"expert":"worker", "prompt":"wait", "background":false}},
            cancel_call("cancel", 1)
        ]},
        {"status":"ok", "text":"done"}
    ]));
    let mut kernel = Kernel::start(
        &parent.assembly,
        &parent.registry,
        &mut parent.factories,
        KernelOptions::default(),
    )
    .unwrap();
    let wake = kernel.take_wake_receiver().unwrap();
    let (ready, _received) = mpsc::channel();
    let mut child = template(json!([]));
    child.factories.insert(
        scripted_model::NAME.into(),
        Box::new(move |_| Box::new(WaitingModel(ready.clone(), false))),
    );
    let mut children = StreamHost::new(HashMap::from([("worker".into(), child)]));
    let mut host = SubagentHost::new();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text":"delegate then cancel"}),
        ),
    );
    loop {
        kernel.run_until_quiescent().unwrap();
        host.poll(&mut MainAndChildren {
            main_id: "parent",
            main: &mut kernel,
            children: &mut children,
        })
        .unwrap();
        if kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "ask")
        {
            break;
        }
        wake.recv_timeout(Duration::from_secs(10))
            .expect("completion wake");
    }
    let events = kernel.log().replay(1).unwrap();
    let answers: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "ask")
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].payload["status"], "cancelled");
    assert_eq!(
        answers[0].payload["result"]["cancellation"]["dispatchStopped"],
        true
    );
    assert!(!events.iter().any(|e| e.event_type == ce::WAKE));
    drop(host);
    kernel.shutdown();
}
