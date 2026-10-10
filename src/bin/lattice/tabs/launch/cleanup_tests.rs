use super::*;
use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::{AssemblyManifest, ComponentInstance, Factory, Kernel, KernelOptions, Wire};
use std::cell::RefCell;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[path = "cleanup_delivery.rs"]
mod delivery;

type Collection = Result<Vec<String>, String>;

#[derive(Default)]
struct Hooks {
    collected: Option<mpsc::Sender<Collection>>,
    writer: Option<Box<dyn std::io::Write>>,
    rescue: Option<mpsc::Sender<Session>>,
}
thread_local! { static HOOKS: RefCell<Hooks> = RefCell::new(Hooks::default()); }
struct HookGuard;
impl Drop for HookGuard {
    fn drop(&mut self) {
        HOOKS.with(|hooks| *hooks.borrow_mut() = Hooks::default());
    }
}

pub(super) fn observe_collection(result: &Result<lattice::shutdown::SessionShutdown, String>) {
    HOOKS.with(|hooks| {
        if let Some(sender) = &hooks.borrow().collected {
            let result = result
                .as_ref()
                .map(|closed| closed.kernel.lingering.clone())
                .map_err(Clone::clone);
            let _ = sender.send(result);
        }
    });
}

pub(super) fn report_if_configured(message: &str) -> bool {
    HOOKS.with(|hooks| match &mut hooks.borrow_mut().writer {
        Some(writer) => {
            report_to(writer.as_mut(), message);
            true
        }
        None => false,
    })
}

fn observe() -> (HookGuard, mpsc::Receiver<Collection>) {
    let (sender, receiver) = mpsc::channel();
    HOOKS.with(|hooks| hooks.borrow_mut().collected = Some(sender));
    (HookGuard, receiver)
}

// A mutation can transfer the live handle here instead of collecting it. The
// fixture always rescues it before asserting the evidence saved at startup return.
// This is not used by production or by the unmodified collection path.
#[allow(dead_code)]
pub(super) fn rescue(session: Session) -> Option<String> {
    let sender = HOOKS
        .with(|hooks| hooks.borrow().rescue.clone())
        .expect("armed rescue owner");
    if let Err(rejected) = sender.send(session) {
        rejected.0.shutdown();
        panic!("rescue owner disappeared");
    }
    None
}
struct Rescue(mpsc::Receiver<Session>);
impl Rescue {
    fn collect(&self) -> Vec<Collection> {
        self.0
            .try_iter()
            .map(|session| {
                session.request_shutdown();
                session
                    .finish_shutdown()
                    .map(|closed| closed.kernel.lingering)
            })
            .collect()
    }
}
impl Drop for Rescue {
    fn drop(&mut self) {
        let _ = self.collect();
    }
}

