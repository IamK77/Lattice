//! Hot install: a component joins the RUNNING assembly — inspected first,
//! recorded as a decision-class event, usable immediately. The last bridge
//! before "the agent builds its own tool".
#![cfg(unix)]

use std::collections::HashMap;

use serde_json::json;

use lattice::conformance::examine_tool_provider;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelError, KernelOptions, PortDecl, RuntimeKind, Wire,
    KERNEL_SOURCE,
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
        implements: vec!["tool-provider".to_string()],
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

/// A kernel that starts with only the driver — the tool arrives later
fn bare_kernel() -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [("driver".to_string(), driver())].into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    let assembly = AssemblyManifest {
        instances: [(
            "driver".to_string(),
            ComponentInstance {
                component: "driver".to_string(),
                requires: Vec::new(),
                config: None,
            },
        )]
        .into(),
        wires: vec![],
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
fn a_component_installed_mid_run_works_immediately() {
    let mut kernel = bare_kernel();

    kernel
        .install(
            python_tool(),
            "py",
            None,
            &[Wire::new("driver.out", "py.execute")],
            "the agent asked for hashing powers",
            &[],
        )
        .unwrap();

    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "sha256", "arguments": {"text": "lattice"}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    // Past the stream's own opening event, which every ledger begins with
    let events: Vec<_> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type != ce::STREAM_OPENED)
        .collect();
    // The installation itself is on the record: decision-class, with reason
    assert_eq!(events[0].event_type, ce::COMPONENT_INSTALLED);
    assert_eq!(events[0].source, KERNEL_SOURCE);
    assert!(events[0].reason.as_deref().unwrap().contains("hashing"));
    // And the freshly installed component answers like any citizen
    let outcome = events.last().unwrap();
    assert_eq!(outcome.event_type, ce::TOOL_EXEC_COMPLETED);
    assert_eq!(outcome.source, "py");
    assert_eq!(
        outcome.payload["result"],
        "4cbe09597b76794b5f6b854c1c1c035ede6d241f250f223b61987dce9d2d7a4b"
    );
    kernel.shutdown();
}

#[test]
fn a_bad_install_is_refused_and_nothing_changes() {
    let mut kernel = bare_kernel();
    let err = kernel
        .install(
            python_tool(),
            "py",
            None,
            &[Wire::new("driver.out", "py.no_such_port")],
            "doomed",
            &[],
        )
        .unwrap_err();
    assert!(matches!(err, KernelError::Inspection(_)));
    // Nothing was recorded beyond the stream's own opening, nothing spawned
    let events = kernel.log().replay(1).unwrap();
    assert_eq!(
        events.len(),
        1,
        "the refused install left no trace: {events:?}"
    );
    assert_eq!(events[0].event_type, ce::STREAM_OPENED);
}

#[test]
fn the_python_tool_passes_the_tool_provider_exam() {
    let problems = examine_tool_provider(&python_tool(), None, "sha256");
    assert_eq!(problems, Vec::<String>::new());
}

/// Claims the profile, answers nothing
struct Mute;
impl Component for Mute {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

#[test]
fn a_mute_tool_provider_fails_the_exam() {
    let mut manifest = python_tool();
    manifest.name = "mute-tool".to_string();
    manifest.runtime = RuntimeKind::Inproc;
    let problems = examine_tool_provider(&manifest, Some(Box::new(|_| Box::new(Mute))), "sha256");
    assert!(problems
        .iter()
        .any(|p| p.contains("exactly one tool_exec_completed")));
}
