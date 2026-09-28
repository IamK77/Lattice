//! Ordinary tool-provider wiring; no live model or language-service download.
use lattice::components::code_tools;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};
use serde_json::{json, Value};
use std::collections::HashMap;

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}

fn kernel(config: Value) -> Kernel {
    let driver = ComponentManifest {
        name: "driver".into(),
        version: "0".into(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:driver".into(),
        inputs: vec![],
        outputs: vec![PortDecl::new("out", &[ce::TOOL_EXEC_STARTED])],
        events: vec![],
        default_wiring: vec![],
        capabilities: None,
        implements: vec![],
        tools: vec![],
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let registry: HashMap<_, _> = [
        ("driver".into(), driver),
        (code_tools::NAME.into(), code_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".into(), Box::new(|_| Box::new(Driver)));
    factories.insert(
        code_tools::NAME.into(),
        Box::new(|config| Box::new(code_tools::CodeTools::from_config(config))),
    );
    let assembly = AssemblyManifest {
        instances: [
            ("driver".into(), ComponentInstance::new("driver", None)),
            (
                "code".into(),
                ComponentInstance::new(code_tools::NAME, Some(config)),
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "code.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

fn call(kernel: &mut Kernel, id: &str, args: Value) -> Value {
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":id,"tool":"Code","arguments":args}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rev()
        .find(|event| event.event_type == ce::TOOL_EXEC_COMPLETED && event.payload["call"] == id)
        .expect("one tool completion")
        .payload
}

#[test]
fn standard_tool_envelope_keeps_original_source_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rs");
    let body = "fn sample() { /* 中文😀 */ }\n";
    std::fs::write(&path, body).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/components/code_tools/fake_server.py");
    let trace = dir.path().join("trace.jsonl");
    let mut kernel = kernel(json!({"servers":{"rust":{"command":["python3",script,trace]}}}));
    let result = call(
        &mut kernel,
        "one",
        json!({"workspace":dir.path(),"path":"a.rs","action":"read","symbol":"sample"}),
    );
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["result"]["content"], body);
    assert!(result["result"]["fileVersion"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let before = std::fs::read(&trace).unwrap();
    kernel.run_until_quiescent().unwrap();
    let completions = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == ce::TOOL_EXEC_COMPLETED)
        .count();
    assert_eq!(completions, 1);
    assert_eq!(std::fs::read(trace).unwrap(), before);
}

#[test]
fn absent_server_is_an_error_not_a_text_search_substitute() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn sample() {}\n").unwrap();
    let absent = dir.path().join("not-an-installed-server");
    let mut kernel = kernel(json!({"servers":{"rust":{"command":[absent]}}}));
    let result = call(
        &mut kernel,
        "missing",
        json!({"workspace":dir.path(),"path":"a.rs"}),
    );
    assert_eq!(result["status"], "error", "{result}");
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cannot start language server"));
    assert!(!absent.exists());
}
