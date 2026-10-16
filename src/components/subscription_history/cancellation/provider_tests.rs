//! Exercise real provider handlers through a forwarding/denying gate. The
//! probe runs inside handle, after the receipt is buffered but before commit.

use super::*;
use crate::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventEnvelope, Factory,
    Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Duration;

type Probe = (bool, bool);
type Opened = (
    Kernel,
    mpsc::Receiver<Arc<LiveCancellations>>,
    mpsc::Receiver<Probe>,
);

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}

struct Gate {
    deny: bool,
}
impl Component for Gate {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if self.deny {
            ctx.emit(
                "outcome",
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&event.id],
                    json!({
                        "call":event.payload["call"], "status":"error", "error":{
                            "code":"test.denied", "message":"Cancellation denied by test gate",
                            "blame":"request", "retryable":false, "transient":false
                        }
                    }),
                ),
            );
        } else {
            ctx.emit(
                "forward",
                EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], event.payload.clone()),
            );
        }
    }
}

struct Probed<T> {
    inner: T,
    inspect: fn(&T) -> Arc<LiveCancellations>,
    restored: mpsc::Sender<Arc<LiveCancellations>>,
    handled: mpsc::Sender<Probe>,
}
impl<T: Component> Component for Probed<T> {
    fn restore(&mut self, ctx: &mut Ctx) {
        self.inner.restore(ctx);
        self.restored.send((self.inspect)(&self.inner)).unwrap();
    }

    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tracker = (self.inspect)(&self.inner);
        let before = tracker.stopped(999).unwrap();
        self.inner.handle(port, event, ctx);
        let buffered = tracker.stopped(999).unwrap();
        self.handled.send((before, buffered)).unwrap();
    }
}

fn manifest(name: &str) -> ComponentManifest {
    ComponentManifest {
        name: name.into(),
        version: "0".into(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{name}"),
        inputs: vec![],
        outputs: vec![],
        events: vec![],
        default_wiring: vec![],
        capabilities: None,
        implements: vec![],
        tools: vec![],
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

fn open<T: Component + 'static>(
    path: &Path,
    provider: ComponentManifest,
    create: fn() -> T,
    inspect: fn(&T) -> Arc<LiveCancellations>,
    deny: bool,
) -> Opened {
    let mut driver = manifest("driver");
    driver
        .outputs
        .push(PortDecl::new("out", &[ce::TOOL_EXEC_STARTED]));
    let mut gate = manifest("gate");
    gate.inputs
        .push(PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED]));
    gate.outputs = vec![
        PortDecl::new("forward", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED]),
    ];
    let provider_name = provider.name.clone();
    let registry = [
        (driver.name.clone(), driver),
        (gate.name.clone(), gate),
        (provider_name.clone(), provider),
    ]
    .into();
    let (restored_tx, restored_rx) = mpsc::channel();
    let (handled_tx, handled_rx) = mpsc::channel();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".into(), Box::new(|_| Box::new(Driver)));
    factories.insert("gate".into(), Box::new(move |_| Box::new(Gate { deny })));
    factories.insert(
        provider_name.clone(),
        Box::new(move |_| {
            Box::new(Probed {
                inner: create(),
                inspect,
                restored: restored_tx.clone(),
                handled: handled_tx.clone(),
            })
        }),
    );
    let assembly = AssemblyManifest {
        instances: [
            ("driver", "driver"),
            ("gate", "gate"),
            ("provider", provider_name.as_str()),
        ]
        .into_iter()
        .map(|(id, component)| {
            (
                id.into(),
                ComponentInstance {
                    component: component.into(),
                    config: None,
                    requires: vec![],
                },
            )
        })
        .collect(),
        wires: vec![
            Wire::new("driver.out", "gate.execute"),
            Wire::new("gate.forward", "provider.execute"),
        ],
    };
    let kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            stream: Some("provider-cancellation".into()),
            log_file: Some(path.to_path_buf()),
            ..Default::default()
        },
    )
    .unwrap();
    (kernel, restored_rx, handled_rx)
}

