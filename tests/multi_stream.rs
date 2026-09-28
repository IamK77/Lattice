//! The multi-stream host: many conversations at once, isolated by
//! construction, plus a sidechannel that observes its parent read-only.

use std::collections::HashMap;

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, PortDecl, RuntimeKind, StreamHost, StreamTemplate,
};

/// A minimal chat template: ui → loop → model, model answers in text.
fn chat_template() -> StreamTemplate {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| {
            Box::new(silent_ui::SilentUi::new(std::sync::Arc::new(
                std::sync::Mutex::new(Vec::new()),
            )))
        }),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
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
                "model".to_string(),
                ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"script": [{"status": "ok", "text": "reply"}]})),
                },
            ),
        ]
        .into(),
        wires: vec![
            lattice::Wire::new("ui.user", "loop.input"),
            lattice::Wire::new("loop.ask", "model.request"),
            lattice::Wire::new("model.result", "loop.model"),
            lattice::Wire::new("loop.out", "ui.display"),
        ],
    };
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

#[test]
fn two_streams_are_isolated() {
    let mut host = StreamHost::new([("chat".to_string(), chat_template())].into());
    host.open("alice", "chat").unwrap();
    host.open("bob", "chat").unwrap();

    host.injector("alice", "ui").unwrap().emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hi from alice"})),
    );
    host.run_all().unwrap();

    // Alice's message is on Alice's ledger, and nothing of it reached Bob's.
    // Both ledgers carry their own opening event, so "Bob's is empty" is now
    // "Bob's says nothing was said".
    let alice = host.kernel("alice").unwrap().log().replay(1).unwrap();
    let said = alice
        .iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .expect("alice said something");
    assert_eq!(said.stream, "alice");
    assert_eq!(said.payload["text"], "hi from alice");
    let bob = host.kernel("bob").unwrap().log().replay(1).unwrap();
    assert_eq!(bob.len(), 1, "only its own opening: {bob:?}");
    assert_eq!(bob[0].event_type, ce::STREAM_OPENED);

    // No id may be opened twice
    assert!(host.open("alice", "chat").is_err());
}

/// A sidechannel component: on its one delivery, it reads the PARENT stream
/// read-only and reports how many events it saw there — proving observation
/// without any ability to write into the parent.
struct Peeker {
    parent: String,
}
impl Component for Peeker {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let seen = ctx.foreign_log(&self.parent).map(|r| r.len()).unwrap_or(0);
        ctx.emit(
            "out",
            EventDraft::new(
                ce::TURN_COMPLETED,
                &[&event.id],
                json!({"parent_events": seen}),
            ),
        );
    }
}

fn peeker_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "peeker".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:peeker".to_string(),
        inputs: vec![PortDecl::new("in", &[ce::USER_MESSAGE])],
        outputs: vec![PortDecl::new("out", &[ce::TURN_COMPLETED])],
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

fn peeker_template(parent: &str) -> StreamTemplate {
    // A frontend socket feeds user input to the peeker (injection must come
    // from an output port, as any real frontend does)
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        ("peeker".to_string(), peeker_manifest()),
    ]
    .into();
    let parent = parent.to_string();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| {
            Box::new(silent_ui::SilentUi::new(std::sync::Arc::new(
                std::sync::Mutex::new(Vec::new()),
            )))
        }),
    );
    factories.insert(
        "peeker".to_string(),
        Box::new(move |_| {
            Box::new(Peeker {
                parent: parent.clone(),
            })
        }),
    );
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
                "peeker".to_string(),
                ComponentInstance {
                    component: "peeker".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            lattice::Wire::new("ui.user", "peeker.in"),
            lattice::Wire::new("peeker.out", "ui.display"),
        ],
    };
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

#[test]
fn a_sidechannel_observes_its_parent_read_only() {
    let mut host = StreamHost::new(
        [
            ("chat".to_string(), chat_template()),
            ("side".to_string(), peeker_template("main")),
        ]
        .into(),
    );
    host.open("main", "chat").unwrap();

    // Give the main conversation some history
    host.injector("main", "ui").unwrap().emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "let's discuss X"})),
    );
    host.run_all().unwrap();
    let main_len = host.kernel("main").unwrap().log().len();
    assert!(main_len > 0);
    let main_last = host
        .kernel("main")
        .unwrap()
        .log()
        .replay(main_len as u64)
        .unwrap()[0]
        .id
        .clone();

    // Open a sidechannel derived from main, and inject a rooted event whose
    // origin points back at the main conversation
    host.open_derived("btw", "side", "main").unwrap();
    let root = StreamHost::derived_root(
        ce::USER_MESSAGE,
        "main",
        &main_last,
        json!({"text": "what were we saying?"}),
    );
    // Inject the rooted event through the sidechannel's frontend socket
    host.injector("btw", "ui").unwrap().emit("user", root);
    host.run_stream("btw").unwrap();

    let side = host.kernel("btw").unwrap().log().replay(1).unwrap();
    // The root carries the cross-stream origin — the audit link. Found by
    // type, not by position: every ledger now opens with its own
    // `core.stream.opened`.
    let rooted = side
        .iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .expect("the rooted event reached the sidechannel");
    assert_eq!(rooted.origin.as_ref().unwrap().stream, "main");
    assert_eq!(rooted.origin.as_ref().unwrap().event, main_last);
    // The peeker read the parent's whole history, read-only
    let report = side
        .iter()
        .find(|e| e.event_type == ce::TURN_COMPLETED)
        .unwrap();
    assert_eq!(report.payload["parent_events"], main_len);

    // And observation left the parent completely untouched
    assert_eq!(host.kernel("main").unwrap().log().len(), main_len);
}

#[test]
fn take_hands_the_kernel_over_and_the_host_forgets() {
    let mut host = StreamHost::new([("chat".to_string(), chat_template())].into());
    host.open("main", "chat").unwrap();

    let kernel = host.take("main").expect("an open stream can be taken");
    assert!(host.take("main").is_none(), "taking twice yields nothing");
    assert!(host.kernel("main").is_none(), "the host forgot the stream");

    // The caller owns the lifecycle now — shutdown must work from here
    kernel.shutdown();
}

#[test]
fn a_derived_stream_observes_a_taken_parent_through_its_retained_reader() {
    // The daemon's real shape: parent kernels live on driver threads (taken
    // out of the host), so sidechannels derive from a RETAINED reader — not
    // from the host's table. The observation must be exactly as good.
    let mut host = StreamHost::new(
        [
            ("chat".to_string(), chat_template()),
            ("side".to_string(), peeker_template("main")),
        ]
        .into(),
    );
    host.open("main", "chat").unwrap();
    host.injector("main", "ui").unwrap().emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "history"})),
    );
    host.run_stream("main").unwrap();

    // Parent leaves the table for its own driver; only the reader stays
    let mut parent = host.take("main").unwrap();
    let reader = parent.log().reader();
    let parent_len = reader.len();
    assert!(parent_len > 0);

    host.open_observing("btw", "side", [("main".to_string(), reader)].into(), |_| {})
        .unwrap();
    host.injector("btw", "ui").unwrap().emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "peek"})),
    );
    host.run_stream("btw").unwrap();

    let report_events = host.kernel("btw").unwrap().log().replay(1).unwrap();
    let report = report_events
        .iter()
        .find(|e| e.event_type == ce::TURN_COMPLETED)
        .expect("the peeker reported");
    assert_eq!(report.payload["parent_events"], parent_len);

    // The taken parent kept working the whole time, untouched by the peek
    assert_eq!(parent.log().len(), parent_len);
    parent.run_until_quiescent().unwrap();
    parent.shutdown();
}
