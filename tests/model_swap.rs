//! Changing the model without ending the conversation.
//!
//! The model is three things at once: a dialect (which component speaks to
//! it), an address (endpoint, model id, key) and a profile (window, effort
//! rungs). Only the last of those could ever travel on a call the way the
//! effort setting does. The endpoint and the key are read once when an adapter
//! is built and belong to the INSTANCE, and a different dialect is a different
//! component — so switching model means replacing the occupant of a seat in
//! the assembly, not stamping a field on a request.
//!
//! What makes that survivable is the wiring: both model adapters declare the
//! same ports and claim the same port profile, so the wires into that seat go
//! on meaning what they meant. These tests hold the kernel to it — the seat
//! keeps its wires, an occupant that does not fit is refused before anything
//! is torn down, and the conversation carries on across the swap because the
//! material is a list of ledger pointers rather than anything the old occupant
//! owned.
//!
//! Not covered here, and said plainly rather than left to look covered: a call
//! held by the occupant AT THE MOMENT it is replaced. That path is the same
//! `settle_chain` an uninstall uses (tested there), and reaching it would mean
//! calling `replace` from inside the dispatch loop it runs outside of.

use std::collections::HashMap;

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, ComponentManifest, Factory, Kernel, KernelOptions,
    PortDecl, RuntimeKind, Wire,
};

/// A component whose ports do NOT match a model adapter's — the thing that
/// must be refused. It is not broken; it simply does not belong in that seat.
struct Stranger;
impl lattice::Component for Stranger {
    fn handle(&mut self, _port: &str, _event: &lattice::EventEnvelope, _ctx: &mut lattice::Ctx) {}
}

fn stranger_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "stranger".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:stranger".to_string(),
        // A single input carrying something no wire into the model seat sends
        inputs: vec![PortDecl::new("nudge", &[ce::WAKE])],
        outputs: Vec::new(),
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

fn script(replies: Value) -> Value {
    json!({ "script": replies })
}

/// A minimal conversation: a frontend socket, the loop, and one model seat.
fn kernel_with_model(first: Value) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        ("stranger".to_string(), stranger_manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(std::sync::Arc::default()))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert("stranger".to_string(), Box::new(|_| Box::new(Stranger)));

    let instance = |component: &str, config: Option<Value>| ComponentInstance {
        component: component.to_string(),
        config,
        requires: Vec::new(),
    };
    let assembly = AssemblyManifest {
        instances: [
            ("ui".to_string(), instance(silent_ui::NAME, None)),
            ("loop".to_string(), instance(minimal_loop::NAME, None)),
            (
                "model".to_string(),
                instance(scripted_model::NAME, Some(script(first))),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("valid assembly");
    // The kernel can only build a replacement if it was given the recipes.
    kernel.adopt_factories(factories);
    kernel
}

fn say(kernel: &mut Kernel, text: &str) {
    kernel.injector("ui").emit(
        "user",
        lattice::EventDraft::new(ce::USER_MESSAGE, &[], json!({ "text": text })),
    );
    kernel.run_until_quiescent().expect("the turn runs");
}

fn replies(kernel: &Kernel) -> Vec<String> {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::OUTPUT_REPLY)
        .filter_map(|e| e.payload["text"].as_str().map(str::to_string))
        .collect()
}

/// The whole point: the seat keeps its name and its wires, the new occupant
/// answers on them, and the conversation is the same conversation — the
/// material is a list of ledger pointers, so the model that arrives reads the
/// history the model that left was reading.
#[test]
fn a_replaced_model_answers_on_the_same_wires_and_the_conversation_carries_on() {
    let mut kernel = kernel_with_model(json!([{"status": "ok", "text": "the first one"}]));
    say(&mut kernel, "hello");
    assert_eq!(replies(&kernel), vec!["the first one"]);

    let wiring = |kernel: &Kernel| -> Vec<(String, String)> {
        let mut all: Vec<(String, String)> = kernel
            .assembly()
            .wires
            .iter()
            .map(|w| (w.from.clone(), w.to.clone()))
            .collect();
        all.sort();
        all
    };
    let wires_before = wiring(&kernel);
    kernel
        .replace(
            "model",
            scripted_model::NAME,
            Some(script(json!([{"status": "ok", "text": "the second one"}]))),
            "the user chose another model",
            &[],
        )
        .expect("a component that fits the seat is accepted");

    assert_eq!(
        wiring(&kernel),
        wires_before,
        "not one wire changes — that is what makes this a replacement"
    );

    say(&mut kernel, "hello again");
    assert_eq!(
        replies(&kernel),
        vec!["the first one", "the second one"],
        "the new occupant answered on the wires the old one sat on"
    );

    // The conversation carried over: the second call's material includes the
    // first exchange, which the new instance never saw happen.
    let asks: Vec<usize> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .map(|e| e.payload["input"]["parts"].as_array().map_or(0, Vec::len))
        .collect();
    assert!(
        asks.len() == 2 && asks[1] > asks[0],
        "the second call carries more history than the first, not a fresh start: {asks:?}"
    );
    kernel.shutdown();
}

/// The swap is a decision, on the ledger, with its reason — so "why is it
/// answering differently since Tuesday" is a question the record answers.
#[test]
fn the_swap_lands_on_the_ledger_saying_what_changed_and_why() {
    let mut kernel = kernel_with_model(json!([{"status": "ok", "text": "one"}]));
    kernel
        .replace(
            "model",
            scripted_model::NAME,
            Some(script(json!([]))),
            "the user chose the model \"sonnet\"",
            &[],
        )
        .unwrap();

    let recorded = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::COMPONENT_REPLACED)
        .expect("the swap is recorded");
    assert_eq!(recorded.payload["instance"], "model");
    assert_eq!(recorded.payload["from"], scripted_model::NAME);
    assert_eq!(recorded.payload["to"], scripted_model::NAME);
    assert!(
        recorded.reason.as_deref().unwrap_or("").contains("sonnet"),
        "a decision event carries its reason: {recorded:?}"
    );
    kernel.shutdown();
}

