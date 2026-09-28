//! Complete generated-image shell, including a stateless edit after reopening.
use lattice::components::responses_model;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
struct Driver;
impl Component for Driver {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}
fn start(path: &std::path::Path, base: &str, model: &str) -> Kernel {
    let mut driver: ComponentManifest = responses_model::manifest();
    driver.name = "driver".into();
    driver.entry = "builtin:driver".into();
    driver.inputs.clear();
    driver.implements.clear();
    driver.tools.clear();
    driver.events.clear();
    driver.outputs = vec![
        PortDecl::new("user", &[ce::USER_MESSAGE]),
        PortDecl::new("request", &[ce::MODEL_CALL_STARTED]),
    ];
    let registry = [
        ("driver".into(), driver),
        (responses_model::NAME.into(), responses_model::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".into(), Box::new(|_| Box::new(Driver)));
    factories.insert(
        responses_model::NAME.into(),
        Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    );
    Kernel::start(&AssemblyManifest{instances:[("driver".into(),ComponentInstance::new("driver",None)),("model".into(),ComponentInstance::new(responses_model::NAME,Some(json!({"model":model,"baseUrl":base,"apiKeyEnv":"LATTICE_MEDIA_TEST_KEY","nativeImageGeneration":true,"maxTokens":4096}))))].into(),wires:vec![Wire::new("driver.request","model.request")]},&registry,&mut factories,KernelOptions{log_file:Some(path.into()),stream:lattice::kernel::log::EventLog::stream_of(path),..Default::default()}).unwrap()
}
fn turn(kernel: &mut Kernel, text: &str, model: &str) -> Value {
    kernel.injector("driver").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":text})),
    );
    kernel.run_until_quiescent().unwrap();
    let parts: Vec<Value> = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .filter(|e| e.event_type == ce::USER_MESSAGE || e.event_type == ce::MODEL_CALL_COMPLETED)
        .map(|e| json!({"event":e.id}))
        .collect();
    let mut hash = Sha256::new();
    for p in &parts {
        hash.update(p["event"].as_str().unwrap().as_bytes());
        hash.update(b"\n");
    }
    kernel.injector("driver").emit("request",EventDraft::new(ce::MODEL_CALL_STARTED,&[],json!({"model":model,"input":{"parts":parts,"fingerprint":format!("sha256:{:x}",hash.finalize())},"system":"Generate or edit the requested image using the image_generation tool. Use a simple small image. Do not substitute a text-only answer."})));
    kernel.run_until_quiescent().unwrap();
    kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .rev()
        .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
        .expect("model result")
        .payload
        .clone()
}
fn assert_artifact(result: &Value, path: &std::path::Path) {
    assert_eq!(result["status"], "ok", "{result}");
    let image = result["responsesOutput"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["type"] == "image_generation_call")
        .expect("generated image");
    assert!(
        image.get("result").is_none(),
        "base64 must not enter the ledger"
    );
    let reference = lattice::contracts::document::DocRef::of(&image["image"]).unwrap();
    assert!(reference
        .read_bytes(&lattice::contracts::document::documents_dir(path))
        .unwrap()
        .starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(result["text"]
        .as_str()
        .unwrap()
        .contains(image["savedPath"].as_str().unwrap()));
}
#[test]
fn generated_images_are_saved_before_audit_and_restored_as_pixels_after_reopen() {
    use base64::Engine;
    use std::io::{Read, Write};
    let mut pixels = Vec::new();
    {
        let encoder = png::Encoder::new(&mut pixels, 1, 1);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0])
            .unwrap();
    }
    let image = base64::engine::general_purpose::STANDARD.encode(pixels);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let headers = String::from_utf8(bytes).unwrap();
            let len: usize = headers
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; len];
            stream.read_exact(&mut body).unwrap();
            bodies.push(serde_json::from_slice::<Value>(&body).unwrap());
            let data = format!(
                "data: {}\n\n",
                json!({"type":"response.completed","response":{"status":"completed","output":[{"id":"ig_one","type":"image_generation_call","status":"completed","result":image}]}})
            );
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",data.len(),data).unwrap();
        }
        bodies
    });
    std::env::set_var("LATTICE_MEDIA_TEST_KEY", "synthetic-not-a-secret");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let mut kernel = start(&path, &base, "exam");
    assert_artifact(&turn(&mut kernel, "Generate a red square.", "exam"), &path);
    drop(kernel);
    let mut reopened = start(&path, &base, "exam");
    assert_artifact(
        &turn(&mut reopened, "Make that square blue.", "exam"),
        &path,
    );
    let events = reopened.log().replay(1).unwrap();
    let costs: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == responses_model::CALL_COST)
        .collect();
    assert_eq!(costs.len(), 2);
    for cost in &costs {
        assert_eq!(cost.source, "model");
        assert_eq!(cost.causes.len(), 1);
        assert!(events
            .iter()
            .any(|e| e.event_type == ce::MODEL_CALL_COMPLETED && e.causes == cost.causes));
        assert_eq!(cost.payload["status"], "ok");
        let timing: lattice::startup::Timings =
            serde_json::from_value(cost.payload["timing"].clone()).unwrap();
        for phase in [
            "fingerprint_verified",
            "messages_materialized",
            "dialect_encoded",
            "media_restored",
            "material_temporaries_released",
            "body_attempt_finished",
            "request_encoded",
            "response_headers",
            "first_response_bytes",
            "terminal_response_parsed",
            "response_normalized",
            "request_and_response_released",
        ] {
            assert!(
                timing.memory.iter().any(|point| point.phase == phase),
                "missing {phase}"
            );
        }
        assert!((timing.total_ms - timing.phases_ms.values().sum::<f64>()).abs() < 1e-6);
        assert!(cost.payload["counts"]["sseBytesRead"].as_u64().unwrap() > 0);
        assert!(cost.payload["historyAfter"]["stats"]["cache"]["decodes"].is_u64());
        let text = cost.payload.to_string();
        for forbidden in [
            "synthetic-not-a-secret",
            "Generate a red square",
            "data:image",
            base.as_str(),
        ] {
            assert!(!text.contains(forbidden), "diagnostic leaked request data");
        }
        assert!(
            text.len() < 32768,
            "one bounded observation, not one per chunk"
        );
    }
    let bodies = server.join().unwrap();
    for (cost, body) in costs.iter().zip(&bodies) {
        assert_eq!(
            cost.payload["counts"]["requestBytes"],
            serde_json::to_vec(body).unwrap().len()
        );
    }
    drop(reopened);
    assert!(bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["type"] == "image_generation"));
    assert!(bodies[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["content"].as_array().into_iter().flatten())
        .any(|c| c["type"] == "input_image"));
    assert!(bodies[1].get("previous_response_id").is_none());
    assert!(!std::fs::read_to_string(path)
        .unwrap()
        .contains("data:image"));
}
#[test]
fn stopping_an_inflight_call_still_has_one_outcome() {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("LATTICE_MEDIA_TEST_KEY", "synthetic-not-a-secret");
    let mut kernel = start(
        &dir.path().join("ledger.jsonl"),
        &format!("http://{}", listener.local_addr().unwrap()),
        "exam",
    );
    let stopper = kernel.stop_handle();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        use std::os::fd::AsRawFd;
        let mut ready = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // Bound a regression that never reaches HTTP without polling or sleeping.
        assert_eq!(
            unsafe { libc::poll(&mut ready, 1, 10_000) },
            1,
            "request never reached the fixture"
        );
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        assert!(stopper.request("cancel offline observation fixture".into(), None));
        // Keep the HTTP response pending until the runtime has stopped it.
        release_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
    });
    kernel.injector("driver").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"wait"})),
    );
    kernel.run_until_quiescent().unwrap();
    let input = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .unwrap();
    let mut hash = Sha256::new();
    hash.update(input.id.as_bytes());
    hash.update(b"\n");
    kernel.injector("driver").emit("request", EventDraft::new(ce::MODEL_CALL_STARTED, &[], json!({
        "model":"exam", "input":{"parts":[{"event":input.id}],"fingerprint":format!("sha256:{:x}", hash.finalize())}
    })));
    kernel.run_until_quiescent().unwrap();
    let log = kernel.shutdown();
    release_tx.send(()).unwrap();
    server.join().unwrap();
    let events = log.replay(1).unwrap();
    let request = events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .unwrap();
    let outcomes: Vec<_> = events
        .iter()
        .filter(|e| ce::is_outcome(&e.event_type) && e.causes.contains(&request.id))
        .collect();
    assert_eq!(outcomes.len(), 1);
    for e in &events {
        assert_ne!(e.event_type, ce::ERROR, "{e:?}");
        if e.event_type == responses_model::CALL_COST {
            assert_eq!(e.payload["status"], "cancelled");
        }
    }
}

