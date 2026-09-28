//! Cancellation, liveness and crash containment: the machinery that lets a
//! real (slow, fallible) model adapter exist without freezing or killing the
//! kernel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire, KERNEL_SOURCE,
};

fn manifest(name: &str, timeout_ms: Option<u64>) -> ComponentManifest {
    ComponentManifest {
        name: name.to_string(),
        version: "0.0.0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{name}"),
        inputs: vec![PortDecl::new("in", &[ce::TURN_STARTED])],
        outputs: vec![PortDecl::new("out", &[ce::TURN_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: timeout_ms,
        concurrency: None,
    }
}

/// A pure injection source: declares an output, never handles anything
fn driver_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "driver".to_string(),
        version: "0.0.0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:driver".to_string(),
        inputs: vec![],
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

struct Noop;
impl Component for Noop {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

fn assembly(worker_component: &str) -> AssemblyManifest {
    AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "driver".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "worker".to_string(),
                ComponentInstance {
                    component: worker_component.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "worker.in")],
    }
}

fn start(
    worker_component: &str,
    worker_manifest: ComponentManifest,
    worker_factory: Factory,
    options: KernelOptions,
) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (worker_component.to_string(), worker_manifest),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    factories.insert(worker_component.to_string(), worker_factory);
    Kernel::start(
        &assembly(worker_component),
        &registry,
        &mut factories,
        options,
    )
    .unwrap()
}

fn kick(kernel: &Kernel) {
    kernel
        .injector("driver")
        .emit("out", EventDraft::new(ce::TURN_STARTED, &[], json!({})));
}

// ── The watchman: deadlines cancel unattended work ──────

/// Cooperative long worker: loops until its token is cancelled
struct Sleeper;
impl Component for Sleeper {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        while !ctx.cancelled() {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Also prove the read-back handle: the event being handled is
        // already in the log (recording precedes delivery)
        let saw_self = ctx.log().get(&event.id).unwrap().is_some();
        ctx.emit(
            "out",
            EventDraft::new(
                ce::TURN_COMPLETED,
                &[&event.id],
                json!({"cancelled": true, "sawSelf": saw_self}),
            ),
        );
    }
}

/// The ledger past its opening event.
///
/// Every stream begins with `core.stream.opened` — what runtime, what
/// assembly, when. These tests are about what happened after it.
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
fn deadline_cancels_a_cooperative_worker_without_any_human() {
    let mut kernel = start(
        "sleeper",
        manifest("sleeper", Some(50)),
        Box::new(|_| Box::new(Sleeper)),
        KernelOptions::default(),
    );
    kick(&kernel);
    kernel.run_until_quiescent().unwrap();

    let events = conversation(&kernel);
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec![ce::TURN_STARTED, ce::INTERRUPTED, ce::TURN_COMPLETED]
    );
    // The deadline interrupt is kernel-recorded, causally tied to the work
    assert_eq!(events[1].source, KERNEL_SOURCE);
    assert_eq!(events[1].payload["by"], "deadline");
    assert_eq!(events[1].causes, vec![events[0].id.clone()]);
    // The worker wound down cooperatively and saw itself in the log
    assert_eq!(events[2].payload["cancelled"], true);
    assert_eq!(events[2].payload["sawSelf"], true);
}

// ── The grace period: unresponsiveness becomes a recorded crash ──

/// Ignores its token entirely
struct Stubborn;
impl Component for Stubborn {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[test]
fn unresponsive_worker_is_declared_crashed_after_grace() {
    let mut kernel = start(
        "stubborn",
        manifest("stubborn", Some(30)),
        Box::new(|_| Box::new(Stubborn)),
        KernelOptions {
            grace_period: Duration::from_millis(50),
            ..KernelOptions::default()
        },
    );
    kick(&kernel);
    kernel.run_until_quiescent().unwrap();

    let events = conversation(&kernel);
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec![ce::TURN_STARTED, ce::INTERRUPTED, ce::COMPONENT_CRASHED]
    );
    assert_eq!(events[2].payload["component"], "worker");
    assert!(events[2].payload["detail"]
        .as_str()
        .unwrap()
        .contains("unresponsive"));
}

