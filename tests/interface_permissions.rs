//! Live interface permission is audited state, never a restored grant.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lattice::components::{interface_permissions as permissions, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, EventDraft, EventEnvelope, Factory, Kernel, KernelOptions,
    PortDecl, Wire,
};
use serde_json::{json, Value};

struct FailingAuthority(permissions::InterfacePermissions);
impl lattice::Component for FailingAuthority {
    fn restore(&mut self, ctx: &mut lattice::Ctx) {
        self.0.restore(ctx);
    }
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut lattice::Ctx) {
        match event.payload["action"].as_str() {
            Some("panic") => panic!("injected authority crash"),
            Some("fail") => ctx.fail("test", "injected authority retirement", &[]),
            _ => self.0.handle(port, event, ctx),
        }
    }
}

fn start(path: Option<&Path>) -> Kernel {
    let mut ui = silent_ui::manifest();
    ui.outputs
        .push(PortDecl::new("work", &[ce::MODEL_CALL_STARTED]));
    let mut authority = permissions::manifest();
    authority
        .inputs
        .push(PortDecl::new("hold", &[ce::MODEL_CALL_STARTED]));
    let registry = [
        (silent_ui::NAME.into(), ui),
        (permissions::NAME.into(), authority),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(vec![]))))),
    );
    factories.insert(
        permissions::NAME.into(),
        Box::new(|config| {
            Box::new(FailingAuthority(
                permissions::InterfacePermissions::from_config(config),
            ))
        }),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".into(),
                ComponentInstance {
                    component: silent_ui::NAME.into(),
                    requires: vec![],
                    config: None,
                },
            ),
            (
                "other".into(),
                ComponentInstance {
                    component: silent_ui::NAME.into(),
                    requires: vec![],
                    config: None,
                },
            ),
            (
                "permissions".into(),
                ComponentInstance {
                    component: permissions::NAME.into(),
                    requires: vec![],
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.answer", "permissions.control"),
            Wire::new("ui.work", "permissions.hold"),
            Wire::new("other.answer", "permissions.control"),
        ],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            stream: Some("permission-test".into()),
            log_file: path.map(Path::to_path_buf),
            ..KernelOptions::default()
        },
    )
    .unwrap()
}

fn change(kernel: &mut Kernel, source: &str, id: &str, action: &str, enabled: Value) {
    kernel.injector(source).emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({
                "channel": permissions::CHANNEL, "interface":id, "action":action, "enabled":enabled
            }),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert!(!kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|e| e.event_type == ce::ERROR));
}

fn state(kernel: &Kernel) -> permissions::InterfaceState {
    permissions::read_state(&kernel.log().reader(), "permissions")
        .unwrap()
        .unwrap()
}

fn last(kernel: &Kernel, kind: &str) -> EventEnvelope {
    let events = kernel.log().replay(1).unwrap();
    events
        .iter()
        .rev()
        .find(|e| e.event_type == kind)
        .cloned()
        .unwrap_or_else(|| panic!("Missing {kind}: {events:?}"))
}

fn input(kernel: &mut Kernel, interface: &str) -> EventEnvelope {
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text":"work","interface":interface}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    last(kernel, ce::USER_MESSAGE)
}

