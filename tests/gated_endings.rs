//! Endings in the shape the product actually ships: a gate between the loop
//! and every tool provider.
//!
//! `tests/heartbeat.rs` proves that a call always ends — wired straight from
//! the loop to the tools, as every test here was. The product is not wired
//! that way. `loop.run` goes to the trust gate, and a gate re-emits what it
//! forwards, so one request exists on the ledger TWICE: the loop's copy and
//! the gate's. An ending lands on the gate's copy, which the loop never saw,
//! and the rule for who gets told matched on that one copy alone.
//!
//! So the guarantee held in every test and in none of the assemblies anyone
//! runs: a hallucinated tool name, or a provider dying mid-call, hung the
//! round forever with the ending sitting on the ledger, delivered to nobody.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, silent_ui, trust_policy};

mod common;
use common::calc_tools;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

const WATCHED_MODEL: &str = "watched-model";
const EXPLODING_TOOLS: &str = "exploding-tools";

/// A model adapter that also remembers anything delivered to its control
/// port. Otherwise the scripted model exactly.
///
/// It exists to hold the line the witness rule draws. The tool request grew
/// out of the model call this adapter answered, so an addressee rule that
/// walked causality generally — rather than only through copies of the one
/// request — would reach it, and a tool's death would cancel the model's call.
struct WatchedModel {
    inner: scripted_model::ScriptedModel,
    controls: Arc<Mutex<Vec<String>>>,
}

impl Component for WatchedModel {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if port == "control" {
            self.controls
                .lock()
                .unwrap()
                .push(event.payload["by"].as_str().unwrap_or("?").to_string());
        }
        self.inner.handle(port, event, ctx);
    }
}

fn watched_model_manifest() -> ComponentManifest {
    ComponentManifest {
        name: WATCHED_MODEL.to_string(),
        entry: format!("builtin:{WATCHED_MODEL}"),
        ..scripted_model::manifest()
    }
}

/// A tool provider that dies on the spot. Same declaration as the calculator,
/// so the same request reaches it — and then nothing comes back.
struct ExplodingTools;

impl Component for ExplodingTools {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {
        panic!("boom");
    }
}

fn exploding_manifest() -> ComponentManifest {
    ComponentManifest {
        name: EXPLODING_TOOLS.to_string(),
        entry: format!("builtin:{EXPLODING_TOOLS}"),
        runtime: RuntimeKind::Inproc,
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        ..calc_tools::manifest()
    }
}

fn registry() -> HashMap<String, ComponentManifest> {
    [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (WATCHED_MODEL.to_string(), watched_model_manifest()),
        (trust_policy::NAME.to_string(), trust_policy::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
        (EXPLODING_TOOLS.to_string(), exploding_manifest()),
    ]
    .into()
}

fn factories(
    displayed: Arc<Mutex<Vec<String>>>,
    controls: Arc<Mutex<Vec<String>>>,
) -> HashMap<String, Factory> {
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
        WATCHED_MODEL.to_string(),
        Box::new(move |config| {
            Box::new(WatchedModel {
                inner: scripted_model::ScriptedModel::from_config(config),
                controls: Arc::clone(&controls),
            })
        }),
    );
    factories.insert(
        trust_policy::NAME.to_string(),
        Box::new(|config| Box::new(trust_policy::TrustPolicy::from_config(config))),
    );
    factories.insert(
        calc_tools::NAME.to_string(),
        Box::new(|config| Box::new(calc_tools::CalcTools::from_config(config))),
    );
    factories.insert(
        EXPLODING_TOOLS.to_string(),
        Box::new(|_| Box::new(ExplodingTools)),
    );
    factories
}

