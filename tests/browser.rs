//! Browser authorization is tested without launching Chromium. The ignored
//! backend smoke test uses only an owned local page, never a user's desktop.
use lattice::components::{browser_driver::Browser, browser_tools};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};
use std::collections::HashMap;
struct Driver;
impl Component for Driver {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}
fn kernel(path: &std::path::Path) -> Kernel {
    kernel_executable(path, "/nonexistent/browser")
}
fn kernel_executable(path: &std::path::Path, executable: &str) -> Kernel {
    let mut driver: ComponentManifest = browser_tools::manifest();
    driver.name = "driver".into();
    driver.entry = "builtin:driver".into();
    driver.inputs = vec![PortDecl::new("questions", &[browser_tools::AUTH_REQUESTED])];
    driver.outputs = vec![
        PortDecl::new("calls", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("answers", &[ce::EXTERNAL_INPUT]),
        PortDecl::new("cancel", &[ce::INTERRUPTED]),
    ];
    driver.tools.clear();
    driver.events.clear();
    driver.implements.clear();
    let registry = [
        ("driver".into(), driver),
        (browser_tools::NAME.into(), browser_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".into(), Box::new(|_| Box::new(Driver)));
    factories.insert(
        browser_tools::NAME.into(),
        Box::new(|c| Box::new(browser_tools::BrowserTools::from_config(c))),
    );
    Kernel::start(
        &AssemblyManifest {
            instances: [
                ("driver".into(), ComponentInstance::new("driver", None)),
                (
                    "browser".into(),
                    ComponentInstance::new(
                        browser_tools::NAME,
                        Some(json!({"executable":executable})),
                    ),
                ),
            ]
            .into(),
            wires: vec![
                Wire::new("driver.calls", "browser.execute"),
                Wire::new("driver.answers", "browser.answer"),
                Wire::new("driver.cancel", "browser.control"),
                Wire::new("browser.request", "driver.questions"),
            ],
        },
        &registry,
        &mut factories,
        KernelOptions {
            log_file: Some(path.into()),
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn browser_batches_wait_for_exact_approval_and_refusal_never_launches() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = kernel(&dir.path().join("ledger.jsonl"));
    kernel.injector("driver").emit("calls",EventDraft::new(ce::TOOL_EXEC_STARTED,&[],json!({"call":"one","tool":"Browser","arguments":{"actions":[{"type":"navigate","url":"https://example.com"}]}})));
    kernel.run_until_quiescent().unwrap();
    let question = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == browser_tools::AUTH_REQUESTED)
        .expect("human approval request");
    assert!(!kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED));
    kernel.injector("driver").emit(
        "answers",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[&question.id],
            json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":question.id,"approve":false}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let outcomes: Vec<Value> = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .map(|e| e.payload.clone())
        .collect();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["error"]["code"], "browser.denied");
    kernel.injector("driver").emit(
        "answers",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[&question.id],
            json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(
        kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
            .count(),
        1,
        "a late approval cannot replay a refused batch"
    );
}

#[test]
fn approval_executes_one_batch_without_authorizing_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = kernel(&dir.path().join("ledger.jsonl"));
    for n in 0..2 {
        kernel.injector("driver").emit("calls",EventDraft::new(ce::TOOL_EXEC_STARTED,&[],json!({"call":format!("close-{n}"),"tool":"Browser","arguments":{"actions":[{"type":"close"}]}})));
        kernel.run_until_quiescent().unwrap();
        let events = kernel.log().replay(1).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
                .count(),
            n
        );
        let question = events
            .iter()
            .rev()
            .find(|e| e.event_type == browser_tools::AUTH_REQUESTED)
            .unwrap();
        kernel.injector("driver").emit("answers",EventDraft::new(ce::EXTERNAL_INPUT,&[&question.id],json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true})));
        kernel.run_until_quiescent().unwrap();
        assert!(kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED
                && e.payload["call"] == format!("close-{n}")
                && e.payload["result"]["closed"] == true));
        let events = kernel.log().replay(1).unwrap();
        let answer = events
            .iter()
            .rev()
            .find(|e| e.event_type == ce::EXTERNAL_INPUT)
            .unwrap();
        let outcome = events
            .iter()
            .rev()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
            .unwrap();
        assert!(
            outcome.causes.contains(&answer.id),
            "the action outcome must trace back to its human approval"
        );
    }
}

