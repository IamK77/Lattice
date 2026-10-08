//! The network shell of the real model adapters, against a local fake API.
//!
//! The adapters' pure parts (materialization, SSE folding) have unit tests;
//! what was structurally untested is the SHELL — HTTP status → error
//! judgment fields, the retry discipline, and cancellation mid-request.
//! A tiny TCP server plays the provider, scripted per case and counting
//! requests: sending failures retry, but a successful HTTP response is never
//! replayed. Exact exhaustion and the full backoff schedule are tested with
//! a virtual clock in model_http, rather than waiting minutes on real sockets.
#![cfg(unix)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use lattice::components::{anthropic_model, openai_model};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

#[path = "adapter_shell/compaction_history.rs"]
mod compaction_history;

/// One canned exchange the fake provider performs per incoming request.
enum Canned {
    /// Respond with this HTTP status and a short body
    Status(u16),
    /// Respond 200 with a complete SSE body
    Sse(&'static str),
    /// Native trigger must stay on the streaming Responses route.
    TriggerSse(&'static str),
    /// JSON endpoint with an exact request route and authentication contract.
    JsonAt(&'static str, &'static str),
    /// Read the request, then close before sending response headers.
    DisconnectBeforeResponse,
    /// Respond 200 claiming a long body, send a fragment, slam the door
    CutMidStream(&'static str),
    /// Never respond; announce the request's arrival on the channel
    Hang(mpsc::Sender<()>),
}

/// Serve `script` on an ephemeral local port; requests beyond the script
/// replay its last entry. Returns (base_url, request_counter, request_bodies).
fn fake_api(script: Vec<Canned>) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies: Arc<Mutex<Vec<String>>> = Arc::default();
    let counter = Arc::clone(&hits);
    let recorded = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for (i, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { break };
            let (headers, body) = read_http_request(&mut stream);
            recorded.lock().unwrap().push(body);
            counter.fetch_add(1, Ordering::SeqCst);
            match script.get(i).or(script.last()) {
                Some(Canned::Status(code)) => {
                    let body = "the provider says no";
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
                Some(Canned::Sse(body) | Canned::TriggerSse(body)) => {
                    if matches!(script.get(i).or(script.last()), Some(Canned::TriggerSse(_))) {
                        assert!(headers.starts_with("post /responses http/1.1\r\n"));
                        assert!(headers.contains("x-codex-beta-features: remote_compaction_v2\r\n"));
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
                Some(Canned::JsonAt(path, body)) => {
                    assert!(
                        headers.starts_with(&format!("post {path} http/1.1\r\n")),
                        "{headers}"
                    );
                    assert!(headers.contains("authorization: bearer test-key-not-a-secret\r\n"));
                    let _ = write!(stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
                Some(Canned::DisconnectBeforeResponse) => drop(stream),
                Some(Canned::CutMidStream(fragment)) => {
                    // Declared length far beyond what is sent: the client
                    // sees bytes flow, then the connection dies
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\n\r\n{fragment}"
                    );
                    let _ = stream.flush();
                    drop(stream);
                }
                Some(Canned::Hang(arrived)) => {
                    let _ = arrived.send(());
                    // Hold the connection open, never answer; the client
                    // cancelling closes it from the other side
                    let mut sink = [0u8; 64];
                    while let Ok(n) = stream.read(&mut sink) {
                        if n == 0 {
                            break;
                        }
                    }
                }
                None => {}
            }
        }
    });
    (base_url, hits, bodies)
}

/// Read one HTTP request: headers, then Content-Length bytes of body.
/// Returns headers and body (empty on a short header read).
fn read_http_request(stream: &mut TcpStream) -> (String, String) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return (String::new(), String::new()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let content_length: usize = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    (
        headers,
        String::from_utf8_lossy(&buf[header_end..]).to_string(),
    )
}

/// A driver that injects; it never handles anything itself.
struct Noop;
impl Component for Noop {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

fn driver_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "shell-driver".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:shell-driver".to_string(),
        inputs: vec![],
        outputs: vec![
            PortDecl::new("user", &[ce::USER_MESSAGE]), // unwired; feeds the ledger
            PortDecl::new("req", &[ce::MODEL_CALL_STARTED]),
            PortDecl::new("irq", &[ce::INTERRUPTED]),
        ],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

fn fingerprint(parts: &[Value]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        if let Some(event) = part["event"].as_str() {
            hasher.update(event.as_bytes());
        } else {
            hasher.update(part["inline"].to_string().as_bytes());
        }
        hasher.update(b"\n");
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Which adapter sits the exam. Both real brains take every case.
struct Candidate {
    manifest: ComponentManifest,
    factory: Factory,
}

fn candidates() -> Vec<Candidate> {
    vec![
        Candidate {
            manifest: openai_model::manifest(),
            factory: Box::new(|c| Box::new(openai_model::OpenAiModel::from_config(c))),
        },
        Candidate {
            manifest: anthropic_model::manifest(),
            factory: Box::new(|c| Box::new(anthropic_model::AnthropicModel::from_config(c))),
        },
    ]
}

fn transport_candidates() -> Vec<Candidate> {
    use lattice::components::responses_model;
    let mut candidates = candidates();
    candidates.push(Candidate {
        manifest: responses_model::manifest(),
        factory: Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    });
    candidates
}

/// A happy SSE body per wire format, folding to text "ok".
fn happy_sse(adapter: &str) -> &'static str {
    if adapter == lattice::components::responses_model::NAME {
        concat!(
            "data: ",
            r#"{"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]}}"#,
            "\n\n"
        )
    } else if adapter == openai_model::NAME {
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n"
    } else {
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\ndata: {\"type\":\"message_stop\"}\n\n"
    }
}

/// Drive one model call through a real kernel against the fake API.
/// Returns the completion payload. `interrupt_when` (from a Hang case)
/// triggers an interrupt injection once the request has arrived.
fn run_call(
    candidate: Candidate,
    base_url: &str,
    interrupt_when: Option<mpsc::Receiver<()>>,
) -> Value {
    run_call_with(candidate, base_url, interrupt_when, None)
}

fn run_call_with(
    candidate: Candidate,
    base_url: &str,
    interrupt_when: Option<mpsc::Receiver<()>>,
    ask_system: Option<&str>,
) -> Value {
    run_call_with_purpose(candidate, base_url, interrupt_when, ask_system, None)
}

fn run_call_with_purpose(
    candidate: Candidate,
    base_url: &str,
    interrupt_when: Option<mpsc::Receiver<()>>,
    ask_system: Option<&str>,
    purpose: Option<&str>,
) -> Value {
    run_call_with_options(
        candidate,
        base_url,
        interrupt_when,
        ask_system,
        purpose,
        KernelOptions::default(),
    )
}

fn run_call_with_options(
    candidate: Candidate,
    base_url: &str,
    interrupt_when: Option<mpsc::Receiver<()>>,
    ask_system: Option<&str>,
    purpose: Option<&str>,
    options: KernelOptions,
) -> Value {
    let name = candidate.manifest.name.clone();
    let registry: HashMap<String, ComponentManifest> = [
        ("shell-driver".to_string(), driver_manifest()),
        (name.clone(), candidate.manifest),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("shell-driver".to_string(), Box::new(|_| Box::new(Noop)));
    factories.insert(name.clone(), candidate.factory);
    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "shell-driver".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: name,
                    requires: Vec::new(),
                    config: Some(json!({
                        "baseUrl": base_url,
                        "apiKeyEnv": "LATTICE_SHELL_TEST_KEY_UNSET",
                        "model": "exam",
                        "system": "FROM-CONFIG",
                        "thinking": "max",
                        "effort": ["low", "medium", "high"],
                    })),
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("driver.req", "model.request"),
            Wire::new("driver.irq", "model.control"),
        ],
    };
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options).unwrap();

    // Material must be real: a recorded user message, pointed at and fingerprinted
    let injector = kernel.injector("driver");
    injector.emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hi"})),
    );
    kernel.run_until_quiescent().unwrap();
    let user_id = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .unwrap()
        .id
        .clone();
    let parts = vec![json!({"event": user_id})];

    if let Some(arrived) = interrupt_when {
        let irq = kernel.injector("driver");
        std::thread::spawn(move || {
            // Causal: only after the fake API has the request in hand
            if arrived.recv_timeout(Duration::from_secs(10)).is_ok() {
                irq.emit(
                    "irq",
                    EventDraft::new(ce::INTERRUPTED, &[], json!({"by": "test"})),
                );
            }
        });
    }

    let mut ask =
        json!({"model": "exam", "input": {"parts": parts, "fingerprint": fingerprint(&parts)}});
    if let Some(system) = ask_system {
        ask["system"] = json!(system);
    }
    if let Some(purpose) = purpose {
        ask["purpose"] = json!(purpose);
    }
    let before_request = kernel.log().reader().snapshot_end();
    injector.emit("req", EventDraft::new(ce::MODEL_CALL_STARTED, &[], ask));
    kernel.run_until_quiescent().unwrap();

    let events = kernel.log().replay(1).unwrap();
    // Blanket security invariant, enforced on every shell case: the API key
    // lives in the environment and the request header — if it ever shows up
    // in ANY recorded event, the audit trail becomes a secret store
    for event in &events {
        assert!(
            !serde_json::to_string(event)
                .unwrap()
                .contains("test-key-not-a-secret"),
            "the API key leaked into the ledger: {event:?}"
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(
                |event| event.seq > before_request && event.event_type == ce::MODEL_CALL_COMPLETED
            )
            .count(),
        1,
        "internal retries must publish only one model completion"
    );
    let payload = events
        .iter()
        .rev()
        .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
        .expect("the adapter must complete the call")
        .payload
        .clone();
    kernel.shutdown();
    payload
}

// The key never leaves the environment; these tests set a dummy so the shell
// actually reaches the network. Env mutation is process-global — set once,
// never unset, same value everywhere, so parallel tests cannot disagree.
fn ensure_key() {
    std::env::set_var("LATTICE_SHELL_TEST_KEY_UNSET", "test-key-not-a-secret");
}

#[test]
fn responses_native_compact_keeps_sealed_replacement() {
    use lattice::components::responses_model;
    ensure_key();
    let candidate = Candidate {
        manifest: responses_model::manifest(),
        factory: Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    };
    let (url, hits, bodies) = fake_api(vec![Canned::Status(503), Canned::JsonAt(
        "/responses/compact",
        "{\"output\":[{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"sealed\"}]}"
    )]);
    let payload = run_call_with_purpose(
        candidate,
        &url,
        None,
        Some("RULES"),
        Some("context.compact.responses"),
    );
    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["nativeCompaction"]["output"][0]["encrypted_content"],
        "sealed"
    );
    assert_eq!(payload["purpose"], "context.compact.responses");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0], bodies[1]);
    let body: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert!(body.get("stream").is_none());
    assert_eq!(body["instructions"], "RULES");
}

#[test]
fn responses_profile_search_opt_in_does_not_change_json_compaction() {
    use lattice::components::responses_model;
    ensure_key();
    for purpose in [None, Some("context.compact.responses")] {
        let candidate = Candidate {
            manifest: responses_model::manifest(),
            factory: Box::new(|c| {
                let mut config = c.unwrap().clone();
                config["nativeWebSearch"] = json!(true);
                Box::new(responses_model::ResponsesModel::from_config(Some(&config)))
            }),
        };
        let response = if purpose.is_none() {
            Canned::Sse(concat!(
                "data: ",
                r#"{"type":"response.completed","response":{"status":"completed","output":[{"type":"web_search_call","status":"completed","id":"ws_1","action":{"type":"search","query":"weather"}},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Found"}]}]}}"#,
                "\n\n"
            ))
        } else {
            Canned::JsonAt(
                "/responses/compact",
                r#"{"output":[{"type":"compaction","encrypted_content":"sealed"}]}"#,
            )
        };
        let (url, _, bodies) = fake_api(vec![response]);
        let result = run_call_with_purpose(candidate, &url, None, Some("RULES"), purpose);
        assert_eq!(result["status"], "ok", "{result}");
        let body: Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
        let offered = body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|t| t["type"] == "web_search");
        assert_eq!(offered, purpose.is_none());
        assert_eq!(body.get("prompt_cache_key").is_some(), purpose.is_none());
        if purpose.is_none() {
            assert_eq!(result["toolCalls"], json!([]));
            assert_eq!(result["responsesOutput"][0]["type"], "web_search_call");
        }
    }
}