/// An occupant that does not fit the seat is refused BEFORE anything is torn
/// down, and the one that was there goes on working. Discovering the misfit
/// after the running instance had been stopped would leave a conversation with
/// no model at all.
#[test]
fn a_component_that_does_not_fit_the_seat_is_refused_and_nothing_is_lost() {
    let mut kernel = kernel_with_model(json!([
        {"status": "ok", "text": "still here"},
        {"status": "ok", "text": "still here too"},
    ]));
    say(&mut kernel, "hello");

    let problem = kernel
        .replace("model", "stranger", None, "swap in something else", &[])
        .expect_err("a component with the wrong ports cannot take that seat");
    assert!(
        problem.contains("stranger") && problem.contains("model"),
        "the refusal says what did not fit where: {problem}"
    );

    say(&mut kernel, "are you there");
    assert_eq!(
        replies(&kernel),
        vec!["still here", "still here too"],
        "the occupant that was refused a replacement is still answering"
    );
    assert!(
        !kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == ce::COMPONENT_REPLACED),
        "a refused swap records nothing — it did not happen"
    );
    kernel.shutdown();
}

/// Naming something this build does not have is refused by name, not by a
/// panic and not by a silent no-op.
#[test]
fn an_unknown_component_or_instance_is_refused_by_name() {
    let mut kernel = kernel_with_model(json!([]));
    let problem = kernel
        .replace("model", "nothing-like-this", None, "try it", &[])
        .expect_err("there is no such component");
    assert!(problem.contains("nothing-like-this"), "{problem}");

    let problem = kernel
        .replace("no-such-seat", scripted_model::NAME, None, "try it", &[])
        .expect_err("there is no such instance");
    assert!(problem.contains("no-such-seat"), "{problem}");
    kernel.shutdown();
}

/// A kernel whose host shares its factories cannot build an in-process
/// replacement — and must say so rather than half-perform the swap. This is
/// the case a stream template creates: one factory map, many streams.
#[test]
fn a_kernel_given_no_recipes_says_so_instead_of_half_swapping() {
    let mut kernel = kernel_with_model(json!([{"status": "ok", "text": "here"}]));
    // Take the recipes back, as a host that never handed them over would leave
    // things
    kernel.adopt_factories(HashMap::new());
    let problem = kernel
        .replace(
            "model",
            scripted_model::NAME,
            Some(script(json!([]))),
            "swap",
            &[],
        )
        .expect_err("with no recipe there is nothing to build");
    assert!(problem.contains("recipe"), "{problem}");

    say(&mut kernel, "hello");
    assert_eq!(
        replies(&kernel),
        vec!["here"],
        "and the occupant it could not replace is untouched"
    );
    kernel.shutdown();
}
