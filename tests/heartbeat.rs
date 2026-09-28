//! The heartbeat: a full user → model → tool → output loop running through
//! real wiring, deterministically. This is the CI regression for the kernel
//! as a whole — if the mental model breaks, this test breaks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, silent_ui};

mod common;
use common::calc_tools;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelError, KernelOptions, PortDecl, RuntimeKind, Wire,
    KERNEL_SOURCE,
};

fn registry() -> HashMap<String, ComponentManifest> {
    [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
    ]
    .into()
}

fn factories(displayed: Arc<Mutex<Vec<String>>>) -> HashMap<String, Factory> {
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&displayed)))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|config| Box::new(minimal_loop::MinimalLoop::from_config(config))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|config| Box::new(scripted_model::ScriptedModel::from_config(config))),
    );
    factories.insert(
        calc_tools::NAME.to_string(),
        Box::new(|config| Box::new(calc_tools::CalcTools::from_config(config))),
    );
    factories
}

/// The scripted scenario: the model first asks for an unknown tool (fails),
/// recovers by asking for "calc", then answers in text.
fn assembly() -> AssemblyManifest {
    let script = json!({
        "script": [
            {"status": "ok", "toolCalls": [{"tool": "slowest_test", "arguments": {}}]},
            {"status": "ok", "toolCalls": [{"tool": "calc", "arguments": {"numbers": [4, 7]}}]},
            {"status": "ok", "text": "4 + 7 = 11"},
        ]
    });
    AssemblyManifest {
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
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    }
}

/// The ledger past its opening event.
///
/// Every stream now begins with `core.stream.opened` — what runtime, what
/// assembly, when. These tests are about what happened after that.
fn conversation(kernel: &Kernel) -> Vec<EventEnvelope> {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type != ce::STREAM_OPENED)
        .collect()
}

#[test]
fn full_loop_runs_through_real_wiring() {
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed));
    let mut kernel = Kernel::start(
        &assembly(),
        &registry(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();

    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "add 4 and 7"})),
    );
    kernel.run_until_quiescent().unwrap();

    // The exact event flow, in order — determinism is the whole point
    let events = conversation(&kernel);
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec![
            ce::USER_MESSAGE,
            ce::MODEL_CALL_STARTED,
            ce::MODEL_CALL_COMPLETED,
            ce::TOOL_EXEC_STARTED,
            // A tool nobody provides reaches nobody, so nobody can answer it.
            // The kernel settles the chain instead — the honest sentence for a
            // call that will never finish, and the same one a restart writes
            // for chains it finds hanging.
            ce::INTERRUPTED,
            ce::MODEL_CALL_STARTED,
            ce::MODEL_CALL_COMPLETED,
            ce::TOOL_EXEC_STARTED,
            ce::TOOL_EXEC_COMPLETED,
            ce::MODEL_CALL_STARTED,
            ce::MODEL_CALL_COMPLETED,
            ce::OUTPUT_REPLY,   // the data channel to frontends
            ce::TURN_COMPLETED, // a pure boundary again
        ]
    );

    // The frontend saw exactly the final answer
    assert_eq!(*displayed.lock().unwrap(), vec!["4 + 7 = 11".to_string()]);

    // Audit: walk back from the reply — the whole run is one causal
    // chain, and the root cause of the detour is on it, self-evident
    let reply_id = events[11].id.clone();
    let chain = kernel.log().trace_back(&reply_id).unwrap();
    assert_eq!(chain.len(), 12);
    assert_eq!(chain.last().unwrap().event_type, ce::USER_MESSAGE);
    // The unprovided call is on the chain as a SETTLEMENT, not a result: it
    // says the chain ended, and says nothing about whether the work happened,
    // because nobody knows — the request never reached anyone.
    let settled = chain
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED)
        .expect("the unprovided call was settled on the chain");
    assert_eq!(settled.payload["by"], "no_provider");
    assert_eq!(settled.payload["tool"], "slowest_test");
    assert!(settled.reason.is_some(), "a settlement states its reason");

    // Every event came from the component the wiring says it must come from
    let sources: Vec<&str> = events.iter().map(|e| e.source.as_str()).collect();
    assert_eq!(
        sources,
        vec![
            // "core" is the kernel settling the call nothing could take — the
            // one event here no component authored
            "ui", "loop", "model", "loop", "core", "loop", "model", "loop", "tools", "loop",
            "model", "loop", "loop"
        ]
    );
}

#[test]
fn kernel_refuses_to_start_on_bad_assembly() {
    let mut bad = assembly();
    bad.wires.push(Wire::new("loop.ask", "tools.execute")); // type mismatch
    let mut factories = factories(Arc::new(Mutex::new(Vec::new())));
    let Err(err) = Kernel::start(&bad, &registry(), &mut factories, KernelOptions::default())
    else {
        panic!("a mis-wired assembly must not start");
    };
    assert!(matches!(err, KernelError::Inspection(_)));
}

