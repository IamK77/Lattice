//! How far the redactor reaches — a boundary, not an accident.
//!
//! It covers CONVERSATION: what the person said, what the model said. It does
//! not cover a tool's arguments or a tool's result, and that is the decision
//! rather than an oversight (user's call, 2026-07-29).
//!
//! The reason is that the kernel appends BEFORE it routes, so whatever
//! `append` changes is also what the component receives. In a tool request
//! that is not hiding, it is EDITING: a read stops returning what is on disk
//! and a write stops writing what it was told to. It destroyed two API keys
//! in a real catalog — the agent read the file, the result came back with the
//! key replaced, and writing the file back put the replacement where the key
//! had been.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

/// Records the arguments exactly as they were delivered.
struct Scribe(Arc<Mutex<Vec<String>>>);
impl Component for Scribe {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        self.0
            .lock()
            .unwrap()
            .push(event.payload["arguments"]["content"].to_string());
        ctx.emit(
            "outcome",
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&event.id],
                json!({"call": event.payload["call"], "status": "ok", "result": "written"}),
            ),
        );
    }
}

struct Noop;
impl Component for Noop {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

fn manifest(name: &str, input: Option<&str>, output: (&str, &str)) -> ComponentManifest {
    ComponentManifest {
        name: name.to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{name}"),
        inputs: input
            .map(|p| vec![PortDecl::new(p, &[ce::TOOL_EXEC_STARTED])])
            .unwrap_or_default(),
        outputs: vec![PortDecl::new(output.0, &[output.1])],
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

/// driver → scribe, with one string the redactor is told to hide.
fn scribe_kernel(seen: &Arc<Mutex<Vec<String>>>, secret: &str) -> (Kernel, ()) {
    let mut driver = manifest("driver", None, ("out", ce::TOOL_EXEC_STARTED));
    driver
        .outputs
        .push(PortDecl::new("said", &[ce::USER_MESSAGE]));
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver),
        (
            "scribe".to_string(),
            manifest(
                "scribe",
                Some("write"),
                ("outcome", ce::TOOL_EXEC_COMPLETED),
            ),
        ),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    let for_component = Arc::clone(seen);
    factories.insert(
        "scribe".to_string(),
        Box::new(move |_| Box::new(Scribe(Arc::clone(&for_component)))),
    );
    let instance = |component: &str| ComponentInstance {
        component: component.to_string(),
        requires: Vec::new(),
        config: None,
    };
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), instance("driver")),
            ("scribe".to_string(), instance("scribe")),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "scribe.write")],
    };
    let kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            redact: vec![secret.to_string()],
            ..KernelOptions::default()
        },
    )
    .unwrap();
    (kernel, ())
}

/// A tool request passes through untouched, secret and all.
///
/// This is the boundary. The redactor's job is to keep a secret out of the
/// conversation; a tool request is a machine operation and has to be exact.
/// Editing it does not hide anything — the component still acts, just on
/// something else than it was told.
#[test]
fn a_tool_request_reaches_the_component_exactly_as_it_was_written() {
    let secret = "sk-averylongsecretvalue-1234567890";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (mut kernel, _) = scribe_kernel(&seen, secret);
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "Write",
                   "arguments": {"path": "/tmp/x", "content": format!("key = {secret}")}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let ledger = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    let delivered = seen.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1, "the request arrived");
    assert!(
        delivered[0].contains(secret),
        "what the component was told to write is what it was told to write: {}",
        delivered[0]
    );
    // And the record says the same, because a tool request is not conversation
    let whole = serde_json::to_string(&ledger).unwrap();
    assert!(
        whole.contains(secret),
        "a tool request is recorded as it happened"
    );
    assert!(
        !whole.contains("[redacted]"),
        "nothing here is conversation: {whole}"
    );
}

/// The conversation IS covered: a secret in what the model said is hidden.
#[test]
fn a_secret_the_model_repeats_is_kept_out_of_the_conversation() {
    let secret = "sk-averylongsecretvalue-1234567890";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (mut kernel, _) = scribe_kernel(&seen, secret);
    kernel.injector("driver").emit(
        "said",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": format!("my key is {secret}, keep it safe")}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let whole = serde_json::to_string(&kernel.log().replay(1).unwrap()).unwrap();
    kernel.shutdown();
    assert!(!whole.contains(secret), "the conversation must not hold it");
    assert!(
        whole.contains("[redacted]"),
        "and says where it was: {whole}"
    );
}

/// The redactor knows the strings it was handed at startup and nothing else.
///
/// So even inside the conversation, where it does apply, a key the agent
/// obtained at RUNTIME — from an API it logged into, from a file nobody
/// named — is recorded in full. Runtime discovery does not extend the
/// startup redaction set.
#[test]
fn a_secret_nobody_named_is_recorded_in_full() {
    let known = "sk-thisoneisknown-000000000000";
    let fetched = "sk-thisonewasfetchedatruntime-111111";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (mut kernel, _) = scribe_kernel(&seen, known);
    kernel.injector("driver").emit(
        "said",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": format!("a={known} b={fetched}")}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let whole = serde_json::to_string(&kernel.log().replay(1).unwrap()).unwrap();
    kernel.shutdown();

    assert!(
        !whole.contains(known),
        "the one it was told about is hidden"
    );
    assert!(
        whole.contains(fetched),
        "the one nobody named is recorded in full"
    );
}