#[test]
#[ignore = "manual live endpoint check; sends a synthetic greeting only"]
fn responses_live_rust_trigger_compaction() {
    use lattice::components::responses_model;
    let catalog: Value = serde_json::from_slice(
        &std::fs::read(
            std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".lattice/models.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let entry = catalog["models"]["gpt-6-astra"].clone();
    let key = entry["apiKey"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| std::env::var(entry["apiKeyEnv"].as_str().unwrap()).unwrap());
    std::env::set_var("LATTICE_LIVE_RESPONSES_PROBE_KEY", key);
    let url = entry["baseUrl"].as_str().unwrap().to_owned();
    let candidate = Candidate {
        manifest: responses_model::manifest(),
        factory: Box::new(move |c| {
            let mut config = c.unwrap().clone();
            config["model"] = entry["model"].clone();
            config["apiKeyEnv"] = json!("LATTICE_LIVE_RESPONSES_PROBE_KEY");
            config["compactionProtocol"] = json!("trigger");
            config["thinking"] = json!(false);
            Box::new(responses_model::ResponsesModel::from_config(Some(&config)))
        }),
    };
    let result = run_call_with_purpose(
        candidate,
        &url,
        None,
        Some("Preserve the greeting."),
        Some("context.compact.responses"),
    );
    assert_eq!(result["status"], "ok", "{result}");
    assert!(result["nativeCompaction"]["output"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["type"] == "compaction"
            && item["encrypted_content"]
                .as_str()
                .is_some_and(|s| !s.is_empty())));
}

#[test]
fn responses_trigger_compact_requires_terminal_sealed_output() {
    use lattice::components::responses_model;
    ensure_key();
    for (sse, expected) in [
        ("data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"compaction\",\"encrypted_content\":\"sealed\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n", "ok"),
        ("data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"compaction\",\"encrypted_content\":\"sealed\"}}\n\n", "error"),
        ("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\"}]}}\n\n", "error"),
    ] {
        let candidate = Candidate {
            manifest: responses_model::manifest(),
            factory: Box::new(|c| {
                let mut config = c.unwrap().clone();
                config["compactionProtocol"] = json!("trigger");
                config["thinking"] = json!(false);
                Box::new(responses_model::ResponsesModel::from_config(Some(&config)))
            }),
        };
        let (url, hits, bodies) = fake_api(vec![Canned::Status(503), Canned::TriggerSse(sse)]);
        let payload = run_call_with_purpose(candidate, &url, None, Some("RULES"), Some("context.compact.responses"));
        assert_eq!(payload["status"], expected, "{payload}");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies[0], bodies[1]);
        if expected == "ok" {
            assert_eq!(payload["nativeCompaction"]["output"][0]["encrypted_content"], "sealed");
        }
        let body: Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(body["stream"], true);
        assert!(body.get("prompt_cache_key").is_none());
        assert_eq!(body["input"].as_array().unwrap().last().unwrap()["type"], "compaction_trigger");
        assert_eq!(body["instructions"], "RULES");
        assert!(body.get("reasoning").is_none(), "native compaction must not inherit the condenser's thinking setting: {body}");
    }
}

#[test]
fn responses_cancel_and_transport_failures_never_execute_tools() {
    use lattice::components::responses_model;
    ensure_key();
    let candidate = || Candidate {
        manifest: responses_model::manifest(),
        factory: Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    };
    for response in [
        Canned::Status(400),
        Canned::CutMidStream("data: {"),
        Canned::Sse("data: [DONE]\n\n"),
        Canned::Sse("data: not-json\n\n"),
    ] {
        let (url, hits, _) = fake_api(vec![response]);
        let payload = run_call(candidate(), &url, None);
        assert_eq!(payload["status"], "error", "{payload}");
        assert!(payload["toolCalls"].as_array().is_none_or(Vec::is_empty));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
    let (tx, rx) = mpsc::channel();
    let (url, _, _) = fake_api(vec![Canned::Hang(tx)]);
    let payload = run_call(candidate(), &url, Some(rx));
    assert_eq!(payload["status"], "cancelled", "{payload}");
}

#[test]
fn responses_uses_native_input_and_terminal_output() {
    use lattice::components::responses_model;
    ensure_key();
    let candidate = Candidate {
        manifest: responses_model::manifest(),
        factory: Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    };
    let sse = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n";
    let (url, _, bodies) = fake_api(vec![Canned::Sse(sse)]);
    let payload = run_call_with(candidate, &url, None, Some("FROM-EVENT"));
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["text"], "ok");
    let body: Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
    assert_eq!(body["instructions"], "FROM-EVENT");
    assert!(body["input"].is_array());
    assert!(body.get("messages").is_none());
    assert_eq!(body["store"], false);
    assert_eq!(body["reasoning"]["effort"], "high");
}

#[test]
fn responses_prompt_cache_key_survives_restart_and_separates_streams() {
    use lattice::components::responses_model;
    ensure_key();
    let dir = tempfile::tempdir().unwrap();
    let sse = "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":200,\"input_tokens_details\":{\"cached_tokens\":128},\"output_tokens\":1}}}\n\n";
    let (url, hits, bodies) = fake_api(vec![Canned::Sse(sse)]);
    for (stream, system) in [
        ("conversation-a-private", "FIRST SYSTEM"),
        ("conversation-a-private", "CHANGED SYSTEM"),
        ("conversation-b-private", "FIRST SYSTEM"),
    ] {
        // Every iteration constructs a new adapter and kernel. The second
        // resumes the first ledger, including its persisted stream identity.
        let path = dir.path().join(format!("{stream}.jsonl"));
        let resumed_stream = lattice::kernel::log::EventLog::stream_of(&path);
        let payload = run_call_with_options(
            Candidate {
                manifest: responses_model::manifest(),
                factory: Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
            },
            &url,
            None,
            Some(system),
            None,
            KernelOptions {
                stream: Some(resumed_stream.unwrap_or_else(|| stream.to_owned())),
                log_file: Some(path),
                ..KernelOptions::default()
            },
        );
        assert_eq!(payload["status"], "ok", "{payload}");
        assert_eq!(payload["usage"]["input_tokens"], 200);
        assert_eq!(
            payload["usage"]["input_tokens_details"]["cached_tokens"],
            128
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    let bodies: Vec<Value> = bodies
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    let keys: Vec<&str> = bodies
        .iter()
        .map(|body| {
            let key = body["prompt_cache_key"]
                .as_str()
                .expect("ordinary Responses calls need a cache key");
            assert_eq!(key.len(), 64);
            assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(!key.contains("conversation"));
            key
        })
        .collect();
    assert_eq!(
        keys[0], keys[1],
        "restart and changed material must not rotate the key"
    );
    assert_ne!(keys[0], keys[2], "independent streams must not share a key");
    assert_ne!(bodies[0]["instructions"], bodies[1]["instructions"]);
}

#[test]
fn other_adapters_do_not_send_prompt_cache_key() {
    ensure_key();
    for candidate in candidates() {
        let sse = happy_sse(&candidate.manifest.name);
        let (url, _, bodies) = fake_api(vec![Canned::Sse(sse)]);
        assert_eq!(run_call(candidate, &url, None)["status"], "ok");
        let body: Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
        assert!(body.get("prompt_cache_key").is_none());
    }
}

#[test]
fn a_4xx_is_the_requests_fault_and_never_retried() {
    ensure_key();
    for candidate in candidates() {
        let name = candidate.manifest.name.clone();
        let (url, hits, _) = fake_api(vec![Canned::Status(400)]);
        let payload = run_call(candidate, &url, None);
        assert_eq!(payload["status"], "error", "{name}");
        assert_eq!(payload["error"]["code"], "provider.bad_request", "{name}");
        assert_eq!(payload["error"]["blame"], "request", "{name}");
        assert_eq!(payload["error"]["retryable"], false, "{name}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "{name}: a 4xx must not be retried"
        );
    }
}

#[test]
fn model_invalid_requests_are_not_marked_retryable() {
    ensure_key();
    for candidate in transport_candidates() {
        let name = candidate.manifest.name.clone();
        let payload = run_call(candidate, "http://[invalid", None);
        assert_eq!(payload["status"], "error", "{name}: {payload}");
        assert_eq!(
            payload["error"]["retryable"], false,
            "{name}: malformed requests cannot recover by waiting"
        );
    }
}

#[test]
fn model_retries_sending_failures_rate_limits_and_server_errors_without_changing_the_request() {
    ensure_key();
    for status in [None, Some(429), Some(500), Some(503), Some(529)] {
        for candidate in transport_candidates() {
            let name = candidate.manifest.name.clone();
            let failure = status
                .map(Canned::Status)
                .unwrap_or(Canned::DisconnectBeforeResponse);
            let (url, hits, bodies) = fake_api(vec![failure, Canned::Sse(happy_sse(&name))]);
            let payload = run_call(candidate, &url, None);
            assert_eq!(payload["status"], "ok", "{name}: {status:?}: {payload}");
            assert_eq!(payload["text"], "ok", "{name}");
            assert_eq!(hits.load(Ordering::SeqCst), 2, "{name}: {status:?}");
            let bodies = bodies.lock().unwrap();
            assert_eq!(bodies.len(), 2);
            assert_eq!(
                bodies[0], bodies[1],
                "{name}: a retry must not rebuild different material"
            );
        }
    }
}

#[test]
fn model_retries_stop_on_a_permanent_status_or_once_response_reading_begins() {
    ensure_key();
    for candidate in transport_candidates() {
        let name = candidate.manifest.name.clone();
        let (url, hits, _) = fake_api(vec![Canned::Status(503), Canned::Status(403)]);
        let payload = run_call(candidate, &url, None);
        assert_eq!(payload["status"], "error", "{name}: {payload}");
        assert_eq!(payload["error"]["retryable"], false, "{name}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "{name}: a permanent error ends the retry budget early"
        );
    }
    for fragment in ["", "data: {\"type\":\"keepalive\"}\n\n"] {
        for candidate in transport_candidates() {
            let name = candidate.manifest.name.clone();
            let (url, hits, _) = fake_api(vec![Canned::CutMidStream(fragment)]);
            let payload = run_call(candidate, &url, None);
            assert_eq!(payload["status"], "error", "{name}: {payload}");
            assert_eq!(
                hits.load(Ordering::SeqCst),
                1,
                "{name}: headers commit to this response, even before readable text"
            );
        }
    }
}

#[test]
fn a_flaky_start_recovers_on_retry() {
    ensure_key();
    for candidate in candidates() {
        let name = candidate.manifest.name.clone();
        let sse = happy_sse(&name);
        let (url, hits, _) = fake_api(vec![Canned::Status(500), Canned::Sse(sse)]);
        let payload = run_call(candidate, &url, None);
        assert_eq!(payload["status"], "ok", "{name}: {payload}");
        assert_eq!(payload["text"], "ok", "{name}");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "{name}");
    }
}

#[test]
fn once_bytes_flowed_a_dead_stream_is_an_error_never_a_retry() {
    ensure_key();
    for candidate in candidates() {
        let name = candidate.manifest.name.clone();
        // The fragment carries a real delta so content demonstrably flowed
        let fragment = if name == openai_model::NAME {
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"par\"},\"finish_reason\":null}]}\n\n"
        } else {
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"par\"}}\n\n"
        };
        let (url, hits, _) = fake_api(vec![Canned::CutMidStream(fragment)]);
        let payload = run_call(candidate, &url, None);
        assert_eq!(payload["status"], "error", "{name}: {payload}");
        assert_eq!(
            payload["error"]["code"], "provider.stream_interrupted",
            "{name}"
        );
        assert_eq!(
            payload["text"], "par",
            "{name}: the partial text is preserved"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "{name}: after any byte flowed, retrying could double-bill or duplicate — never"
        );
    }
}

#[test]
fn an_interrupt_mid_request_completes_as_cancelled() {
    ensure_key();
    for candidate in transport_candidates() {
        let name = candidate.manifest.name.clone();
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (url, hits, _) = fake_api(vec![Canned::Hang(arrived_tx)]);
        let payload = run_call(candidate, &url, Some(arrived_rx));
        assert_eq!(payload["status"], "cancelled", "{name}: {payload}");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "{name}");
    }
}

#[test]
fn a_system_carried_by_the_event_overrides_the_config_fallback() {
    ensure_key();
    for candidate in candidates() {
        let name = candidate.manifest.name.clone();
        let sse = happy_sse(&name);
        let (url, _, bodies) = fake_api(vec![Canned::Sse(sse)]);
        let payload = run_call_with(candidate, &url, None, Some("FROM-EVENT"));
        assert_eq!(payload["status"], "ok", "{name}");
        let body = bodies.lock().unwrap().first().cloned().unwrap();
        assert!(
            body.contains("FROM-EVENT"),
            "{name}: the event's assembled system must reach the wire: {body}"
        );
        assert!(
            !body.contains("FROM-CONFIG"),
            "{name}: the config fallback must lose to the event's system"
        );
    }
}

#[test]
fn without_an_event_system_the_config_fallback_applies() {
    ensure_key();
    for candidate in candidates() {
        let name = candidate.manifest.name.clone();
        let sse = happy_sse(&name);
        let (url, _, bodies) = fake_api(vec![Canned::Sse(sse)]);
        let payload = run_call(candidate, &url, None);
        assert_eq!(payload["status"], "ok", "{name}");
        let body = bodies.lock().unwrap().first().cloned().unwrap();
        assert!(
            body.contains("FROM-CONFIG"),
            "{name}: a gateless assembly still gets its config system: {body}"
        );
    }
}