#[test]
fn undeclared_emission_becomes_an_error_event() {
    let mut factories = factories(Arc::new(Mutex::new(Vec::new())));
    let mut kernel = Kernel::start(
        &assembly(),
        &registry(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();

    // "ui.user" declares only user_message; a turn event from it is a violation.
    // The kernel's own fault handling must be audit-visible: the violation is
    // recorded as a core.control.error event, not silently dropped.
    kernel
        .injector("ui")
        .emit("user", EventDraft::new(ce::TURN_COMPLETED, &[], json!({})));
    kernel.run_until_quiescent().unwrap();

    let events = conversation(&kernel);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, ce::ERROR);
    assert_eq!(events[0].source, KERNEL_SOURCE);
    assert!(events[0].payload["message"]
        .as_str()
        .unwrap()
        .contains("never declared"));
}

#[test]
fn unwitnessed_cause_is_rejected_as_error_event() {
    // Two frontends, no wires between them: what one emits, the other never sees
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui_a".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "ui_b".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![],
    };
    let mut factories = factories(Arc::new(Mutex::new(Vec::new())));
    let mut kernel = Kernel::start(
        &assembly,
        &registry(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();

    kernel.injector("ui_a").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "first"})),
    );
    kernel.run_until_quiescent().unwrap();
    let first_id = conversation(&kernel)[0].id.clone();

    // ui_b claims the first message caused its emission — but it never
    // witnessed that event, so the claim is a lie and must be refused
    kernel.injector("ui_b").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[&first_id], json!({"text": "lie"})),
    );
    kernel.run_until_quiescent().unwrap();

    let events = conversation(&kernel);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].event_type, ce::ERROR);
    assert!(events[1].payload["message"]
        .as_str()
        .unwrap()
        .contains("not witnessed"));
}

/// A component that echoes every event back out — two of these wired in a
/// circle produce an infinite cascade, which the dispatch budget must brake.
struct PingPong;

impl Component for PingPong {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        ctx.emit(
            "out",
            EventDraft::new(ce::TURN_STARTED, &[&event.id], json!({})),
        );
    }
}

