//! A real HTTP peer sends half a data line, then keeps its socket open.
//! Exercise the existing adapter cancellation and kernel deadline, not a new
//! read timeout. No sleeps: cancellation follows the flushed partial response.
use super::*;

#[derive(Clone, Copy)]
enum Ending {
    Deadline,
    UserCancel,
}

struct HeldResponse {
    url: String,
    written: mpsc::Receiver<()>,
    inspect_retries: mpsc::Sender<()>,
    server: std::thread::JoinHandle<bool>,
}

fn half_line_server() -> HeldResponse {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (written_tx, written) = mpsc::channel();
    let (inspect_retries, inspect_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, request) = read_http_request(&mut stream);
        assert!(
            !request.is_empty(),
            "the real adapter must send its request"
        );
        // There is neither a newline nor enough bytes to satisfy Content-Length.
        // Do not close: only a client cancellation can end this pending body.
        stream
            .write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n",
                    "Content-Length: 100000\r\n\r\ndata: {\"unfinished\":"
                )
                .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();
        written_tx.send(()).unwrap();
        // Keep the peer and listener alive until the call finishes. A server
        // disconnect must not be what releases the blocked adapter. Socket
        // teardown timing is separate from whether the model call has ended.
        inspect_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let retried = match listener.accept() {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("cannot inspect retry queue: {error}"),
        };
        drop(stream);
        retried
    });
    HeldResponse {
        url,
        written,
        inspect_retries,
        server,
    }
}

fn exercise(adapter: &str, ending: Ending) {
    ensure_key();
    let mut candidate = transport_candidates()
        .into_iter()
        .find(|candidate| candidate.manifest.name == adapter)
        .unwrap();
    // Pin the product default separately; this test accelerates that deadline,
    // not the HTTP connect timeout or an invented idle-read timeout.
    assert_eq!(candidate.manifest.handle_timeout_ms, Some(600_000));
    if matches!(ending, Ending::Deadline) {
        candidate.manifest.handle_timeout_ms = Some(2_000);
    }
    let HeldResponse {
        url,
        written,
        inspect_retries,
        server,
    } = half_line_server();
    use lattice::components::minimal_loop;
    let registry = [
        ("shell-driver".to_string(), driver_manifest()),
        (adapter.to_string(), candidate.manifest),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("shell-driver".to_string(), Box::new(|_| Box::new(Noop)));
    factories.insert(adapter.to_string(), candidate.factory);
    factories.insert(
        minimal_loop::NAME.into(),
        Box::new(|config| Box::new(minimal_loop::MinimalLoop::from_config(config))),
    );
    let assembly = AssemblyManifest {
        instances: [
            ("driver".into(), ComponentInstance {
                component: "shell-driver".into(), requires: vec![], config: None,
            }),
            ("model".into(), ComponentInstance {
                component: adapter.into(), requires: vec![],
                config: Some(json!({"baseUrl":url,"apiKeyEnv":"LATTICE_SHELL_TEST_KEY_UNSET","model":"exam"})),
            }),
            ("loop".into(), ComponentInstance {
                component: minimal_loop::NAME.into(), requires: vec![],
                config: Some(json!({"model":"exam"})),
            }),
        ].into(),
        wires: vec![
            Wire::new("driver.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("driver.irq", "model.control"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            grace_period: Duration::from_secs(1),
            shutdown_timeout: Duration::from_secs(1),
            ..KernelOptions::default()
        },
    )
    .unwrap();
    let injector = kernel.injector("driver");
    let irq = kernel.injector("driver");
    let observer = std::thread::spawn(move || {
        // The server must actually write the half-line before we claim to have
        // tested a stalled body. No guessed delay stands in for this signal.
        written.recv_timeout(Duration::from_secs(5)).unwrap();
        if matches!(ending, Ending::UserCancel) {
            irq.emit(
                "irq",
                EventDraft::new(ce::INTERRUPTED, &[], json!({"by":"partial-line-test"})),
            );
        }
    });
    let stop = kernel.stop_handle();
    let (finished_tx, finished_rx) = mpsc::channel();
    let guard = std::thread::spawn(move || {
        if finished_rx.recv_timeout(Duration::from_secs(8)).is_err() {
            stop.request("partial-line test guard expired".into(), None);
            true
        } else {
            false
        }
    });
    injector.emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"hi"})),
    );
    kernel.run_until_quiescent().unwrap();
    let _ = finished_tx.send(());
    let guard_fired = guard.join().unwrap();
    observer.join().unwrap();
    let events = kernel.log().replay(1).unwrap();
    // The peer is released only after the kernel call has ended, and before
    // teardown. This tests call cancellation, not immediate TCP socket closure.
    inspect_retries.send(()).unwrap();
    let retried = server.join().unwrap();
    kernel.shutdown();

    assert!(
        !guard_fired,
        "{adapter}: only the test guard ended the call"
    );
    assert!(
        !retried,
        "{adapter}: a received response must never be retried"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == ce::COMPONENT_CRASHED),
        "{adapter}: cancelling a pending body must not retire the adapter: {events:?}"
    );
    let request = events
        .iter()
        .find(|event| event.event_type == ce::MODEL_CALL_STARTED)
        .unwrap();
    let cancellations: Vec<_> = events
        .iter()
        .filter(|event| {
            event.causes.contains(&request.id) && event.event_type == ce::MODEL_CALL_COMPLETED
        })
        .collect();
    assert_eq!(cancellations.len(), 1, "{adapter}: one adapter completion");
    assert_eq!(cancellations[0].payload["status"], "cancelled");
    let deadlines = events
        .iter()
        .filter(|event| {
            event.causes.contains(&request.id)
                && event.event_type == ce::INTERRUPTED
                && event.payload["by"] == "deadline"
        })
        .count();
    assert_eq!(deadlines, usize::from(matches!(ending, Ending::Deadline)));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == ce::MODEL_CALL_STARTED)
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|event| event.event_type == ce::TOOL_EXEC_STARTED));
    assert!(
        cancellations
            .iter()
            .all(|event| event.payload.get("toolCalls").is_none()),
        "{adapter}: no incomplete tool calls"
    );
    // A retained late completion is not by itself a duplicate user reply.
    // Observe the real loop rather than equating two audit records with two
    // externally visible endings.
    let replies: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == ce::OUTPUT_REPLY)
        .collect();
    let turns = events
        .iter()
        .filter(|event| event.event_type == ce::TURN_COMPLETED)
        .count();
    assert_eq!(
        (replies.len(), turns),
        (1, 1),
        "{adapter}: a stalled call must emit one reply and one turn boundary: {replies:?}"
    );
}

#[test]
fn messages_half_line_is_ended_by_the_existing_deadline() {
    exercise(anthropic_model::NAME, Ending::Deadline);
}

#[test]
fn chat_half_line_is_ended_by_the_existing_deadline() {
    exercise(openai_model::NAME, Ending::Deadline);
}

#[test]
fn responses_half_line_is_ended_by_the_existing_deadline() {
    exercise(lattice::components::responses_model::NAME, Ending::Deadline);
}

#[test]
fn messages_half_line_can_be_cancelled_by_the_user() {
    exercise(anthropic_model::NAME, Ending::UserCancel);
}

#[test]
fn chat_half_line_can_be_cancelled_by_the_user() {
    exercise(openai_model::NAME, Ending::UserCancel);
}

#[test]
fn responses_half_line_can_be_cancelled_by_the_user() {
    exercise(
        lattice::components::responses_model::NAME,
        Ending::UserCancel,
    );
}