/// The product's shape: the loop asks the gate, the gate hands on to the
/// provider. `tool_component` picks which provider sits behind the gate.
fn gated_assembly(
    script: serde_json::Value,
    tool_component: &str,
    grants: &str,
) -> AssemblyManifest {
    let inst = |component: &str, config: Option<serde_json::Value>| ComponentInstance {
        component: component.to_string(),
        requires: Vec::new(),
        config,
    };
    AssemblyManifest {
        instances: [
            ("ui".to_string(), inst(silent_ui::NAME, None)),
            ("loop".to_string(), inst(minimal_loop::NAME, None)),
            ("model".to_string(), inst(WATCHED_MODEL, Some(script))),
            (
                "trust".to_string(),
                inst(
                    trust_policy::NAME,
                    Some(json!({"stance": "ask", "grants": grants})),
                ),
            ),
            ("tools".to_string(), inst(tool_component, None)),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            // The two hops that every test before this one skipped
            Wire::new("loop.run", "trust.review"),
            Wire::new("trust.verdict", "loop.tools"),
            Wire::new("trust.forward", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("ui.answer", "trust.answer"),
            Wire::new("loop.out", "ui.display"),
        ],
    }
}

struct Ran {
    events: Vec<EventEnvelope>,
    displayed: Vec<String>,
    controls: Vec<String>,
}

fn run(script: serde_json::Value, tool_component: &str) -> Ran {
    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json").display().to_string();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed), Arc::clone(&controls));
    let mut kernel = Kernel::start(
        &gated_assembly(script, tool_component, &grants),
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
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    let displayed = displayed.lock().unwrap().clone();
    let controls = controls.lock().unwrap().clone();
    Ran {
        events,
        displayed,
        controls,
    }
}

/// A tool name the model invented. Nobody provides it, so the kernel says so
/// — and the loop has to HEAR it, or the round waits forever for a result
/// that was already declared impossible.
#[test]
fn a_tool_nobody_provides_ends_the_turn_even_behind_a_gate() {
    let ran = run(
        json!({"script": [
            {"status": "ok", "toolCalls": [{"tool": "slowest_test", "arguments": {}}]},
            {"status": "ok", "text": "4 + 7 = 11"},
        ]}),
        calc_tools::NAME,
    );

    let settled = ran
        .events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "no_provider")
        .expect("the unprovided call is settled");
    assert_eq!(settled.payload["tool"], "slowest_test");

    // The ending was RECORDED before this fix too. What it was not is heard.
    assert_eq!(
        ran.displayed,
        vec!["4 + 7 = 11".to_string()],
        "the round went on and answered"
    );
    assert!(
        ran.events
            .iter()
            .any(|e| e.event_type == ce::TURN_COMPLETED),
        "the turn ended"
    );
}

/// The provider dies with the request in its hands. Same guarantee, the other
/// way in: the loop is told, rather than waiting on a component that is gone.
#[test]
fn a_provider_dying_behind_a_gate_still_ends_the_round() {
    let ran = run(
        json!({"script": [
            {"status": "ok", "toolCalls": [{"tool": "calc", "arguments": {"numbers": [4, 7]}}]},
            {"status": "ok", "text": "it died"},
        ]}),
        EXPLODING_TOOLS,
    );

    assert!(
        ran.events
            .iter()
            .any(|e| e.event_type == ce::COMPONENT_CRASHED),
        "the provider's death is on the record"
    );
    let settled = ran
        .events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "crash")
        .expect("the call it was holding is settled");
    assert_eq!(settled.payload["component"], "tools");
    assert_eq!(
        ran.displayed,
        vec!["it died".to_string()],
        "the round went on rather than waiting on a dead component"
    );
}

/// The line the witness rule draws, and the reason the walk back through
/// copies of a request stops where it does.
///
/// The dead tool's request descends from the model call the adapter answered.
/// Told about that death, an adapter would treat its own call as cancelled —
/// so a broken tool could cancel the model's work. Nothing about a tool's
/// ending is any of the adapter's business.
#[test]
fn a_dying_tool_never_cancels_the_model_adapters_call() {
    let ran = run(
        json!({"script": [
            {"status": "ok", "toolCalls": [{"tool": "calc", "arguments": {"numbers": [4, 7]}}]},
            {"status": "ok", "text": "it died"},
        ]}),
        EXPLODING_TOOLS,
    );

    assert!(
        ran.events
            .iter()
            .any(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "crash"),
        "precondition: a tool did die and was settled"
    );
    assert!(
        ran.controls.is_empty(),
        "the adapter was told a tool died: {:?}",
        ran.controls
    );
}

