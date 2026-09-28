//! Injection wakes a turn. A component spawns a background thread holding its
//! own injector; when the thread injects (a stand-in for a background task
//! finishing, a timer firing, a monitor tripping), a host push loop that
//! waits on the wake receiver runs a fresh turn and the injected event lands
//! on the ledger — with the kernel never knowing the words "background" or
//! "timer".
#![cfg(unix)]

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;

use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

/// On its first delivery, spawns a background thread that (after a beat)
/// injects a user message — as itself, through the injector it took from ctx.
struct Waker {
    fired: bool,
}
impl Component for Waker {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, ctx: &mut Ctx) {
        if self.fired {
            return;
        }
        self.fired = true;
        let injector = ctx.injector();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            injector.emit(
                "out",
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "woke up"})),
            );
        });
    }
}

fn manifest(name: &str, input: bool) -> ComponentManifest {
    ComponentManifest {
        name: name.to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{name}"),
        inputs: if input {
            vec![PortDecl::new("kick", &[ce::USER_MESSAGE])]
        } else {
            vec![]
        },
        outputs: vec![PortDecl::new("out", &[ce::USER_MESSAGE])],
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
fn a_background_injection_wakes_a_turn() {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), manifest("driver", false)),
        ("waker".to_string(), manifest("waker", true)),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        "driver".to_string(),
        Box::new(|_| Box::new(Waker { fired: true })),
    );
    factories.insert(
        "waker".to_string(),
        Box::new(|_| Box::new(Waker { fired: false })),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "driver".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "waker".to_string(),
                ComponentInstance {
                    component: "waker".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "waker.kick")],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    let wake_rx = kernel
        .take_wake_receiver()
        .expect("a fresh kernel offers its wake receiver");

    // Kick the waker once (this injection wakes too)
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "kick"})),
    );

    // The host push loop: run to quiescence, then wait for the next wake. A
    // window with no wake means we are quiescent for good.
    loop {
        kernel.run_until_quiescent().unwrap();
        if wake_rx.recv_timeout(Duration::from_millis(300)).is_err() {
            break;
        }
    }

    // The background thread's injection made it onto the ledger — which only
    // happens if its wake pulled the host back for another turn
    let woke = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|e| e.event_type == ce::USER_MESSAGE && e.payload["text"] == "woke up");
    assert!(
        woke,
        "the background injection must have woken a turn and been recorded"
    );

    kernel.shutdown();
}
