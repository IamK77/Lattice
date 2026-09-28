//! The cross-process bridge: a foreign-language component (Python, stdlib
//! only) joins the assembly as an ordinary citizen — same envelopes, same
//! enforcement, same audit trail. Unix-only, like the bridge itself.
#![cfg(unix)]

use std::collections::HashMap;

use serde_json::json;

use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

fn python_tool() -> ComponentManifest {
    ComponentManifest {
        name: "hash-tool".to_string(),
        version: "0.1.0".to_string(),
        runtime: RuntimeKind::Process,
        entry: "python3 examples/components/hash_tool.py".to_string(),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: Some(10_000),
        concurrency: None,
    }
}

fn driver() -> ComponentManifest {
    ComponentManifest {
        name: "driver".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:driver".to_string(),
        inputs: vec![],
        outputs: vec![PortDecl::new("out", &[ce::TOOL_EXEC_STARTED])],
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

fn start_kernel() -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver()),
        ("hash-tool".to_string(), python_tool()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    // Note: no factory for the Python component — the bridge hosts it
    let assembly = AssemblyManifest {
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
                "py".to_string(),
                ComponentInstance {
                    component: "hash-tool".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "py.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

#[test]
fn a_python_component_is_an_ordinary_citizen() {
    let mut kernel = start_kernel();
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "sha256", "arguments": {"text": "lattice"}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    // Past the stream's own opening event, which every ledger now begins with
    let events: Vec<_> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type != ce::STREAM_OPENED)
        .collect();
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(types, vec![ce::TOOL_EXEC_STARTED, ce::TOOL_EXEC_COMPLETED]);
    let outcome = &events[1];
    assert_eq!(outcome.source, "py");
    // Causality holds across the process boundary
    assert_eq!(outcome.causes, vec![events[0].id.clone()]);
    // sha256("lattice")
    assert_eq!(
        outcome.payload["result"],
        "4cbe09597b76794b5f6b854c1c1c035ede6d241f250f223b61987dce9d2d7a4b"
    );

    // Graceful shutdown must not hang on the child
    kernel.shutdown();
}

#[test]
fn a_dying_child_becomes_a_recorded_crash_and_the_kernel_lives() {
    let mut kernel = start_kernel();
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c2", "tool": "die", "arguments": {}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    let events: Vec<_> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type != ce::STREAM_OPENED)
        .collect();
    let crash = events
        .iter()
        .find(|e| e.event_type == ce::COMPONENT_CRASHED)
        .expect("the crash is recorded");
    assert_eq!(crash.payload["component"], "py");
    assert_eq!(crash.causes, vec![events[0].id.clone()]);

    // And the call it died holding is SETTLED, or whoever was waiting on it
    // would wait forever. Settled as interrupted, never as a result: a
    // component can die with its work half done, and "it failed" would send
    // the model back to redo something that may already have happened.
    let settled = events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "crash")
        .expect("the call it was holding is settled");
    assert!(
        settled.causes.iter().any(|c| *c == events[0].id),
        "settled against the request it was holding"
    );

    // The kernel still records new root events afterwards
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c3", "tool": "sha256", "arguments": {"text": "x"}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    assert_eq!(
        events.last().unwrap().event_type,
        ce::INTERRUPTED,
        "the tool of a DEAD component is now unprovided, and a request for it \
         is settled rather than left hanging: {:?}",
        events.last().unwrap()
    );
    assert_eq!(events.last().unwrap().payload["by"], "no_provider");
}

/// A child that misbehaves in every tolerated and every policed way before
/// doing its job: a garbage line (forward-compat: ignored), a lying emission
/// (an event type its port never declared: refused and RECORDED), then the
/// honest answer. Run twice — after all that, it must still be a citizen.
const CHAOS_PY: &str = r#"
import sys, json
def send(obj):
    print(json.dumps(obj), flush=True)
hello = json.loads(sys.stdin.readline())
for line in sys.stdin:
    msg = json.loads(line)
    if "deliver" in msg:
        ev = msg["deliver"]["event"]
        print("this line is not even json", flush=True)
        send({"emit": {"port": "outcome", "type": "core.made.up",
                       "causes": [ev["id"]], "payload": {}}})
        send({"emit": {"port": "outcome", "type": "core.tool.exec_completed",
                       "causes": [ev["id"]],
                       "payload": {"call": ev["payload"]["call"], "status": "ok",
                                   "result": "done"}}})
        send({"processed": {}})
    elif "stop" in msg:
        break
"#;

#[test]
fn a_misbehaving_child_is_policed_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("chaos_tool.py");
    std::fs::write(&script, CHAOS_PY).unwrap();

    let mut manifest = python_tool();
    manifest.name = "chaos-tool".to_string();
    manifest.entry = format!("python3 {}", script.display());

    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver()),
        ("chaos-tool".to_string(), manifest),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    let assembly = AssemblyManifest {
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
                "py".to_string(),
                ComponentInstance {
                    component: "chaos-tool".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "py.execute")],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();

    // The call-it-again probe: misbehaving in round one must not cost round two
    for call in ["c1", "c2"] {
        kernel.injector("driver").emit(
            "out",
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call": call, "tool": "chaos", "arguments": {}}),
            ),
        );
    }
    kernel.run_until_quiescent().unwrap();

    let events = kernel.log().replay(1).unwrap();
    // The honest answers both landed, in order, from the child
    let outcomes: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "py")
        .collect();
    assert_eq!(outcomes.len(), 2, "the child must stay a citizen");
    assert_eq!(outcomes[0].payload["call"], "c1");
    assert_eq!(outcomes[1].payload["call"], "c2");
    // The lying emissions were refused AND recorded — enforcement as audit,
    // one error per lie, attributed to the kernel
    let lies: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::ERROR && e.source == lattice::KERNEL_SOURCE)
        .collect();
    assert_eq!(
        lies.len(),
        2,
        "each refused emission must be recorded: {events:?}"
    );
    // The garbage line left no trace and killed nothing — tolerated by design
    kernel.shutdown();
}