fn work(kernel: &mut Kernel, inputs: &[&EventEnvelope]) -> EventEnvelope {
    let ids: Vec<_> = inputs.iter().map(|e| e.id.as_str()).collect();
    kernel.injector("ui").emit(
        "work",
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &ids,
            json!({"model":"test","input":{"parts":[],"fingerprint":"test"},"workInputs":ids}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    last(kernel, ce::MODEL_CALL_STARTED)
}

#[test]
fn a_retired_authority_cannot_keep_granting_its_last_permission() {
    for ending in ["remove", "panic", "fail"] {
        let mut kernel = start(None);
        change(&mut kernel, "ui", "a", "open", Value::Null);
        change(&mut kernel, "ui", "a", "set", json!(true));
        let message = input(&mut kernel, "a");
        let request = work(&mut kernel, &[&message]);
        let reader = kernel.log().reader();
        assert!(permissions::allowance(&reader, &request, "permissions")
            .unwrap()
            .is_some());
        // Removing an unrelated instance must not invalidate this authority.
        kernel
            .uninstall("other", "test unrelated removal", &[])
            .unwrap();
        assert!(permissions::read_state(&reader, "permissions")
            .unwrap()
            .unwrap()
            .permits("a"));
        if ending == "remove" {
            kernel
                .uninstall("permissions", "test authority removal", &[])
                .unwrap();
            let removed = last(&kernel, ce::COMPONENT_REMOVED);
            assert_eq!(removed.payload["instance"], "permissions");
            assert_eq!(removed.payload["component"], permissions::NAME);
        } else {
            kernel.injector("ui").emit(
                "answer",
                EventDraft::new(
                    ce::EXTERNAL_INPUT,
                    &[],
                    json!({"channel":permissions::CHANNEL,"action":ending}),
                ),
            );
            kernel.run_until_quiescent().unwrap();
        }
        assert!(
            permissions::read_state(&reader, "permissions")
                .unwrap()
                .is_none(),
            "{ending}"
        );
        assert!(
            permissions::allowance(&reader, &request, "permissions")
                .unwrap()
                .is_none(),
            "{ending}"
        );
    }
}

#[test]
fn only_contributing_live_interfaces_supply_permission() {
    let mut kernel = start(None);
    change(&mut kernel, "ui", "a", "open", Value::Null);
    change(&mut kernel, "ui", "b", "open", Value::Null);
    change(&mut kernel, "ui", "b", "set", json!(true));
    let a = input(&mut kernel, "a");
    let single = work(&mut kernel, &[&a]);
    assert!(
        permissions::allowance(&kernel.log().reader(), &single, "permissions")
            .unwrap()
            .is_none(),
        "an enabled observer must not authorize another interface's work"
    );
    let b = input(&mut kernel, "b");
    let combined = work(&mut kernel, &[&a, &b]);
    let evidence = permissions::allowance(&kernel.log().reader(), &combined, "permissions")
        .unwrap()
        .unwrap();
    assert_eq!(evidence.interfaces, ["b"]);
    change(&mut kernel, "ui", "b", "close", Value::Null);
    assert!(
        permissions::allowance(&kernel.log().reader(), &combined, "permissions")
            .unwrap()
            .is_none(),
        "closing a contributor ends its permission for later work too"
    );
}

#[test]
fn new_host_identity_after_reopening_does_not_authorize_historical_work() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("flow.jsonl");
    let (old_id, old_work) = {
        let mut kernel = start(Some(&path));
        let id = permissions::new_instance_id(&kernel.log().reader());
        change(&mut kernel, "ui", &id, "open", Value::Null);
        change(&mut kernel, "ui", &id, "set", json!(true));
        let message = input(&mut kernel, &id);
        (id, work(&mut kernel, &[&message]))
    };
    let mut kernel = start(Some(&path));
    let new_id = permissions::new_instance_id(&kernel.log().reader());
    assert_ne!(old_id, new_id);
    change(&mut kernel, "ui", &new_id, "open", Value::Null);
    change(&mut kernel, "ui", &new_id, "set", json!(true));
    assert!(
        permissions::allowance(&kernel.log().reader(), &old_work, "permissions")
            .unwrap()
            .is_none()
    );
    let message = input(&mut kernel, &new_id);
    let new_work = work(&mut kernel, &[&message]);
    assert!(
        permissions::allowance(&kernel.log().reader(), &new_work, "permissions")
            .unwrap()
            .is_some()
    );
}

#[test]
fn permissions_are_independent_for_interfaces_in_the_same_flow() {
    let mut kernel = start(None);
    change(&mut kernel, "ui", "a", "open", Value::Null);
    change(&mut kernel, "ui", "b", "open", Value::Null);
    change(&mut kernel, "ui", "a", "set", json!(true));
    assert!(state(&kernel).permits("a"));
    assert!(!state(&kernel).permits("b"));
    change(&mut kernel, "ui", "a", "close", Value::Null);
    assert!(!state(&kernel).permits("a"));
    change(&mut kernel, "ui", "a", "set", json!(true));
    assert!(!state(&kernel).permits("a"));
}

#[test]
fn permission_is_not_restored_with_the_ledger() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("flow.jsonl");
    {
        let mut kernel = start(Some(&path));
        change(&mut kernel, "ui", "a", "open", Value::Null);
        change(&mut kernel, "ui", "a", "set", json!(true));
        assert!(state(&kernel).permits("a"));
    }
    let mut resumed = start(Some(&path));
    // The runtime boundary already invalidates permission before the new
    // authority's restore snapshot has been drained into the ledger.
    assert!(
        permissions::read_state(&resumed.log().reader(), "permissions")
            .unwrap()
            .is_none()
    );
    resumed.run_until_quiescent().unwrap();
    assert!(!state(&resumed).permits("a"));
    change(&mut resumed, "ui", "b", "open", Value::Null);
    assert!(!state(&resumed).permits("b"));
    assert!(!state(&resumed).permits("a"));
}

#[test]
fn an_unconfigured_controller_cannot_open_or_enable_an_interface() {
    let mut kernel = start(None);
    change(&mut kernel, "ui", "a", "open", Value::Null);
    change(&mut kernel, "other", "a", "set", json!(true));
    change(&mut kernel, "other", "b", "open", Value::Null);
    assert!(!state(&kernel).permits("a"));
    assert!(!state(&kernel).interfaces.contains_key("b"));
    let events = kernel.log().replay(1).unwrap();
    let refusal = events
        .iter()
        .rev()
        .find(|e| e.event_type == permissions::STATE)
        .unwrap();
    assert_eq!(refusal.payload["accepted"], false);
    assert!(!refusal.reason.as_deref().unwrap().is_empty());
    assert_eq!(refusal.causes.len(), 1);
}
