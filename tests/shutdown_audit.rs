//! Offline regression for exit cancellation, independent of any real model.

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::{
    core_events as ce, AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx,
    EventDraft, EventEnvelope, Factory, Kernel, KernelOptions, Session, Wire,
};
use serde_json::json;

struct Held {
    entered: mpsc::Sender<tokio_util::sync::CancellationToken>,
    release: Arc<Mutex<mpsc::Receiver<()>>>,
}
impl Component for Held {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if port != "request" {
            return;
        }
        self.entered.send(ctx.cancellation()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        ctx.emit(
            "result",
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[&event.id],
                json!({"status":"ok", "text":"released by audit fixture"}),
            ),
        );
    }
}

struct InterruptWitness(mpsc::Sender<()>);
impl Component for InterruptWitness {
    fn handle(&mut self, port: &str, event: &EventEnvelope, _: &mut Ctx) {
        if port == "control" && event.event_type == ce::INTERRUPTED {
            let _ = self.0.send(());
        }
    }
}

#[test]
fn shutdown_must_cancel_an_active_model_without_a_user_interrupt_wire() {
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let release = Arc::new(Mutex::new(release));
    let (witness_tx, witnessed) = mpsc::channel();
    let session = Session::spawn("ui", move |_render| {
        let registry: HashMap<String, ComponentManifest> = [
            (silent_ui::NAME.into(), silent_ui::manifest()),
            (minimal_loop::NAME.into(), minimal_loop::manifest()),
            (scripted_model::NAME.into(), scripted_model::manifest()),
        ]
        .into();
        let entered = entered_tx.clone();
        let release = Arc::clone(&release);
        let witness = witness_tx.clone();
        let mut factories: HashMap<String, Factory> = HashMap::new();
        factories.insert(
            silent_ui::NAME.into(),
            Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
        );
        factories.insert(
            minimal_loop::NAME.into(),
            Box::new(|config| Box::new(minimal_loop::MinimalLoop::from_config(config))),
        );
        factories.insert(
            scripted_model::NAME.into(),
            Box::new(move |config| {
                if config.and_then(|c| c.get("hold")).and_then(|v| v.as_bool()) == Some(true) {
                    Box::new(Held {
                        entered: entered.clone(),
                        release: Arc::clone(&release),
                    })
                } else {
                    Box::new(InterruptWitness(witness.clone()))
                }
            }),
        );
        let instance = |component: &str, config| ComponentInstance {
            component: component.into(),
            requires: Vec::new(),
            config,
        };
        let assembly = AssemblyManifest {
            instances: [
                ("ui".into(), instance(silent_ui::NAME, None)),
                ("loop".into(), instance(minimal_loop::NAME, None)),
                ("model".into(), instance(scripted_model::NAME, None)),
                (
                    "cmodel".into(),
                    instance(scripted_model::NAME, Some(json!({"hold":true}))),
                ),
            ]
            .into(),
            // Exercise the cancellation topology of the standard assembly:
            // a model call is active at cmodel, but user interrupt goes to model.
            // The minimal loop starts that call here instead of the context gate.
            wires: vec![
                Wire::new("ui.user", "loop.input"),
                Wire::new("ui.interrupt", "model.control"),
                Wire::new("loop.ask", "cmodel.request"),
                Wire::new("cmodel.result", "loop.model"),
                Wire::new("loop.out", "ui.display"),
            ],
        };
        Kernel::start(
            &assembly,
            &registry,
            &mut factories,
            KernelOptions::default(),
        )
    })
    .unwrap();
    session.send_text("start the held offline call");
    let token = entered.recv_timeout(Duration::from_secs(5)).unwrap();
    session.request_shutdown();
    // Wait on cancellation itself, not a particular user-interrupt wire.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let cancelled = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), token.cancelled())
            .await
            .is_ok()
    });
    // Always release and join before asserting, including on the old broken path.
    release_tx.send(()).unwrap();
    let closed = session.finish_shutdown().unwrap();
    assert!(cancelled, "Stream shutdown left the unwired model running");
    assert!(
        witnessed.try_recv().is_err(),
        "Exit must not impersonate a user interrupt"
    );
    let mut stops = 0;
    let reader = closed.log.reader();
    reader
        .visit_prefix(reader.snapshot_end(), |events| {
            stops += events
                .iter()
                .filter(|e| e.event_type == ce::INTERRUPTED && e.payload["scope"] == "stream")
                .count();
            Ok(())
        })
        .unwrap();
    assert_eq!(stops, 1);
}
