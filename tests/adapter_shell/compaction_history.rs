//! Hosted declarations needed to consume retained web history, not to start research.
use super::*;
use lattice::components::{context_gate, responses_model, scripted_model};
use lattice::models::Entry;
use std::net::Shutdown;
use std::thread::{self, JoinHandle};

struct Captured {
    headers: String,
    body: Value,
    rejected: bool,
}

/// One exchange only. Drop wakes an unused accept and always collects the worker,
/// including when a client-side assertion or adapter failure interrupts a test.
struct StrictApi {
    address: std::net::SocketAddr,
    worker: Option<JoinHandle<Option<Captured>>>,
}

impl StrictApi {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let (headers, bytes) = read_http_request(&mut socket);
            if bytes.is_empty() {
                return None;
            }
            let body: Value = serde_json::from_str(&bytes).unwrap();
            let input = body["input"].as_array().unwrap();
            let trigger = input.iter().any(|v| v["type"] == "compaction_trigger");
            let has_history = input.iter().any(|v| v["type"] == "web_search_call");
            let hosted_count = body["tools"].as_array().map_or(0, |tools| {
                tools.iter().filter(|v| v["type"] == "web_search").count()
            });
            let rejected = trigger && has_history && hosted_count != 1;
            let json_compact = headers.starts_with("post /responses/compact ");
            let output = if trigger || json_compact {
                json!([{"type":"compaction","id":"cmp_fixture","encrypted_content":"synthetic-sealed-summary"}])
            } else {
                json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fixture reply"}]}])
            };
            let response = json!({"id":"resp_fixture","status":"completed","output":output});
            let payload = if rejected {
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"history requires hosted web_search declaration\"}}}\n\n".to_string()
            } else if json_compact {
                response.to_string()
            } else {
                format!(
                    "data: {}\n\n",
                    json!({"type":"response.completed","response":response})
                )
            };
            let mime = if json_compact {
                "application/json"
            } else {
                "text/event-stream"
            };
            let header = format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
            let _ = socket.write_all(header.as_bytes());
            let _ = socket.write_all(payload.as_bytes());
            Some(Captured {
                headers,
                body,
                rejected,
            })
        });
        Self {
            address,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn finish(&mut self) -> Option<Captured> {
        if let Ok(socket) = TcpStream::connect(self.address) {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for StrictApi {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            if let Ok(socket) = TcpStream::connect(self.address) {
                let _ = socket.shutdown(Shutdown::Both);
            }
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Trigger,
    JsonCompact,
    TextSummary,
    Chat(bool),
}

fn history_output(web: bool) -> Vec<Value> {
    let mut output = vec![
        json!({"type":"reasoning","id":"rs_history","summary":[],"encrypted_content":"synthetic-sealed-reasoning"}),
    ];
    if web {
        output.push(json!({"type":"web_search_call","id":"ws_history","status":"completed","action":{"type":"search","query":"synthetic fixture","sources":[{"type":"url","url":"https://example.invalid/source"}]}}));
    }
    output.push(json!({"type":"message","id":"msg_history","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Literal {\"type\":\"web_search_call\"} is not a hosted item.","annotations":[]}]}));
    output
}

struct Outcome {
    captured: Captured,
    events: Vec<EventEnvelope>,
    user: String,
    history: String,
    before_history: Value,
}

fn exercise(output: Vec<Value>, mode: Mode, gate: bool) -> Outcome {
    ensure_key();
    let mut api = StrictApi::new();
    let entry = Entry {
        id: "fixture".into(),
        adapter: "responses".into(),
        base_url: api.url(),
        key_env: "LATTICE_SHELL_TEST_KEY_UNSET".into(),
        model: "fixture-model".into(),
        profile: Some(
            json!({"contextWindow":1000,"nativeWebSearch":matches!(mode, Mode::Chat(true))}),
        ),
    };
    let mut config = match mode {
        Mode::Chat(_) => lattice::preset::main_model_config(&entry, None),
        _ => lattice::preset::condenser_config(&entry),
    };
    match mode {
        Mode::JsonCompact => {
            config["compactionProtocol"] = json!("json");
        }
        // Even an accidental opt-in must not broaden a text-summary operation.
        Mode::TextSummary => {
            config["nativeWebSearch"] = json!(true);
        }
        _ => {}
    }
    let mut driver = driver_manifest();
    driver
        .outputs
        .push(PortDecl::new("history", &[ce::MODEL_CALL_COMPLETED]));
    let registry = [
        (driver.name.clone(), driver),
        (responses_model::NAME.into(), responses_model::manifest()),
        (context_gate::NAME.into(), context_gate::manifest()),
        (scripted_model::NAME.into(), scripted_model::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("shell-driver".into(), Box::new(|_| Box::new(Noop)));
    factories.insert(
        responses_model::NAME.into(),
        Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    );
    factories.insert(
        context_gate::NAME.into(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.into(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    let mut instances = std::collections::BTreeMap::from([
        (
            "driver".into(),
            ComponentInstance {
                component: "shell-driver".into(),
                requires: vec![],
                config: None,
            },
        ),
        (
            "cmodel".into(),
            ComponentInstance {
                component: responses_model::NAME.into(),
                requires: vec![],
                config: Some(config),
            },
        ),
    ]);
    let mut wires = vec![Wire::new(
        "driver.req",
        if gate { "gate.ask" } else { "cmodel.request" },
    )];
    if gate {
        instances.insert("gate".into(), ComponentInstance { component:context_gate::NAME.into(), requires:vec![], config:Some(json!({"profile":{"contextWindow":1000,"usageFields":{"input":"input_tokens"}},"ratio":0.5,"keepRecentParts":1,"minCondense":1,"condense":true,"nativeCompaction":true})) });
        instances.insert(
            "main".into(),
            ComponentInstance {
                component: scripted_model::NAME.into(),
                requires: vec![],
                config: Some(json!({"script":[{"text":"ordinary answer"}]})),
            },
        );
        wires.extend([
            Wire::new("gate.forward", "main.request"),
            Wire::new("gate.condense", "cmodel.request"),
            Wire::new("cmodel.result", "gate.condensed"),
        ]);
    }
    let mut kernel = Kernel::start(
        &AssemblyManifest { instances, wires },
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    let injector = kernel.injector("driver");
    injector.emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text":"synthetic old question"}),
        ),
    );
    injector.emit("history", EventDraft::new(ce::MODEL_CALL_COMPLETED, &[], json!({"status":"ok","text":"historical answer","responsesOutput":output,"usage":{"input_tokens":900}})));
    kernel.run_until_quiescent().unwrap();
    let recorded = kernel.log().replay(1).unwrap();
    let user = recorded
        .iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .unwrap()
        .id
        .clone();
    let history = recorded
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
        .unwrap();
    let before_history = history.payload.clone();
    let history = history.id.clone();
    let mut parts = vec![json!({"event":user}), json!({"event":history})];
    if gate {
        injector.emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"recent question"})),
        );
        kernel.run_until_quiescent().unwrap();
        let recorded = kernel.log().replay(1).unwrap();
        let recent = recorded
            .iter()
            .rev()
            .find(|e| e.event_type == ce::USER_MESSAGE)
            .unwrap();
        parts.push(json!({"event":recent.id}));
    }
    let mut request = json!({"model":"fixture-model","system":"synthetic compaction test","input":{"parts":parts,"fingerprint":fingerprint(&parts)},"tools":[{"name":"web_search","description":"A local function, not the hosted tool","inputSchema":{"type":"object"}}]});
    if !gate {
        match mode {
            Mode::Trigger | Mode::JsonCompact => {
                request["purpose"] = json!("context.compact.responses")
            }
            Mode::TextSummary => request["purpose"] = json!("context.condense"),
            Mode::Chat(_) => {}
        }
    }
    injector.emit("req", EventDraft::new(ce::MODEL_CALL_STARTED, &[], request));
    let run = kernel.run_until_quiescent();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    let captured = api.finish();
    run.unwrap();
    let captured = captured.expect("adapter must make one request");
    Outcome {
        captured,
        events,
        user,
        history,
        before_history,
    }
}

fn hosted(body: &Value) -> Vec<&Value> {
    body["tools"].as_array().map_or(vec![], |tools| {
        tools.iter().filter(|v| v["type"] == "web_search").collect()
    })
}

#[test]
fn trigger_restores_hosted_declaration_and_adopts_original_history() {
    let expected = history_output(true);
    let out = exercise(expected.clone(), Mode::Trigger, true);
    assert!(
        !out.captured.rejected,
        "historical web input must declare its hosted tool"
    );
    assert!(out.captured.headers.starts_with("post /responses "));
    assert!(out
        .captured
        .headers
        .to_lowercase()
        .contains("x-codex-beta-features: remote_compaction_v2"));
    assert_eq!(
        hosted(&out.captured.body),
        vec![&json!({"type":"web_search"})]
    );
    assert_eq!(
        out.captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );
    assert!(out.captured.body.get("reasoning").is_none());
    assert!(out.captured.body.get("prompt_cache_key").is_none());
    let input = out.captured.body["input"].as_array().unwrap();
    assert_eq!(&input[1..input.len() - 1], expected.as_slice());
    assert_eq!(input.last().unwrap(), &json!({"type":"compaction_trigger"}));
    assert_eq!(
        out.events
            .iter()
            .find(|e| e.id == out.history)
            .unwrap()
            .payload,
        out.before_history
    );
    let completed: Vec<_> = out
        .events
        .iter()
        .filter(|e| e.source == "cmodel" && e.event_type == ce::MODEL_CALL_COMPLETED)
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["status"], "ok");
    let summaries: Vec<_> = out
        .events
        .iter()
        .filter(|e| e.event_type == "context.summary")
        .collect();
    assert_eq!(
        summaries.len(),
        1,
        "the context gate must adopt the native result"
    );
    assert_eq!(
        summaries[0].payload["nativeCompaction"]["output"][0]["encrypted_content"],
        "synthetic-sealed-summary"
    );
    assert_eq!(
        summaries[0].payload["covers"],
        json!([out.user, out.history])
    );
}