struct TrackedUi {
    inner: silent_ui::SilentUi,
    dropped: mpsc::Sender<()>,
}
impl lattice::Component for TrackedUi {
    fn handle(&mut self, entry: &str, event: &lattice::EventEnvelope, cx: &mut lattice::Ctx) {
        self.inner.handle(entry, event, cx);
    }
}
impl Drop for TrackedUi {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

fn fixture() -> (Session, mpsc::Receiver<()>) {
    let (dropped, observed) = mpsc::channel();
    let session = Session::spawn("ui", move |render| {
        let registry = [
            (silent_ui::NAME.into(), silent_ui::manifest()),
            (minimal_loop::NAME.into(), minimal_loop::manifest()),
            (scripted_model::NAME.into(), scripted_model::manifest()),
        ]
        .into();
        let mut factories: std::collections::HashMap<String, Factory> = Default::default();
        factories.insert(
            silent_ui::NAME.into(),
            Box::new(move |_| {
                Box::new(TrackedUi {
                    inner: silent_ui::SilentUi::new(Arc::new(Mutex::new(vec![]))),
                    dropped: dropped.clone(),
                })
            }),
        );
        factories.insert(
            minimal_loop::NAME.into(),
            Box::new(|cfg| Box::new(minimal_loop::MinimalLoop::from_config(cfg))),
        );
        factories.insert(
            scripted_model::NAME.into(),
            Box::new(|cfg| Box::new(scripted_model::ScriptedModel::from_config(cfg))),
        );
        let assembly = AssemblyManifest {
            instances: [
                ("ui".into(), ComponentInstance::new(silent_ui::NAME, None)),
                (
                    "loop".into(),
                    ComponentInstance::new(minimal_loop::NAME, None),
                ),
                (
                    "model".into(),
                    ComponentInstance {
                        component: scripted_model::NAME.into(),
                        requires: vec![],
                        config: Some(json!({"script":[{"status":"ok","text":"fixture answer"}]})),
                    },
                ),
            ]
            .into(),
            wires: vec![
                Wire::new("ui.user", "loop.input"),
                Wire::new("loop.ask", "model.request"),
                Wire::new("model.result", "loop.model"),
                Wire::new("loop.out", "ui.display"),
            ],
        };
        let mut kernel = Kernel::start(
            &assembly,
            &registry,
            &mut factories,
            KernelOptions::default(),
        )?;
        kernel.subscribe_log(move |event| {
            let _ = render.send(lattice::RenderEvent::Appended(Box::new(event.clone())));
        });
        Ok(kernel)
    })
    .unwrap();
    (session, observed)
}

fn broken_shutdown() -> Session {
    // No component threads or subprocesses: only this test-owned Session worker
    // panics on its host-stop observation, producing a genuine join error.
    Session::spawn("unused", |_| {
        let mut kernel = Kernel::start(
            &AssemblyManifest {
                instances: Default::default(),
                wires: vec![],
            },
            &Default::default(),
            &mut Default::default(),
            KernelOptions::default(),
        )?;
        kernel.subscribe_log(|event| {
            assert!(
                event.event_type != core_events::INTERRUPTED,
                "fixture stop observer panic"
            );
        });
        Ok(kernel)
    })
    .unwrap()
}

fn blank(session: Session) -> Ready {
    Ready::initialize(session, Ui::replayed(&[]), 0, |_, _| Ok(()))
        .unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn real_replay_failure_collects_before_return_without_admitting_input() {
    let (_hooks, collected) = observe();
    let (rescue_tx, rescue_rx) = mpsc::channel();
    HOOKS.with(|hooks| hooks.borrow_mut().rescue = Some(rescue_tx));
    let rescue = Rescue(rescue_rx);
    let (session, dropped) = fixture();
    let reader = session.log_reader();
    let mut primary = None;
    let result = Ready::initialize(session, Ui::replayed(&[]), 0, |session, ui| {
        let log = session.log_reader();
        let error = ui
            .replay_prefix(&log, log.snapshot_end() + 1)
            .unwrap_err()
            .to_string();
        primary = Some(error.clone());
        Err(error)
    });
    let error = match result {
        Err(error) => Some(error),
        Ok(ready) => {
            drop(ready);
            None
        }
    };
    // Collection is synchronous and startup has returned. These are observations
    // at that causal boundary, not a race against a still-running cleanup thread.
    let before_rescue = collected.try_iter().collect::<Vec<_>>();
    let destroyed_before_rescue = dropped.try_recv().is_ok();
    let rescued = rescue.collect();
    if !destroyed_before_rescue && !rescued.is_empty() {
        dropped.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert!(
        rescued
            .iter()
            .all(|result| matches!(result, Ok(lingering) if lingering.is_empty())),
        "the negative control must finish its rescue before failing"
    );
    assert_eq!(error, primary);
    assert_eq!(
        before_rescue,
        [Ok(vec![])],
        "startup returned without real finish_shutdown evidence"
    );
    assert!(
        destroyed_before_rescue,
        "component was not destroyed before startup returned"
    );
    assert!(!reader.replay(1).unwrap().iter().any(|event| matches!(
        event.event_type.as_str(),
        core_events::USER_MESSAGE | core_events::MODEL_CALL_STARTED
    )));
}

#[test]
fn successful_handoff_answers_and_has_only_one_session_owner() {
    let (_hooks, collected) = observe();
    let (session, dropped) = fixture();
    let ready = blank(session);
    let mut seat = ready.seat();
    seat.session.get().send_text("fixture question");
    let mut answer = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    for _ in 0..100 {
        let Ok(event) = seat
            .session
            .get()
            .next_render_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        else {
            break;
        };
        if let lattice::RenderEvent::Appended(event) = event {
            if event.event_type == core_events::OUTPUT_REPLY {
                answer = Some(event.payload["text"].clone());
                break;
            }
        }
    }
    let owned = match &mut seat.session {
        SeatSession::Owned(session) => session.take().unwrap(),
        SeatSession::Main(_) => unreachable!(),
    };
    owned.request_shutdown();
    let closed = owned.finish_shutdown().unwrap();
    drop(seat);
    assert!(closed.kernel.lingering.is_empty());
    assert_eq!(answer, Some(json!("fixture answer")));
    assert!(
        collected.try_iter().next().is_none(),
        "transferred Ready must not also collect"
    );
    dropped.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn rejected_and_queued_ready_values_each_collect_their_session() {
    for queued in [false, true] {
        let (_hooks, collected) = observe();
        let (session, dropped) = fixture();
        let (tx, rx) = mpsc::channel();
        if queued {
            deliver(tx, Ok(blank(session)));
            drop(rx);
        } else {
            drop(rx);
            deliver(tx, Ok(blank(session)));
        }
        assert_eq!(collected.try_iter().collect::<Vec<_>>(), [Ok(vec![])]);
        dropped.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

#[test]
fn initialization_unwind_uses_the_same_real_collection_path() {
    let (_hooks, collected) = observe();
    let (session, dropped) = fixture();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = Ready::initialize(session, Ui::replayed(&[]), 0, |_, _| {
            panic!("original initialization panic")
        });
    }))
    .unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"original initialization panic")
    );
    assert_eq!(collected.try_iter().collect::<Vec<_>>(), [Ok(vec![])]);
    dropped.recv_timeout(Duration::from_secs(5)).unwrap();
}

struct FailingWriter(Arc<Mutex<Vec<String>>>);
impl std::io::Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(bytes).into_owned());
        Err(std::io::Error::other("fixture diagnostic failure"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn failing_writer() -> Arc<Mutex<Vec<String>>> {
    let attempts = Arc::new(Mutex::new(vec![]));
    HOOKS.with(|hooks| hooks.borrow_mut().writer = Some(Box::new(FailingWriter(attempts.clone()))));
    attempts
}

#[test]
fn original_error_survives_join_error_and_failed_missing_receiver_diagnostic() {
    let (_hooks, collected) = observe();
    let attempts = failing_writer();
    let result = Ready::initialize(broken_shutdown(), Ui::replayed(&[]), 0, |_, _| {
        Err("original replay error".into())
    });
    let error = match result {
        Err(error) => error,
        Ok(ready) => {
            drop(ready);
            panic!("initialization unexpectedly succeeded")
        }
    };
    let collection = collected.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(collection.is_err());
    assert!(error.starts_with("original replay error\n"));
    assert!(error.contains("could not join"));
    let (tx, rx) = mpsc::channel();
    drop(rx);
    deliver(tx, Err(error.clone()));
    assert!(attempts.lock().unwrap().join("").contains(&error));
}

#[test]
fn failed_unwind_diagnostic_does_not_replace_the_original_panic() {
    let (_hooks, collected) = observe();
    let attempts = failing_writer();
    let session = broken_shutdown();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = Ready::initialize(session, Ui::replayed(&[]), 0, |_, _| {
            panic!("original initialization panic")
        });
    }))
    .unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"original initialization panic")
    );
    assert!(collected
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_err());
    assert!(attempts.lock().unwrap().join("").contains("could not join"));
}

#[test]
fn cleanup_composition_keeps_join_failure_and_lingering_distinct() {
    let primary = "original replay error".to_string();
    assert_eq!(
        with_cleanup(primary.clone(), cleanup_problem(Ok(vec![]))),
        primary
    );
    let failed = with_cleanup(primary.clone(), cleanup_problem(Err("join failure".into())));
    assert!(failed.starts_with(&primary));
    assert!(failed.contains("join failure"));
    assert!(!failed.contains("components are still running"));
    let lingering = with_cleanup(
        primary.clone(),
        cleanup_problem(Ok(vec!["held-component".into()])),
    );
    assert!(lingering.starts_with(&primary));
    assert!(lingering.contains("components are still running: held-component"));
    assert!(!lingering.contains("could not join"));
}
