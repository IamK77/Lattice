//! An opt-in whole browser/model loop against an explicitly configured proxy.
//! Approval is scripted ONLY for a fixed, owned localhost page and marker text.
use lattice::components::{browser_tools, responses_model};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft, EventEnvelope, Factory,
    Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
struct Driver;
impl Component for Driver {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}

#[test]
#[ignore = "paid proxy browser loop; only a private Chromium and an owned localhost page"]
fn model_observes_clicks_types_and_observes_the_result() {
    let base = std::env::var("LATTICE_MEDIA_BASE_URL").expect("explicit proxy URL");
    let model = std::env::var("LATTICE_MEDIA_MODEL").expect("explicit model");
    assert!(std::env::var("LATTICE_MEDIA_TEST_KEY").is_ok());
    let (url, server) = page();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let mut driver = responses_model::manifest();
    driver.name = "driver".into();
    driver.entry = "builtin:driver".into();
    driver.implements.clear();
    driver.tools.clear();
    driver.events.clear();
    driver.inputs = vec![
        PortDecl::new("model", &[ce::MODEL_CALL_COMPLETED]),
        PortDecl::new("questions", &[browser_tools::AUTH_REQUESTED]),
        PortDecl::new("tools", &[ce::TOOL_EXEC_COMPLETED]),
    ];
    driver.outputs = vec![
        PortDecl::new("user", &[ce::USER_MESSAGE]),
        PortDecl::new("request", &[ce::MODEL_CALL_STARTED]),
        PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("answer", &[ce::EXTERNAL_INPUT]),
    ];
    let registry = [
        ("driver".into(), driver),
        (responses_model::NAME.into(), responses_model::manifest()),
        (browser_tools::NAME.into(), browser_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".into(), Box::new(|_| Box::new(Driver)));
    factories.insert(
        responses_model::NAME.into(),
        Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    );
    factories.insert(
        browser_tools::NAME.into(),
        Box::new(|c| Box::new(browser_tools::BrowserTools::from_config(c))),
    );
    let mut kernel=Kernel::start(&AssemblyManifest{instances:[("driver".into(),ComponentInstance::new("driver",None)),("model".into(),ComponentInstance::new(responses_model::NAME,Some(json!({"model":model,"baseUrl":base,"apiKeyEnv":"LATTICE_MEDIA_TEST_KEY","maxTokens":2048})))),("browser".into(),ComponentInstance::new(browser_tools::NAME,None))].into(),wires:vec![Wire::new("driver.request","model.request"),Wire::new("model.result","driver.model"),Wire::new("driver.execute","browser.execute"),Wire::new("browser.request","driver.questions"),Wire::new("driver.answer","browser.answer"),Wire::new("browser.outcome","driver.tools")]},&registry,&mut factories,KernelOptions{log_file:Some(path.clone()),..Default::default()}).unwrap();
    kernel.injector("driver").emit("user",EventDraft::new(ce::USER_MESSAGE,&[],json!({"text":format!("This is an authorized test on our owned localhost page only. Navigate Browser to {url}. Observe the screenshot, click the Change button, then type exactly marker in the visible input. Verify the background becomes green from the resulting screenshot and stop. Do not navigate anywhere else or use scripts. Each permitted batch is approved by the test fixture. Do not close the browser until you have observed the result.")})));
    kernel.run_until_quiescent().unwrap();
    let mut observed_green = false;
    for _ in 0..6 {
        let parts: Vec<Value> = kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .filter(|e| {
                [
                    ce::USER_MESSAGE,
                    ce::MODEL_CALL_COMPLETED,
                    ce::TOOL_EXEC_COMPLETED,
                ]
                .contains(&e.event_type.as_str())
            })
            .map(|e| json!({"event":e.id}))
            .collect();
        let mut hash = Sha256::new();
        for part in &parts {
            hash.update(part["event"].as_str().unwrap().as_bytes());
            hash.update(b"\n");
        }
        kernel.injector("driver").emit("request",EventDraft::new(ce::MODEL_CALL_STARTED,&[],json!({"model":model,"input":{"parts":parts,"fingerprint":format!("sha256:{:x}",hash.finalize())},"system":"Use only the supplied Browser tool for the user's owned test page. Read screenshots rather than guessing. This test has a six-model-call limit.","tools":browser_tools::manifest().tools})));
        kernel.run_until_quiescent().unwrap();
        let reply = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .rev()
            .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
            .unwrap();
        assert_eq!(reply.payload["status"], "ok", "{}", reply.payload);
        let calls = reply.payload["toolCalls"].as_array().unwrap();
        if calls.is_empty() {
            break;
        }
        for call in calls {
            assert_eq!(call["tool"], "Browser");
            let actions = call["arguments"]["actions"].as_array().unwrap();
            assert!(actions.len() <= 8);
            for action in actions {
                match action["type"].as_str().unwrap() {
                    "navigate" => assert_eq!(
                        action["url"], url,
                        "the fixture never authorizes an external site"
                    ),
                    "type" => assert_eq!(
                        action["text"], "marker",
                        "the fixture never authorizes arbitrary text"
                    ),
                    "click" | "double_click" | "move" | "scroll" | "screenshot" | "close" => {}
                    other => panic!("fixture refuses action {other}"),
                }
            }
            kernel.injector("driver").emit(
                "execute",
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[&reply.id],
                    json!({"call":call["id"],"tool":"Browser","arguments":call["arguments"]}),
                ),
            );
            kernel.run_until_quiescent().unwrap();
            let log = kernel.log().replay(1).unwrap();
            let request = log
                .iter()
                .rev()
                .find(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.payload["call"] == call["id"])
                .unwrap();
            if let Some(question) = log.iter().rev().find(|e| {
                e.event_type == browser_tools::AUTH_REQUESTED && e.payload["request"] == request.id
            }) {
                kernel.injector("driver").emit("answer",EventDraft::new(ce::EXTERNAL_INPUT,&[&question.id],json!({"channel":lattice::components::trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true})));
                kernel.run_until_quiescent().unwrap();
            }
            let result = kernel
                .log()
                .replay(1)
                .unwrap()
                .into_iter()
                .rev()
                .find(|e| {
                    e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == call["id"]
                })
                .expect("browser result");
            assert_eq!(result.payload["status"], "ok", "{}", result.payload);
            assert!(
                result.payload["result"]["problem"].is_null(),
                "{}",
                result.payload
            );
            if let Some(image) = result.payload["result"]["latticeImages"]
                .as_array()
                .and_then(|v| v.first())
            {
                let reference = lattice::contracts::document::DocRef::of(image).unwrap();
                let bytes = reference
                    .read_bytes(&lattice::contracts::document::documents_dir(&path))
                    .unwrap();
                let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
                    .read_info()
                    .unwrap();
                let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
                let info = reader.next_frame(&mut pixels).unwrap();
                let at = (700 * info.width as usize + 1000) * info.color_type.samples();
                observed_green |= pixels[at..at + 3] == [0, 255, 0];
            }
        }
    }
    assert!(
        observed_green,
        "real screenshots never confirmed the input changed the page to green"
    );
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("data:image"),
        "screenshots must not be inlined in the ledger"
    );
    drop(kernel);
    server.join().unwrap();
}
fn page() -> (String, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let served = Arc::new(AtomicBool::new(false));
        let mut handlers = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            if served.load(Ordering::SeqCst) {
                break;
            }
            let served = served.clone();
            handlers.push(std::thread::spawn(move || {
                let mut reader=BufReader::new(stream.try_clone().unwrap());let mut line=String::new();
                if reader.read_line(&mut line).unwrap_or(0)==0{return;}
                loop{line.clear();if reader.read_line(&mut line).unwrap_or(0)==0||line=="\r\n"{break;}}
                let html=r#"<!doctype html><body style="margin:0;background:rgb(255,0,0)"><button style="position:absolute;left:10px;top:10px;width:180px;height:60px" onclick="document.body.style.background='rgb(0,0,255)'">Change</button><input style="position:absolute;left:10px;top:100px;width:180px;height:60px" oninput="if(this.value==='marker')document.body.style.background='rgb(0,255,0)'">"#;
                if write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",html.len(),html).is_ok(){served.store(true,Ordering::SeqCst);let _=std::net::TcpStream::connect(address);}
            }));
        }
        for handler in handlers {
            handler.join().unwrap();
        }
    });
    (format!("http://{address}/"), thread)
}