#[test]
fn an_interrupted_browser_batch_cannot_be_resurrected_by_approval() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = kernel(&dir.path().join("ledger.jsonl"));
    kernel.injector("driver").emit("calls",EventDraft::new(ce::TOOL_EXEC_STARTED,&[],json!({"call":"cancelled","tool":"Browser","arguments":{"actions":[{"type":"type","text":"must not execute"}]}})));
    kernel.run_until_quiescent().unwrap();
    let question = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == browser_tools::AUTH_REQUESTED)
        .unwrap();
    let held = question.payload["request"].as_str().unwrap();
    kernel.injector("driver").emit(
        "cancel",
        EventDraft::new(ce::INTERRUPTED, &[held], json!({"by":"test"})),
    );
    kernel.run_until_quiescent().unwrap();
    kernel.injector("driver").emit("answers",EventDraft::new(ce::EXTERNAL_INPUT,&[&question.id],json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true})));
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    assert!(!events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED));
    assert!(events
        .iter()
        .any(|e| e.event_type == browser_tools::DECISION && e.payload["verdict"] == "denied"));
}

#[cfg(unix)]
#[test]
fn stopping_startup_or_blocked_cdp_keeps_the_component_alive_and_invalidates_queued_approval() {
    use std::io::Read;
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    for during_startup in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let blocked = dir.path().join("blocked");
        for pipe in [&ready, &blocked] {
            let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let abort_ready = ready.clone();
        let quote =
            |p: &std::path::Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        let script = dir.path().join("fake-browser");
        let announcement = if during_startup {
            format!("printf ready > {}", quote(&ready))
        } else {
            format!(
                "printf 'DevTools listening on ws://{}/devtools/browser/test\\n' >&2",
                listener.local_addr().unwrap()
            )
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n{announcement}\nexec /bin/cat {}\n",
                quote(&blocked)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut kernel =
            kernel_executable(&dir.path().join("ledger.jsonl"), script.to_str().unwrap());
        for (call, actions) in [
            (
                "blocked",
                json!([{"type":"navigate","url":"https://example.invalid"}]),
            ),
            ("queued", json!([{"type":"close"}])),
        ] {
            kernel.injector("driver").emit(
                "calls",
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[],
                    json!({"call":call,"tool":"Browser","arguments":{"actions":actions}}),
                ),
            );
        }
        kernel.run_until_quiescent().unwrap();
        let questions: Vec<_> = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .filter(|e| e.event_type == browser_tools::AUTH_REQUESTED)
            .collect();
        assert_eq!(questions.len(), 2);
        let queued = questions[1].id.clone();
        let injector = kernel.injector("driver");
        let controller = std::thread::spawn(move || {
            let stop = move || {
                injector.emit("answers",EventDraft::new(ce::EXTERNAL_INPUT,&[&queued],json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":queued,"approve":true})));
                injector.emit(
                    "cancel",
                    EventDraft::new(ce::INTERRUPTED, &[], json!({"by":"user"})),
                );
            };
            if during_startup {
                let mut signal = String::new();
                std::fs::File::open(ready)
                    .unwrap()
                    .read_to_string(&mut signal)
                    .unwrap();
                assert_eq!(signal, "ready");
                stop();
            } else {
                let (stream, _) = listener.accept().unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                loop {
                    let message = socket.read().unwrap();
                    let tungstenite::Message::Text(text) = message else {
                        continue;
                    };
                    let command: Value = serde_json::from_str(&text).unwrap();
                    if command["method"] == "Page.navigate" {
                        stop();
                        while socket.read().is_ok() {}
                        break;
                    }
                    let result = match command["method"].as_str().unwrap() {
                        "Target.createTarget" => json!({"targetId":"page"}),
                        "Target.attachToTarget" => json!({"sessionId":"session"}),
                        _ => json!({}),
                    };
                    socket
                        .send(tungstenite::Message::Text(
                            json!({"id":command["id"],"result":result})
                                .to_string()
                                .into(),
                        ))
                        .unwrap();
                }
            }
        });
        kernel.injector("driver").emit("answers",EventDraft::new(ce::EXTERNAL_INPUT,&[&questions[0].id],json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":questions[0].id,"approve":true})));
        kernel.run_until_quiescent().unwrap();
        // Release fixture readers as well when a regression fails before the
        // expected startup handshake, so a failure cannot strand the test.
        if during_startup {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut pipe) = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(abort_ready)
            {
                use std::io::Write;
                let _ = pipe.write_all(b"abort");
            }
        } else if let Ok(stream) =
            std::net::TcpStream::connect_timeout(&endpoint, std::time::Duration::from_millis(100))
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        controller.join().unwrap();
        let events = kernel.log().replay(1).unwrap();
        assert!(
            !events.iter().any(|e| e.event_type == ce::COMPONENT_CRASHED),
            "stopping is not a component crash: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED),
            "neither cancelled batch may complete or execute the queued close"
        );
        for question in &questions {
            assert_eq!(
                events
                    .iter()
                    .filter(|e| ce::ends_call(e, question.payload["request"].as_str().unwrap()))
                    .count(),
                1
            );
        }
        kernel.injector("driver").emit(
            "calls",
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"fresh","tool":"Browser","arguments":{"actions":[{"type":"close"}]}}),
            ),
        );
        kernel.run_until_quiescent().unwrap();
        let question = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .rev()
            .find(|e| e.event_type == browser_tools::AUTH_REQUESTED)
            .unwrap();
        kernel.injector("driver").emit("answers",EventDraft::new(ce::EXTERNAL_INPUT,&[&question.id],json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true})));
        kernel.run_until_quiescent().unwrap();
        assert!(kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED
                && e.payload["call"] == "fresh"
                && e.payload["result"]["closed"] == true));
    }
}

