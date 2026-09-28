use super::*;
use crate::PortDecl;

struct Healthy;
impl Component for Healthy {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        ctx.emit(
            "out",
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&event.id],
                json!({"call": event.payload["call"], "status": "ok", "result": "healthy"}),
            ),
        );
    }
}

struct Broken {
    restoring: bool,
    waiting: Vec<String>,
    token: Arc<Mutex<Option<CancellationToken>>>,
}
impl Component for Broken {
    fn restore(&mut self, ctx: &mut Ctx) {
        if self.restoring {
            ctx.emit(
                "out",
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[],
                    json!({"call": "restore", "status": "ok", "result": "must not escape"}),
                ),
            );
            ctx.fail("restore fixture", "unreadable fixture history", &[]);
        }
    }
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if event.payload["tool"] == "hold" {
            self.waiting.push(event.id.clone());
            return;
        }
        *self.token.lock().unwrap() = Some(ctx.cancellation().clone());
        ctx.emit(
            "out",
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&event.id],
                json!({"call": event.payload["call"], "status": "ok", "result": "must not escape"}),
            ),
        );
        ctx.fail("read fixture", "unreadable fixture history", &self.waiting);
    }
}

fn manifest(name: &str) -> ComponentManifest {
    ComponentManifest {
        name: name.into(),
        version: "0.0.0".into(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{name}"),
        inputs: vec![PortDecl::new("in", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("out", &[ce::TOOL_EXEC_COMPLETED])],
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

fn start(
    restoring: bool,
    concurrency: usize,
    token: Arc<Mutex<Option<CancellationToken>>>,
) -> Kernel {
    start_component(
        Box::new(move |_| {
            Box::new(Broken {
                restoring,
                waiting: vec![],
                token: token.clone(),
            })
        }),
        concurrency,
        KernelOptions::default(),
    )
}

fn start_component(factory: Factory, concurrency: usize, options: KernelOptions) -> Kernel {
    let mut driver = manifest("driver");
    driver.inputs.clear();
    driver.outputs = vec![
        PortDecl::new("out", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("probe", &[ce::TOOL_EXEC_STARTED]),
    ];
    let mut broken = manifest("broken");
    broken.concurrency = Some(concurrency);
    let registry = [
        ("driver".into(), driver),
        ("broken".into(), broken),
        ("healthy".into(), manifest("healthy")),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".into(), Box::new(|_| Box::new(Healthy)));
    factories.insert("healthy".into(), Box::new(|_| Box::new(Healthy)));
    factories.insert("broken".into(), factory);
    let assembly = AssemblyManifest {
        instances: ["driver", "broken", "healthy"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    ComponentInstance {
                        component: name.into(),
                        requires: vec![],
                        config: None,
                    },
                )
            })
            .collect(),
        wires: vec![
            Wire::new("driver.out", "broken.in"),
            Wire::new("driver.probe", "healthy.in"),
        ],
    };
    Kernel::start(&assembly, &registry, &mut factories, options).unwrap()
}

fn send(kernel: &Kernel, port: &str, call: &str, tool: &str) {
    kernel.injector("driver").emit(
        port,
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": call, "tool": tool, "arguments": {}}),
        ),
    );
}

fn assert_healthy(kernel: &mut Kernel) {
    send(kernel, "probe", "probe", "probe");
    kernel.run_until_quiescent().unwrap();
    assert!(kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|event| event.event_type == ce::TOOL_EXEC_COMPLETED
            && event.payload["result"] == "healthy"));
}

#[test]
fn reopened_witness_prefix_includes_settlement_but_not_future_or_forged_ids() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("witness.ledger");
    let mut log = EventLog::open(
        vec![crate::EventTypeDecl::new(ce::TOOL_EXEC_STARTED, "fixture")],
        "witness",
        Some(path.clone()),
    )
    .unwrap();
    let old = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call": "old", "tool": "probe", "arguments": {}}),
            ),
            "driver",
        )
        .unwrap();
    drop(log);
    let mut kernel = start_component(
        Box::new(|_| Box::new(Healthy)),
        1,
        KernelOptions {
            stream: Some("witness".into()),
            log_file: Some(path),
            ..KernelOptions::default()
        },
    );
    let recovered = kernel.log().replay(1).unwrap();
    let settled = recovered
        .iter()
        .find(|event| event.event_type == ce::INTERRUPTED && event.causes.contains(&old.id))
        .unwrap();
    assert!(kernel.witnessed_by("healthy", &old.id).unwrap());
    assert!(kernel.witnessed_by("healthy", &settled.id).unwrap());
    assert!(!kernel.witnessed_by("healthy", "ev_1_forged").unwrap());
    let boundary = kernel.reopened_through;
    assert_eq!(boundary, recovered.last().unwrap().seq);
    send(&kernel, "probe", "new", "probe");
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(boundary + 1).unwrap();
    let new = events
        .iter()
        .find(|event| event.event_type == ce::TOOL_EXEC_STARTED && event.payload["call"] == "new")
        .unwrap();
    assert!(kernel.witnessed_by("healthy", &new.id).unwrap());
    assert!(!kernel.witnessed_by("broken", &new.id).unwrap());
    assert_eq!(kernel.reopened_through, boundary);
    kernel.shutdown();
}