/// A child that ignores `cancel` is killed rather than waited on forever.
///
/// The kernel's watchman cancels a delivery that runs too long, then declares
/// the component unresponsive if the grace passes — and for a process
/// component that declaration used to be the end of the kernel's involvement:
/// it dropped the mailbox and forgot the instance, while the bridge thread
/// went on polling for an acknowledgement that was never coming and the
/// process, with its whole group, kept running. The one SIGTERM/SIGKILL in
/// the codebase sat on the ordinary shutdown path, which this never reached.
const DEAF_PY: &str = r#"import json, sys, time
for line in sys.stdin:
    msg = json.loads(line)
    if "hello" in msg:
        continue
    if "deliver" in msg:
        # Deaf on purpose: no acknowledgement, and no attention paid to any
        # `cancel` that arrives.
        time.sleep(3600)
"#;

#[test]
fn a_child_that_will_not_stop_is_killed_and_the_kernel_moves_on() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("deaf_tool.py");
    std::fs::write(&script, DEAF_PY).unwrap();
    let marker = dir.path().join("pid.txt");

    let mut manifest = python_tool();
    manifest.name = "deaf-tool".to_string();
    manifest.entry = format!("python3 {}", script.display());
    // Short enough that the test is quick, long enough that the child is
    // certainly inside its sleep when the clock runs out.
    manifest.handle_timeout_ms = Some(300);

    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver()),
        ("deaf-tool".to_string(), manifest),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    let assembly = AssemblyManifest {
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
                "py".to_string(),
                ComponentInstance {
                    component: "deaf-tool".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "py.execute")],
    };
    let options = KernelOptions {
        grace_period: std::time::Duration::from_millis(300),
        ..KernelOptions::default()
    };
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options).unwrap();
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "deaf", "arguments": {}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();

    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::INTERRUPTED && e.payload["by"] == "deadline"),
        "the watchman cancelled the delivery"
    );
    assert!(
        events.iter().any(|e| e.event_type == ce::COMPONENT_CRASHED),
        "and the deaf child was declared unresponsive"
    );
    // The call it was holding has an ending, so whoever asked is not left
    // waiting on a component that is gone. Exactly one: the deadline already
    // ended it, and a second would be a second answer.
    let started = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .expect("the request is on the ledger");
    let endings = events
        .iter()
        .filter(|e| e.event_type == ce::INTERRUPTED && e.causes.contains(&started.id))
        .count();
    assert_eq!(endings, 1, "one ending, no more and no fewer");

    // Shutting down must not hang on the bridge thread: it gave up on the
    // acknowledgement rather than polling for it until the process ended.
    let stopped = std::thread::spawn(move || kernel.shutdown());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !stopped.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        stopped.is_finished(),
        "shutdown waited on a bridge thread that was never going to be answered"
    );
    stopped.join().unwrap();
    let _ = marker;
}