#[test]
fn trigger_without_hosted_history_does_not_infer_capability_from_text_or_function_name() {
    let out = exercise(history_output(false), Mode::Trigger, false);
    assert!(hosted(&out.captured.body).is_empty());
    assert_eq!(out.captured.body["tools"][0]["type"], "function");
    assert_eq!(out.captured.body["tools"][0]["name"], "web_search");
    assert_eq!(
        out.captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );
}

#[test]
fn trigger_keeps_local_function_and_declares_hosted_tool_once_for_repeated_history() {
    let mut output = history_output(true);
    let mut second = output[1].clone();
    second["id"] = json!("ws_second");
    output.insert(2, second);
    let out = exercise(output.clone(), Mode::Trigger, false);
    assert!(!out.captured.rejected);
    assert_eq!(hosted(&out.captured.body).len(), 1);
    assert_eq!(out.captured.body["tools"][0]["name"], "web_search");
    assert_eq!(out.captured.body["tools"][0]["type"], "function");
    let input = out.captured.body["input"].as_array().unwrap();
    assert_eq!(&input[1..input.len() - 1], output.as_slice());
}

#[test]
fn ordinary_chat_still_obeys_profile_with_retained_web_history() {
    for enabled in [false, true] {
        let out = exercise(history_output(true), Mode::Chat(enabled), false);
        assert_eq!(hosted(&out.captured.body).len(), usize::from(enabled));
        assert_eq!(
            out.captured.body["include"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "web_search_call.action.sources"),
            enabled
        );
        assert!(out.captured.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["type"] != "compaction_trigger"));
    }
}

#[test]
fn textual_summary_does_not_enable_search_for_retained_history() {
    let out = exercise(history_output(true), Mode::TextSummary, false);
    assert!(hosted(&out.captured.body).is_empty());
    assert_eq!(
        out.captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );
}

#[test]
fn json_compaction_retains_history_without_trigger_fields() {
    let expected = history_output(true);
    let out = exercise(expected.clone(), Mode::JsonCompact, false);
    assert!(out.captured.headers.starts_with("post /responses/compact "));
    assert!(!out
        .captured
        .headers
        .to_lowercase()
        .contains("x-codex-beta-features"));
    for field in [
        "tools",
        "include",
        "stream",
        "reasoning",
        "prompt_cache_key",
    ] {
        assert!(
            out.captured.body.get(field).is_none(),
            "unexpected JSON compact field: {field}"
        );
    }
    assert_eq!(
        &out.captured.body["input"].as_array().unwrap()[1..],
        expected.as_slice()
    );
}