struct Delayed {
    release: mpsc::Receiver<()>,
}
impl Component for Delayed {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        self.release.recv().unwrap();
        Healthy.handle(port, event, ctx);
    }
}

#[test]
fn stalled_dispatch_keeps_receiving_until_the_worker_actually_finishes() {
    let (release, receiver) = mpsc::channel();
    let receiver = Arc::new(Mutex::new(Some(receiver)));
    let mut kernel = start_component(
        Box::new(move |_| {
            Box::new(Delayed {
                release: receiver.lock().unwrap().take().unwrap(),
            })
        }),
        1,
        KernelOptions {
            stall_timeout: Duration::from_millis(1),
            ..KernelOptions::default()
        },
    );
    let mut release = Some(release);
    kernel.subscribe_log(move |event| {
        if event.payload["code"] == "core.dispatch_stalled" {
            // Completion is causally AFTER the stalled diagnostic, not after
            // a guessed sleep. No extra external wake is sent to the kernel.
            if let Some(release) = release.take() {
                release.send(()).unwrap();
            }
        }
    });
    send(&kernel, "out", "slow", "slow");
    kernel.run_until_quiescent().unwrap();
    assert_eq!(kernel.in_flight, 0, "a stalled worker is not quiescence");
    assert!(kernel
        .log()
        .reader()
        .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .unwrap());
    kernel.shutdown();
}

#[test]
fn stalled_dispatch_remains_stoppable_without_a_worker_reply() {
    let (release, receiver) = mpsc::channel();
    let receiver = Arc::new(Mutex::new(Some(receiver)));
    let mut kernel = start_component(
        Box::new(move |_| {
            Box::new(Delayed {
                release: receiver.lock().unwrap().take().unwrap(),
            })
        }),
        1,
        KernelOptions {
            stall_timeout: Duration::from_millis(1),
            ..KernelOptions::default()
        },
    );
    let stop = kernel.stop_handle();
    kernel.subscribe_log(move |event| {
        if event.payload["code"] == "core.dispatch_stalled" {
            assert!(stop.request("stop stalled fixture".into(), None));
        }
    });
    send(&kernel, "out", "slow", "slow");
    kernel.run_until_quiescent().unwrap();
    assert!(
        kernel.is_stopping(),
        "the host stop must be processed, not stranded"
    );
    assert_eq!(kernel.in_flight, 0);
    let events = kernel.log().replay(1).unwrap();
    assert!(events.iter().any(|e| e.event_type == ce::INTERRUPTED));
    // Let the retired thread unwind after its unknown outcome was recorded.
    release.send(()).unwrap();
    kernel.shutdown();
}

#[test]
fn explicit_failure_discards_buffered_results_and_settles_waiting_responsibility() {
    let token = Arc::new(Mutex::new(None));
    let mut kernel = start(false, 1, token.clone());
    send(&kernel, "out", "waiting", "hold");
    kernel.run_until_quiescent().unwrap();
    // The first delivery ended, but this call is still owned by the gate.
    assert_eq!(kernel.in_flight, 0);
    send(&kernel, "out", "failing", "fail");
    kernel.run_until_quiescent().unwrap();
    assert!(token.lock().unwrap().as_ref().unwrap().is_cancelled());
    let events = kernel.log().replay(1).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.event_type == ce::TOOL_EXEC_COMPLETED));
    assert!(!events
        .iter()
        .any(|event| event.event_type == ce::COMPONENT_CRASHED));
    let failure = events
        .iter()
        .find(|event| event.payload["code"] == "core.component_failed")
        .unwrap();
    assert_eq!(failure.source, KERNEL_SOURCE);
    assert_eq!(failure.payload["detail"]["operation"], "read fixture");
    assert_eq!(failure.payload["detail"]["phase"], "handle");
    let starts: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == ce::TOOL_EXEC_STARTED)
        .collect();
    assert_eq!(starts.len(), 2);
    for started in starts {
        assert_eq!(
            events
                .iter()
                .filter(|event| ce::ends_call(event, &started.id))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .any(|event| event.event_type == ce::INTERRUPTED
                    && event.causes.contains(&started.id))
        );
    }
    assert_healthy(&mut kernel);
    kernel.shutdown();
}

#[test]
fn failed_restore_acknowledges_startup_and_releases_the_whole_worker_crew() {
    // A deadline bounds the assertion; synchronization is the returned result,
    // not a sleep that guesses when another thread has made progress.
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut kernel = start(true, 3, Arc::new(Mutex::new(None)));
        kernel.run_until_quiescent().unwrap();
        let events = kernel.log().replay(1).unwrap();
        let failure = events
            .iter()
            .find(|event| event.payload["code"] == "core.component_failed")
            .unwrap();
        assert_eq!(failure.payload["detail"]["phase"], "restore");
        assert!(!events
            .iter()
            .any(|event| event.event_type == ce::TOOL_EXEC_COMPLETED));
        assert_healthy(&mut kernel);
        kernel.shutdown();
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("failed restore must not strand startup or worker shutdown");
    thread.join().unwrap();
}