/// The other half of "a call always ends": the MODEL's call.
///
/// Death and removal settled tool calls only. An adapter that died holding a
/// call therefore left the loop waiting on an answer with no sender, and the
/// conversation was finished until someone restarted it — no error, no reply,
/// nothing on the ledger to say why. Two things had to be true for the round
/// to close: the kernel had to give that call an ending, and the loop had to
/// understand one when it arrived.
#[test]
fn a_model_adapter_dying_mid_call_ends_the_round_instead_of_hanging_it() {
    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json").display().to_string();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed), Arc::clone(&controls));
    // The adapter in this run answers nothing and dies where it stands.
    factories.insert(
        WATCHED_MODEL.to_string(),
        Box::new(|_| Box::new(ExplodingTools)),
    );
    let mut kernel = Kernel::start(
        &gated_assembly(json!({"script": []}), calc_tools::NAME, &grants),
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
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    let settled = events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "crash")
        .expect("the model call it was holding gets an ending");
    let ended = events
        .iter()
        .find(|e| settled.causes.contains(&e.id))
        .expect("the ending names what it ended");
    assert_eq!(
        ended.event_type,
        ce::MODEL_CALL_STARTED,
        "it is the MODEL call that was settled"
    );
    assert!(
        events.iter().any(|e| e.event_type == ce::TURN_COMPLETED),
        "and the round closed rather than waiting forever"
    );
}

/// Once the adapter is gone, later questions must not hang either.
///
/// "Nobody took this request" was a fact the kernel only stated about tool
/// requests. A model call reaching no one is the same fact, and without it
/// every question asked after an adapter's death waits for an answer that has
/// no sender.
#[test]
fn a_model_call_nobody_takes_is_settled_like_any_other() {
    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json").display().to_string();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed), Arc::clone(&controls));
    let mut kernel = Kernel::start(
        &gated_assembly(json!({"script": []}), calc_tools::NAME, &grants),
        &registry(),
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    // The seat is empty before a word is said.
    kernel
        .uninstall("model", "the adapter is gone", &[])
        .expect("removable");
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "no_provider"),
        "a question nobody can answer is said to be over"
    );
    assert!(
        events.iter().any(|e| e.event_type == ce::TURN_COMPLETED),
        "and the round closes"
    );
}

/// An answer refused at the door still ends the call it was answering.
///
/// The four checks turn a bad emission into an error event and drop it. What
/// gets dropped can be a COMPLETION — and by then the tool has already done
/// the work, so the caller is waiting for an answer that was thrown away. The
/// realistic way in is a foreign component whose letter drifts from the
/// schema by one field: the work happens, the result is refused, the round
/// waits forever.
#[test]
fn a_result_refused_at_the_door_still_ends_the_call() {
    /// Answers every request with a completion whose LETTER is wrong: the
    /// right event type on the right port, and a `status` the canon does not
    /// know. This is what schema drift looks like from a component written
    /// against a slightly different version of the contract — the realistic
    /// way an answer gets refused after the work is already done.
    struct Malformed;
    impl Component for Malformed {
        fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
            ctx.emit(
                "outcome",
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&event.id],
                    json!({"call": event.payload["call"], "status": "finished"}),
                ),
            );
        }
    }

    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json").display().to_string();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed), Arc::clone(&controls));
    factories.insert(
        EXPLODING_TOOLS.to_string(),
        Box::new(|_| Box::new(Malformed)),
    );
    let mut kernel = Kernel::start(
        &gated_assembly(
            json!({"script": [
                {"status": "ok", "toolCalls": [{"tool": "calc", "arguments": {"numbers": [4, 7]}}]},
                {"status": "ok", "text": "it answered badly"},
            ]}),
            EXPLODING_TOOLS,
            &grants,
        ),
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
    let events = kernel.log().replay(1).unwrap();
    let shown = displayed.lock().unwrap().clone();
    kernel.shutdown();

    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::ERROR && e.payload["code"] == "core.audit_rejected"),
        "the malformed answer is refused, and the refusal is on the record"
    );
    let settled = events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "rejected")
        .expect("the call the refused answer belonged to is ended");
    // Ended by way of the request the provider actually received — the
    // gate's forward, which is the copy it witnessed.
    let ended = events
        .iter()
        .find(|e| settled.causes.contains(&e.id))
        .expect("the ending names what it ended");
    assert_eq!(ended.event_type, ce::TOOL_EXEC_STARTED);
    assert_eq!(
        shown,
        vec!["it answered badly".to_string()],
        "and the round went on rather than waiting on an answer that was thrown away"
    );
}

