//! Synthetic two-turn HTTP exam for endpoint-specific adapter behavior.
use crate::contracts::core_events as ce;
use crate::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::mpsc,
    time::Duration,
};

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}

pub(crate) fn round_trip(
    manifest: ComponentManifest,
    build: impl Fn(SocketAddr) -> Box<dyn Component> + Send + Sync + 'static,
    first_reply: String,
    second_reply: String,
) -> Vec<(String, Value)> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in [first_reply, second_reply] {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 65536);
            }
            let headers = String::from_utf8(bytes).unwrap().to_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            requests.push((headers, serde_json::from_slice(&body).unwrap()));
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
        }
        tx.send(requests).unwrap();
    });
    let driver = ComponentManifest {
        name: "compat-driver".into(),
        version: "0".into(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:compat-driver".into(),
        inputs: vec![PortDecl::new("result", &[ce::MODEL_CALL_COMPLETED])],
        outputs: vec![
            PortDecl::new("user", &[ce::USER_MESSAGE]),
            PortDecl::new("req", &[ce::MODEL_CALL_STARTED]),
            PortDecl::new("tool", &[ce::TOOL_EXEC_COMPLETED]),
        ],
        events: vec![],
        default_wiring: vec![],
        capabilities: None,
        implements: vec![],
        tools: vec![],
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let name = manifest.name.clone();
    let registry = HashMap::from([(driver.name.clone(), driver), (name.clone(), manifest)]);
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("compat-driver".into(), Box::new(|_| Box::new(Driver)));
    factories.insert(name.clone(), Box::new(move |_| build(address)));
    let assembly = AssemblyManifest {
        instances: std::collections::BTreeMap::from([
            (
                "driver".into(),
                ComponentInstance {
                    component: "compat-driver".into(),
                    requires: vec![],
                    config: None,
                },
            ),
            (
                "model".into(),
                ComponentInstance {
                    component: name,
                    requires: vec![],
                    config: None,
                },
            ),
        ]),
        wires: vec![
            Wire::new("driver.req", "model.request"),
            Wire::new("model.result", "driver.result"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    let injector = kernel.injector("driver");
    injector.emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"inspect it"})),
    );
    kernel.run_until_quiescent().unwrap();
    let user = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .unwrap();
    let mut parts = vec![json!({"event":user.id})];
    for turn in 0..2 {
        let fingerprint = crate::components::model_common::material_fingerprint(&parts);
        injector.emit("req", EventDraft::new(ce::MODEL_CALL_STARTED, &[], json!({
            "model":"synthetic", "input":{"parts":parts,"fingerprint":fingerprint},
            "system":"Synthetic test only.", "tools":[{"name":"Inspect", "description":"synthetic", "parameters":{"type":"object","properties":{"value":{"type":"integer"}}}}]
        })));
        kernel.run_until_quiescent().unwrap();
        let result = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .rev()
            .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
            .unwrap();
        assert_eq!(result.payload["status"], "ok", "{}", result.payload);
        if turn == 0 {
            assert_eq!(result.payload["reasoning"][0]["text"], "original thought");
            assert_eq!(result.payload["toolCalls"][0]["id"], "call_synthetic");
            assert_eq!(
                result.payload["toolCalls"][0]["arguments"],
                json!({"value":1})
            );
            parts.push(json!({"event":result.id}));
            injector.emit(
                "tool",
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&result.id],
                    json!({"call":"call_synthetic","status":"ok","result":"synthetic result"}),
                ),
            );
            kernel.run_until_quiescent().unwrap();
            let answer = kernel
                .log()
                .replay(1)
                .unwrap()
                .into_iter()
                .rev()
                .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
                .unwrap();
            parts.push(json!({"event":answer.id}));
        } else {
            assert_eq!(result.payload["text"], "ok");
        }
    }
    for event in kernel.log().replay(1).unwrap() {
        assert!(!serde_json::to_string(&event)
            .unwrap()
            .contains("SYNTHETIC_KEY"));
    }
    rx.recv_timeout(Duration::from_secs(10)).unwrap()
}

pub(crate) fn client(host: &str, address: SocketAddr) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .resolve(host, address)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