pub(in crate::components) fn assert_record_first<T: Component + 'static>(
    kind: Kind,
    provider: ComponentManifest,
    create: fn() -> T,
    inspect: fn(&T) -> Arc<LiveCancellations>,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("provider.ledger");
    let (_, stop, field) = kind.tools();
    let mut original_boundary = None;
    // Denied, then accepted after reopening, then reopened once more. The
    // future numeric id deliberately needs no actual timer or OS watch.
    for (life, deny) in [(0, true), (1, false), (2, false)] {
        let (mut kernel, restored, handled) = open(&path, provider.clone(), create, inspect, deny);
        let tracker = restored.recv_timeout(Duration::from_secs(5)).unwrap();
        kernel.run_until_quiescent().unwrap();
        assert_eq!(tracker.stopped(999).unwrap(), life == 2);
        if life < 2 {
            kernel.injector("driver").emit(
                "out",
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[],
                    json!({
                        "call":format!("stop-{life}"), "tool":stop, "arguments":{(field):999}
                    }),
                ),
            );
            kernel.run_until_quiescent().unwrap();
            let outcomes: Vec<_> = kernel
                .log()
                .replay(1)
                .unwrap()
                .into_iter()
                .filter(|event| {
                    event.event_type == ce::TOOL_EXEC_COMPLETED
                        && event.payload["call"] == format!("stop-{life}")
                })
                .collect();
            assert_eq!(
                outcomes.len(),
                1,
                "one cancellation call has exactly one outcome"
            );
            if deny {
                assert!(
                    handled.try_recv().is_err(),
                    "the denied request must not reach the provider"
                );
                assert_eq!(outcomes[0].payload["status"], "error");
                assert!(!tracker.stopped(999).unwrap());
            } else {
                assert_eq!(
                    handled.recv_timeout(Duration::from_secs(5)).unwrap(),
                    (false, false),
                    "the real handler must not cancel before its buffered receipt is committed"
                );
                assert_eq!(outcomes[0].payload["result"]["cancellation"]["v"], 1);
                assert!(
                    tracker.stopped(999).unwrap(),
                    "the committed receipt must cancel"
                );
                kernel.injector("driver").emit(
                    "out",
                    EventDraft::new(
                        ce::TOOL_EXEC_STARTED,
                        &[],
                        json!({
                            "call":"repeat-stop", "tool":stop, "arguments":{(field):999}
                        }),
                    ),
                );
                kernel.run_until_quiescent().unwrap();
                assert_eq!(
                    handled.recv_timeout(Duration::from_secs(5)).unwrap(),
                    (true, true)
                );
                let repeated: Vec<_> = kernel
                    .log()
                    .replay(1)
                    .unwrap()
                    .into_iter()
                    .filter(|event| {
                        event.event_type == ce::TOOL_EXEC_COMPLETED
                            && event.payload["call"] == "repeat-stop"
                    })
                    .collect();
                assert_eq!(repeated.len(), 1);
                assert_eq!(repeated[0].payload["status"], "ok");
            }
        }
        let restored_state =
            crate::components::subscription_history::recover(&kernel.log().reader(), kind).unwrap();
        assert_eq!(
            restored_state.state.future_cancelled.contains(&999),
            life > 0
        );
        let boundary = restored_state.state.protocol.legacy_through.unwrap();
        assert_eq!(
            *original_boundary.get_or_insert(boundary),
            boundary,
            "reopening must not move the legacy boundary"
        );
        if life == 2 {
            // A semantically invalid history record exercises the real worker
            // stop/error-report boundary without corrupting the writer's file.
            kernel
                .injector("provider")
                .emit("state", activation(kind, boundary + 1));
            kernel.run_until_quiescent().unwrap();
            assert!(tracker.stopped_or_report(1000, &kernel.injector("provider"), None));
            kernel.run_until_quiescent().unwrap();
            let diagnostics: Vec<_> = kernel
                .log()
                .replay(1)
                .unwrap()
                .into_iter()
                .filter(|event| {
                    event.event_type == ce::WAKE
                        && event.payload["body"]["interrupted"] == "cancellation_history_unreadable"
                })
                .collect();
            assert_eq!(
                diagnostics.len(),
                1,
                "an unreadable history must be reported, not treated as an empty set"
            );
            assert_eq!(diagnostics[0].payload["body"][field], 1000);
            assert!(!diagnostics[0].payload["body"]["error"]
                .as_str()
                .unwrap()
                .is_empty());
        }
        kernel.shutdown();
    }
}
