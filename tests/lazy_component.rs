//! A component's DECLARATION and its IMPLEMENTATION need not arrive together.
//!
//! The manifest — ports, tools, prompt fragment — is what the model sees, and
//! it is cheap and stable. The process behind it, and whatever that process
//! loads when it starts, is the expensive half. `lazy` in an instance's config
//! separates them: the declaration is on the books from the first turn, the
//! child starts the first time something is actually delivered to it.
//!
//! The point is what does NOT change: the tool list is byte-identical either
//! way, so the prompt cache never notices.
#![cfg(unix)]

use std::collections::HashMap;

use serde_json::json;

use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

fn driver_manifest() -> ComponentManifest {
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

fn probe_manifest(marker: &std::path::Path) -> ComponentManifest {
    ComponentManifest {
        name: "probe".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Process,
        entry: format!(
            "python3 examples/components/lazy_probe.py {}",
            marker.display()
        ),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["tool-provider".to_string()],
        tools: vec![json!({
            "name": "echo",
            "description": "give back what it was given",
            "parameters": {"type": "object", "properties": {"text": {"type": "string"}}},
            "effects": {"reversible": true},
        })],
        prompt: Some("There is an `echo` tool.".to_string()),
        handle_timeout_ms: Some(10_000),
        concurrency: None,
    }
}

fn kernel_with(marker: &std::path::Path, lazy: bool) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        ("probe".to_string(), probe_manifest(marker)),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), ComponentInstance::new("driver", None)),
            (
                "probe".to_string(),
                ComponentInstance::new("probe", lazy.then(|| json!({"lazy": true}))),
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "probe.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the assembly is valid either way")
}

fn call(kernel: &mut Kernel, id: &str, text: &str) -> serde_json::Value {
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": id, "tool": "echo", "arguments": {"text": text}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rev()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == id)
        .expect("an outcome")
        .payload
}

#[test]
fn a_lazy_component_does_not_start_until_it_is_asked() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let mut kernel = kernel_with(&marker, true);

    assert!(
        !marker.exists(),
        "nothing was delivered, so nothing was started"
    );

    let out = call(&mut kernel, "c1", "hello");
    assert_eq!(out["status"], "ok", "{out}");
    assert_eq!(out["result"], "hello");
    assert!(marker.exists(), "the first delivery started it");
    let started_once = std::fs::read_to_string(&marker).unwrap();

    // And it is reused, not restarted, for the next call.
    let again = call(&mut kernel, "c2", "world");
    assert_eq!(again["result"], "world", "{again}");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        started_once,
        "the second call reused the child"
    );
    kernel.shutdown();
}

/// The semantic difference, stated where it is decidable. A marker file only
/// says when the CHILD got around to running, which is its own business and
/// none of ours; what the kernel decides is WHEN IT TRIES. Eager tries at
/// build, so a program that is not there fails the build. Lazy does not try,
/// so the same assembly starts — and pays for it at the first call instead.
#[test]
fn eager_tries_at_build_and_lazy_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = probe_manifest(&dir.path().join("never"));
    manifest.entry = "definitely-not-a-program-here".to_string();

    for (lazy, should_build) in [(false, false), (true, true)] {
        let registry: HashMap<String, ComponentManifest> = [
            ("driver".to_string(), driver_manifest()),
            ("probe".to_string(), manifest.clone()),
        ]
        .into();
        let mut factories: HashMap<String, Factory> = HashMap::new();
        factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
        let assembly = AssemblyManifest {
            instances: [
                ("driver".to_string(), ComponentInstance::new("driver", None)),
                (
                    "probe".to_string(),
                    ComponentInstance::new("probe", lazy.then(|| json!({"lazy": true}))),
                ),
            ]
            .into(),
            wires: vec![Wire::new("driver.out", "probe.execute")],
        };
        let built = Kernel::start(
            &assembly,
            &registry,
            &mut factories,
            KernelOptions::default(),
        );
        assert_eq!(
            built.is_ok(),
            should_build,
            "lazy={lazy}: a missing program {} stop the build",
            if should_build { "must not" } else { "must" }
        );
        if let Ok(kernel) = built {
            kernel.shutdown();
        }
    }
}

/// The reason this is worth having: deferring the process changes NOTHING the
/// model sees. The tool list and the prompt fragment come from the manifest,
/// so they are identical byte for byte — and the cached prefix of every call
/// is therefore untouched.
#[test]
fn deferring_the_process_changes_nothing_the_model_sees() {
    let dir = tempfile::tempdir().unwrap();
    let eager = kernel_with(&dir.path().join("a"), false);
    let (eager_tools, eager_prompts) = (eager.tool_decls(), eager.prompt_fragments());
    eager.shutdown();

    let lazy = kernel_with(&dir.path().join("b"), true);
    assert_eq!(lazy.tool_decls(), eager_tools, "the same tools are offered");
    assert_eq!(
        lazy.prompt_fragments(),
        eager_prompts,
        "and the same prompt fragment"
    );
    lazy.shutdown();
}

/// A lazy child that cannot start fails at the moment of asking — and the call
/// that asked is settled by the kernel rather than answered with an invented
/// failure. A component can die with its work half done; the one thing that
/// must never be said is "this failed", because a model told that retries.
#[test]
fn a_lazy_child_that_will_not_start_settles_the_call_that_woke_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = probe_manifest(&dir.path().join("never"));
    manifest.entry = "definitely-not-a-program-here".to_string();
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        ("probe".to_string(), manifest),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), ComponentInstance::new("driver", None)),
            (
                "probe".to_string(),
                ComponentInstance::new("probe", Some(json!({"lazy": true}))),
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "probe.execute")],
    };
    // Lazy, so a missing program does not stop the build: nothing tried to run
    // it yet. That is the trade — the failure moves from build time to use.
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("a lazy child is not started at build, so it cannot fail there");

    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "echo", "arguments": {"text": "x"}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    let events = kernel.log().replay(1).unwrap();
    let settled = events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED)
        .expect("the call was settled, not left hanging");
    assert_eq!(settled.payload["by"], "crash", "{}", settled.payload);
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED),
        "and never answered with an invented result"
    );
    kernel.shutdown();
}