/// The watchman's deadline is an ending like any other, and endings are
/// DELIVERED.
///
/// It was the one that was only written down. A tool that ran past its
/// deadline got a `core.control.interrupted` on the ledger and the requester
/// was never told, so the round waited for a result the kernel had already
/// declared would never come. Nor did the component's own death rescue it:
/// by then the call HAS an ending, and settling twice is refused — correctly.
/// So the turn simply stopped, with the reason sitting on the record.
///
/// Seen in a real session: two searches issued together, one finished, one
/// ran long, and the conversation stopped there for as long as it was left
/// running.
/// A tool that answers and THEN hangs on its way out is past its deadline
/// with the work already done. The watchman must say nothing: the call has an
/// ending, and a second ending is a second answer to one call id, which no
/// wire format accepts. Seen on a real ledger — one call with a completion
/// and three deadline interruptions stacked on top of it.
#[test]
fn a_call_already_answered_is_not_settled_again_when_its_tool_hangs() {
    /// Answers immediately and then never returns from handle. The injector
    /// is what makes that possible: `Ctx::emit` holds its drafts until handle
    /// returns, so a component that hangs mid-handle emits nothing, while the
    /// injector sends as it is called — the same path every background worker
    /// takes.
    struct AnswersThenHangs;
    impl Component for AnswersThenHangs {
        fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
            ctx.injector().emit(
                "outcome",
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&event.id],
                    json!({"call": event.payload["call"], "status": "ok", "result": 11}),
                ),
            );
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json").display().to_string();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed), Arc::clone(&controls));
    factories.insert(
        EXPLODING_TOOLS.to_string(),
        Box::new(|_| Box::new(AnswersThenHangs)),
    );

    let mut registry = registry();
    let mut slow = exploding_manifest();
    slow.handle_timeout_ms = Some(200);
    registry.insert(EXPLODING_TOOLS.to_string(), slow);

    let mut kernel = Kernel::start(
        &gated_assembly(
            json!({"script": [
                {"status": "ok", "toolCalls": [{"tool": "calc", "arguments": {"numbers": [4, 7]}}]},
                {"status": "ok", "text": "the sum is 11"},
            ]}),
            EXPLODING_TOOLS,
            &grants,
        ),
        &registry,
        &mut factories,
        KernelOptions {
            grace_period: std::time::Duration::from_millis(200),
            ..KernelOptions::default()
        },
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED),
        "precondition: the tool answered before it hung"
    );
    // One tool call in this run, so every copy of the request belongs to it
    let started: Vec<&str> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .map(|e| e.id.as_str())
        .collect();
    let endings: Vec<&EventEnvelope> = events
        .iter()
        .filter(|e| ce::is_outcome(&e.event_type))
        .filter(|e| e.causes.iter().any(|c| started.contains(&c.as_str())))
        .collect();
    assert_eq!(
        endings.len(),
        1,
        "the call was answered; the watchman must not write over it: {endings:?}"
    );
    assert_eq!(endings[0].event_type, ce::TOOL_EXEC_COMPLETED);
}

#[test]
fn a_call_that_runs_past_its_deadline_tells_the_one_waiting_on_it() {
    /// Never answers, never looks at its token — the shape of a tool doing
    /// pure computation without checking whether it was cancelled.
    struct Deaf;
    impl Component for Deaf {
        fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json").display().to_string();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let mut factories = factories(Arc::clone(&displayed), Arc::clone(&controls));
    factories.insert(EXPLODING_TOOLS.to_string(), Box::new(|_| Box::new(Deaf)));

    let mut registry = registry();
    // Short enough to keep the test quick.
    let mut deaf = exploding_manifest();
    deaf.handle_timeout_ms = Some(200);
    registry.insert(EXPLODING_TOOLS.to_string(), deaf);

    let mut kernel = Kernel::start(
        &gated_assembly(
            json!({"script": [
                {"status": "ok", "toolCalls": [{"tool": "calc", "arguments": {"numbers": [4, 7]}}]},
                {"status": "ok", "text": "it never answered"},
            ]}),
            EXPLODING_TOOLS,
            &grants,
        ),
        &registry,
        &mut factories,
        KernelOptions {
            grace_period: std::time::Duration::from_millis(200),
            ..KernelOptions::default()
        },
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    let shown = displayed.lock().unwrap().clone();
    kernel.shutdown();

    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "deadline"),
        "the watchman cancelled it"
    );
    // The point: the round went on. Recording the ending without delivering
    // it left this empty and the turn open forever.
    assert_eq!(
        shown,
        vec!["it never answered".to_string()],
        "the requester was told, and the round closed"
    );
    assert!(events.iter().any(|e| e.event_type == ce::TURN_COMPLETED));
}