// ── Panic containment: a crash is an event, not an outage ──

struct Bomb;
impl Component for Bomb {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {
        panic!("boom");
    }
}

#[test]
fn component_panic_is_recorded_and_the_kernel_lives_on() {
    let mut kernel = start(
        "bomb",
        manifest("bomb", None),
        Box::new(|_| Box::new(Bomb)),
        KernelOptions::default(),
    );
    kick(&kernel);
    kernel.run_until_quiescent().unwrap();

    let events = conversation(&kernel);
    let last = events.last().unwrap();
    assert_eq!(last.event_type, ce::COMPONENT_CRASHED);
    assert_eq!(last.payload["component"], "worker");
    assert_eq!(last.causes, vec![events[0].id.clone()]);

    // The kernel is alive: it still records new root events afterwards
    kick(&kernel);
    kernel.run_until_quiescent().unwrap();
    assert_eq!(conversation(&kernel).len(), 3);
}

// ── The bypass: notices reach the host live, never the log ──

struct Streamer;
impl Component for Streamer {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        ctx.notify(json!({"chunk": "he"}));
        ctx.notify(json!({"chunk": "llo"}));
        ctx.emit(
            "out",
            EventDraft::new(ce::TURN_COMPLETED, &[&event.id], json!({"text": "hello"})),
        );
    }
}

#[test]
fn notices_bypass_the_log() {
    let mut kernel = start(
        "streamer",
        manifest("streamer", None),
        Box::new(|_| Box::new(Streamer)),
        KernelOptions::default(),
    );
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    kernel.set_notice_handler(move |source: &str, payload: &Value| {
        sink.lock()
            .unwrap()
            .push(format!("{source}:{}", payload["chunk"].as_str().unwrap()));
    });

    kick(&kernel);
    kernel.run_until_quiescent().unwrap();

    // Fragments arrived live, in order, attributed to their source
    assert_eq!(*seen.lock().unwrap(), vec!["worker:he", "worker:llo"]);
    // And the log contains only real events — no trace of the fragments
    let events = conversation(&kernel);
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(types, vec![ce::TURN_STARTED, ce::TURN_COMPLETED]);
}

/// A component that will not wind down does not make the session unquittable.
///
/// Shutdown waits for every component thread, and the wait had no limit — so
/// one component stuck inside its handler held the whole shutdown open. The
/// components most able to do that are the ones with no per-delivery deadline
/// of their own, which in the standard assembly is the main loop and both
/// gates. "Let what is in flight finish within a limit" was the stated
/// protocol; the limit is the part that was missing.
#[test]
fn shutdown_does_not_wait_forever_on_a_component_that_will_not_stop() {
    struct NeverReturns;
    impl Component for NeverReturns {
        fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {
            // Not cooperative, and with no handle timeout nothing cancels it.
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    let mut kernel = start(
        "stuck",
        // No handle timeout: nothing cancels it, which is the case that
        // could hold shutdown open.
        manifest("stuck", None),
        Box::new(|_| Box::new(NeverReturns)),
        KernelOptions {
            // Everything else short, so the test is quick and only the
            // shutdown limit is under examination.
            stall_timeout: Duration::from_millis(300),
            shutdown_timeout: Duration::from_millis(300),
            ..KernelOptions::default()
        },
    );
    kick(&kernel);
    // A stall is a diagnostic, not quiescence. The host can still ask to
    // stop; synchronize that request with the diagnostic rather than a sleep.
    let stop = kernel.stop_handle();
    kernel.subscribe_log(move |event| {
        if event.payload["code"] == "core.dispatch_stalled" {
            assert!(stop.request("stop stalled fixture".into(), None));
        }
    });
    let (done, completed) = std::sync::mpsc::channel();
    let stopped = std::thread::spawn(move || {
        kernel.run_until_quiescent().unwrap();
        assert!(kernel.is_stopping());
        kernel.shutdown();
        done.send(()).unwrap();
    });
    completed
        .recv_timeout(Duration::from_secs(20))
        .expect("shutdown waited on a component that was never going to return");
    stopped.join().unwrap();
}