#[test]
#[ignore = "manual isolated Chromium smoke; opens an owned localhost page only"]
fn isolated_browser_navigates_clicks_types_and_returns_real_pixels() {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let served = Arc::new(AtomicBool::new(false));
        let address = listener.local_addr().unwrap();
        let mut handlers = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            if served.load(Ordering::SeqCst) {
                break;
            }
            let served = served.clone();
            handlers.push(std::thread::spawn(move || {
                // A speculative idle connection must not block the real GET.
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 { return; }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" { break; }
                }
                let html = r#"<!doctype html><body style="margin:0;background:rgb(255,0,0)"><button style="position:absolute;left:10px;top:10px;width:180px;height:60px" onclick="document.body.style.background='rgb(0,0,255)'">Change</button><input style="position:absolute;left:10px;top:100px;width:180px;height:60px" oninput="if(this.value==='marker')document.body.style.background='rgb(0,255,0)'">"#;
                if write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",html.len(),html).is_ok() {
                    served.store(true, Ordering::SeqCst);
                    let _ = std::net::TcpStream::connect(address);
                }
            }));
        }
        for handler in handlers {
            handler.join().unwrap();
        }
    });
    let exe = std::env::var("LATTICE_BROWSER")
        .unwrap_or_else(|_| "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
    let mut browser = Browser::launch(&exe).unwrap();
    browser
        .action(&json!({"type":"navigate","url":url}))
        .unwrap();
    assert_eq!(pixel(&browser.screenshot().unwrap()), [255, 0, 0]);
    browser
        .action(&json!({"type":"click","x":80,"y":40}))
        .unwrap();
    assert_eq!(pixel(&browser.screenshot().unwrap()), [0, 0, 255]);
    browser
        .action(&json!({"type":"click","x":80,"y":130}))
        .unwrap();
    browser
        .action(&json!({"type":"type","text":"marker"}))
        .unwrap();
    assert_eq!(pixel(&browser.screenshot().unwrap()), [0, 255, 0]);
    assert!(browser
        .action(&json!({"type":"navigate","url":"file:///etc/passwd"}))
        .is_err());
    assert!(browser
        .action(&json!({"type":"click","x":1280,"y":0}))
        .is_err());
    drop(browser);
    server.join().unwrap();
}
fn pixel(encoded: &str) -> [u8; 3] {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut out = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut out).unwrap();
    assert_eq!((info.width, info.height), (1280, 800));
    let channels = info.color_type.samples();
    let at = (700 * 1280 + 1000) * channels;
    [out[at], out[at + 1], out[at + 2]]
}
