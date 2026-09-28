//! A process-form component may speak without first receiving a delivery.
#![cfg(unix)]

use std::collections::HashMap;
use std::time::Duration;

use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, ComponentManifest, Kernel, KernelOptions, PortDecl,
    RuntimeKind,
};

#[test]
fn a_spontaneous_process_emission_wakes_the_waiting_host() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("waker.py");
    std::fs::write(
        &script,
        r#"
import json
import sys

hello = json.loads(sys.stdin.readline())
assert "hello" in hello
print(json.dumps({"emit": {
    "port": "out", "type": "core.input.user_message", "causes": [],
    "payload": {"text": "spontaneous process emission"}
}}), flush=True)
for line in sys.stdin:
    if "stop" in json.loads(line):
        break
"#,
    )
    .unwrap();
    let component = ComponentManifest {
        name: "waker".into(),
        version: "0.0.0".into(),
        runtime: RuntimeKind::Process,
        entry: format!("python3 {}", script.display()),
        inputs: vec![],
        outputs: vec![PortDecl::new("out", &[ce::USER_MESSAGE])],
        events: vec![],
        default_wiring: vec![],
        capabilities: None,
        implements: vec![],
        tools: vec![],
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let assembly = AssemblyManifest {
        instances: [(
            "waker".into(),
            ComponentInstance {
                component: "waker".into(),
                config: None,
                requires: vec![],
            },
        )]
        .into(),
        wires: vec![],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &[("waker".into(), component)].into(),
        &mut HashMap::new(),
        KernelOptions::default(),
    )
    .unwrap();
    let wake = kernel.take_wake_receiver().unwrap();
    // No driver injection and no polling of the central inbox: only the
    // process can wake this host. The timeout bounds a missing notification.
    let notified = wake.recv_timeout(Duration::from_secs(5));
    kernel.run_until_quiescent().unwrap();
    // Reap the fixture even when the old bridge fails to send its wake.
    let log = kernel.shutdown();
    assert!(
        log.replay(1)
            .unwrap()
            .iter()
            .any(|event| event.event_type == ce::USER_MESSAGE
                && event.payload["text"] == "spontaneous process emission"),
        "the child really emitted and its event reached the central inbox"
    );
    assert!(
        notified.is_ok(),
        "a process emission must wake a host waiting outside the kernel run loop"
    );
}