#[test]
fn invalid_material_records_cost_without_sending_a_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = start(
        &dir.path().join("ledger.jsonl"),
        &format!("http://{}", listener.local_addr().unwrap()),
        "exam",
    );
    kernel.injector("driver").emit("request", EventDraft::new(ce::MODEL_CALL_STARTED, &[], json!({
        "model":"exam", "input":{"parts":[{"inline":{"role":"user","content":"private fixture text"}}], "fingerprint":format!("sha256:{}", "0".repeat(64))}
    })));
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    let cost = events
        .iter()
        .find(|e| e.event_type == responses_model::CALL_COST)
        .expect("failed calls must still be observed");
    assert_eq!(cost.payload["status"], "error");
    assert!(cost.payload["counts"]["requestBytes"].is_null());
    let completed: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].payload["error"]["code"], "material.invalid");
    assert_eq!(cost.causes, completed[0].causes);
    assert!(!cost.payload.to_string().contains("private fixture text"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
#[ignore = "manual paid proxy image generation and stateless editing; requires explicit environment"]
fn responses_live_images_survive_reopen_and_edit() {
    let base = std::env::var("LATTICE_MEDIA_BASE_URL").expect("explicit proxy URL");
    let model = std::env::var("LATTICE_MEDIA_MODEL").expect("explicit model");
    assert!(std::env::var("LATTICE_MEDIA_TEST_KEY").is_ok());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let mut kernel = start(&path, &base, &model);
    assert_artifact(
        &turn(
            &mut kernel,
            "Generate a simple solid red square icon on white, low quality, 1024 square PNG.",
            &model,
        ),
        &path,
    );
    drop(kernel);
    let mut reopened = start(&path, &base, &model);
    assert_artifact(&turn(&mut reopened,"Edit the earlier image: make the square blue; keep the white background. Use the supplied pixels.",&model),&path);
}