fn ping_pong_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "ping-pong".to_string(),
        version: "0.0.0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:ping-pong".to_string(),
        inputs: vec![PortDecl::new("in", &[ce::TURN_STARTED])],
        outputs: vec![PortDecl::new("out", &[ce::TURN_STARTED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

#[test]
fn runaway_cascade_hits_the_dispatch_budget() {
    let registry: HashMap<String, ComponentManifest> =
        [("ping-pong".to_string(), ping_pong_manifest())].into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("ping-pong".to_string(), Box::new(|_| Box::new(PingPong)));

    let assembly = AssemblyManifest {
        instances: [
            (
                "a".to_string(),
                ComponentInstance {
                    component: "ping-pong".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "b".to_string(),
                ComponentInstance {
                    component: "ping-pong".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("a.out", "b.in"), Wire::new("b.out", "a.in")],
    };

    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            max_dispatch_per_run: 16,
            ..KernelOptions::default()
        },
    )
    .unwrap();

    kernel
        .injector("a")
        .emit("out", EventDraft::new(ce::TURN_STARTED, &[], json!({})));
    kernel.run_until_quiescent().unwrap();

    // 16 dispatched events, then the brake: one error event, delivery stopped
    let events = conversation(&kernel);
    assert_eq!(events.len(), 17);
    let last = events.last().unwrap();
    assert_eq!(last.event_type, ce::ERROR);
    assert_eq!(last.source, KERNEL_SOURCE);
    assert!(last.payload["message"]
        .as_str()
        .unwrap()
        .contains("budget exceeded"));
}

#[test]
fn model_material_is_pointers_plus_fingerprint_not_copies() {
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed));
    let mut kernel = Kernel::start(
        &assembly(),
        &registry(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "add 4 and 7"})),
    );
    kernel.run_until_quiescent().unwrap();

    let events = conversation(&kernel);
    let first_ask = &events[1];
    assert_eq!(
        first_ask.payload["input"]["parts"][0]["event"]
            .as_str()
            .unwrap(),
        events[0].id
    );
    assert!(first_ask.payload["input"]["fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    // The raw user text exists in exactly one event — the user message itself.
    // Every later model call refers to it by pointer; the log stays O(n)
    let copies = events
        .iter()
        .filter(|e| e.payload.to_string().contains("add 4 and 7"))
        .count();
    assert_eq!(copies, 1);
}

/// A tool nobody provides must not hang the turn.
///
/// The model can name a tool that does not exist — it invents them. Nothing
/// answers such a request, and the loop counts answers, so the turn used to
/// wait forever and the session could only be restarted. The kernel now says
/// the one honest thing about a chain that will never finish: it was
/// interrupted. Never "it failed" — a component can die with its work half
/// done, and a model told "failed" retries, which is the one thing that must
/// not happen to a call that may already have deleted something.
#[test]
fn a_tool_nobody_provides_settles_instead_of_hanging_the_turn() {
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed));
    let mut kernel = Kernel::start(
        &assembly(),
        &registry(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = conversation(&kernel);

    // The script's first call is `slowest_test`, which nothing provides
    let settled = events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "no_provider")
        .expect("the unprovided call is settled");
    assert_eq!(settled.payload["tool"], "slowest_test");

    // And the turn went ON: it reached the reply, rather than waiting forever
    assert_eq!(*displayed.lock().unwrap(), vec!["4 + 7 = 11".to_string()]);
    assert!(
        events.iter().any(|e| e.event_type == ce::TURN_COMPLETED),
        "the turn ended"
    );
    kernel.shutdown();
}

/// A result arriving when no round is open must not start a fresh question.
///
/// The loop used to ask "have all my results arrived?" by comparing a count
/// against zero — trivially true whenever it was waiting for nothing. So one
/// stray or duplicate result fired a model call on top of an unanswered one,
/// which is the exact shape both wire formats reject.
#[test]
fn a_stray_tool_result_does_not_open_a_question() {
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed));
    let mut kernel = Kernel::start(
        &assembly(),
        &registry(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hi"})),
    );
    kernel.run_until_quiescent().unwrap();
    let asks = |k: &Kernel| {
        k.log()
            .replay(1)
            .unwrap()
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
            .count()
    };
    let before = asks(&kernel);

    // A result for a call nobody is waiting on — a late answer, a duplicate,
    // a component that woke up after its round closed
    kernel.injector("tools").emit(
        "outcome",
        EventDraft::new(
            ce::TOOL_EXEC_COMPLETED,
            &[],
            json!({"call": "nobody_waits_for_this", "status": "ok", "result": 1.0}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(asks(&kernel), before, "a stray result asks nothing");
    kernel.shutdown();
}

#[test]
fn parallel_tool_results_join_into_one_model_call() {
    let script = json!({
        "script": [
            {"status": "ok", "toolCalls": [
                {"tool": "calc", "arguments": {"numbers": [1, 2]}},
                {"tool": "calc", "arguments": {"numbers": [3, 4]}},
            ]},
            {"status": "ok", "text": "3 and 7"},
        ]
    });
    let mut asm = assembly();
    asm.instances.get_mut("model").unwrap().config = Some(script);

    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed));
    let mut kernel =
        Kernel::start(&asm, &registry(), &mut factories, KernelOptions::default()).unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "two sums"})),
    );
    kernel.run_until_quiescent().unwrap();

    let events = conversation(&kernel);
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();

    // The contract promises CAUSALITY, not a global interleaving: while the
    // tools component works on call one, the dispatcher may record call two's
    // start before or after call one's completion — both schedules are legal.
    // Assert the shape the kernel actually guarantees.
    assert_eq!(
        &types[..3],
        [
            ce::USER_MESSAGE,
            ce::MODEL_CALL_STARTED,
            ce::MODEL_CALL_COMPLETED
        ]
    );
    let mut middle: Vec<&str> = types[3..7].to_vec();
    middle.sort_unstable();
    assert_eq!(
        middle,
        [
            ce::TOOL_EXEC_COMPLETED,
            ce::TOOL_EXEC_COMPLETED,
            ce::TOOL_EXEC_STARTED,
            ce::TOOL_EXEC_STARTED,
        ],
        "two tool calls, two outcomes, in any legal interleaving"
    );
    assert_eq!(
        &types[7..],
        [
            ce::MODEL_CALL_STARTED,
            ce::MODEL_CALL_COMPLETED,
            ce::OUTPUT_REPLY,
            ce::TURN_COMPLETED,
        ]
    );
    // Per call: each outcome is caused by ITS start — order within the pair
    // is causal, not positional
    for outcome in events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
    {
        let start = events
            .iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_STARTED && outcome.causes.contains(&e.id))
            .expect("every outcome points back at its own start");
        assert_eq!(start.payload["call"], outcome.payload["call"]);
    }

    // The join: the second model call is jointly caused by BOTH tool outcomes
    let outcome_ids: Vec<String> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .map(|e| e.id.clone())
        .collect();
    let second_ask = &events[7];
    let mut ask_causes = second_ask.causes.clone();
    ask_causes.sort_unstable();
    let mut expected = outcome_ids.clone();
    expected.sort_unstable();
    assert_eq!(ask_causes, expected);

    // The audit graph from the reply reaches every prior event, each once
    let ancestors = kernel.log().trace_back(&events[9].id).unwrap();
    assert_eq!(ancestors.len(), 10);
    assert_eq!(*displayed.lock().unwrap(), vec!["3 and 7".to_string()]);
}
