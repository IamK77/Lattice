//! Synthetic provider wire shapes through the real HTTP adapter and ledger.
//! These prove local round-trip behavior, not live provider/account compatibility.
use super::*;

const GEMINI: &str = concat!(
    "data: ",
    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{ \"n\": 1 }"},"extra_content":{"google":{"thought_signature":"SYNTHETIC_SIGNATURE"}}}]},"finish_reason":"tool_calls"}]}"#,
    "\n\n",
    "data: [DONE]\n\n"
);
const ROUTER: &str = concat!(
    "data: ",
    r#"{"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.encrypted","index":0,"data":"SYNTHETIC_ENCRYPTED"}]}}]}"#,
    "\n\n",
    "data: ",
    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
    "\n\n",
    "data: [DONE]\n\n"
);
const MISTRAL: &str = concat!(
    "data: ",
    r#"{"choices":[{"delta":{"content":[{"type":"thinking","thinking":[{"type":"text","text":"synthetic thought"}]},{"type":"text","text":"checking"}]}}]}"#,
    "\n\n",
    "data: ",
    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
    "\n\n",
    "data: [DONE]\n\n"
);
const TOGETHER: &str = concat!(
    "data: ",
    r#"{"choices":[{"delta":{"reasoning":"synthetic thought","tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
    "\n\n",
    "data: [DONE]\n\n"
);
const MINIMAX: &str = concat!(
    "data: ",
    r#"{"choices":[{"delta":{"content":"<think>synthetic thought</think>checking","tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
    "\n\n",
    "data: [DONE]\n\n"
);

#[test]
fn chat_provider_extensions_reach_the_second_authenticated_http_request() {
    ensure_key();
    for (kind, stream) in [
        ("gemini", GEMINI),
        ("router", ROUTER),
        ("mistral", MISTRAL),
        ("together", TOGETHER),
        ("minimax", MINIMAX),
    ] {
        let (base, hits, bodies) = fake_api(vec![
            Canned::ChatSse(stream),
            Canned::ChatSse(happy_sse(openai_model::NAME)),
        ]);
        let mut driver = driver_manifest();
        driver
            .inputs
            .push(PortDecl::new("seen", &[ce::MODEL_CALL_COMPLETED]));
        driver
            .outputs
            .push(PortDecl::new("tool", &[ce::TOOL_EXEC_COMPLETED]));
        let registry: HashMap<_, _> = [
            ("shell-driver".into(), driver),
            (openai_model::NAME.into(), openai_model::manifest()),
        ]
        .into();
        let mut factories: HashMap<String, Factory> = HashMap::new();
        factories.insert("shell-driver".into(), Box::new(|_| Box::new(Noop)));
        factories.insert(
            openai_model::NAME.into(),
            Box::new(|c| Box::new(openai_model::OpenAiModel::from_config(c))),
        );
        let assembly = AssemblyManifest {
            instances: [
                ("driver".into(), ComponentInstance { component:"shell-driver".into(), requires:vec![], config:None }),
                ("model".into(), ComponentInstance { component:openai_model::NAME.into(), requires:vec![], config:Some(json!({"baseUrl":base,"apiKeyEnv":"LATTICE_SHELL_TEST_KEY_UNSET","model":"synthetic"})) }),
            ].into(),
            wires: vec![Wire::new("driver.req", "model.request"), Wire::new("model.result", "driver.seen")],
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
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text":"Use the synthetic lookup tool."}),
            ),
        );
        kernel.run_until_quiescent().unwrap();
        let user = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .find(|e| e.event_type == ce::USER_MESSAGE)
            .unwrap();
        let parts = vec![json!({"event":user.id})];
        let tools = json!([{"name":"lookup","description":"Synthetic fixture","parameters":{"type":"object","properties":{"n":{"type":"integer"}}}}]);
        injector.emit("req", EventDraft::new(ce::MODEL_CALL_STARTED, &[&user.id], json!({"model":"synthetic","tools":tools,"input":{"parts":parts,"fingerprint":fingerprint(&parts)}})));
        kernel.run_until_quiescent().unwrap();
        let reply = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
            .unwrap();
        assert_eq!(reply.payload["status"], "ok", "{kind}: {:?}", reply.payload);
        assert_eq!(reply.payload["toolCalls"][0]["tool"], "lookup");
        injector.emit(
            "tool",
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&reply.id],
                json!({"call":"call","status":"ok","result":{"synthetic":true}}),
            ),
        );
        kernel.run_until_quiescent().unwrap();
        let result = kernel
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
            .unwrap();
        let parts = vec![
            json!({"event":user.id}),
            json!({"event":reply.id}),
            json!({"event":result.id}),
        ];
        injector.emit("req", EventDraft::new(ce::MODEL_CALL_STARTED, &[&result.id], json!({"model":"synthetic","tools":tools,"input":{"parts":parts,"fingerprint":fingerprint(&parts)}})));
        kernel.run_until_quiescent().unwrap();
        let events = kernel.log().replay(1).unwrap();
        let completions = events
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
            .collect::<Vec<_>>();
        assert_eq!(completions.len(), 2, "{kind}");
        assert_eq!(completions[1].payload["text"], "ok");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let body: Value = serde_json::from_str(&bodies.lock().unwrap()[1]).unwrap();
        let assistant = &body["messages"][1];
        assert_eq!(body["messages"][2]["tool_call_id"], "call");
        match kind {
            "gemini" => {
                assert_eq!(
                    assistant["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
                    "SYNTHETIC_SIGNATURE"
                );
                assert_eq!(
                    assistant["tool_calls"][0]["function"]["arguments"],
                    "{ \"n\": 1 }"
                );
            }
            "router" => assert_eq!(
                assistant["reasoning_details"][0]["data"],
                "SYNTHETIC_ENCRYPTED"
            ),
            "mistral" => assert_eq!(
                assistant["content"][0]["thinking"][0]["text"],
                "synthetic thought"
            ),
            "together" => assert_eq!(assistant["reasoning"], "synthetic thought"),
            "minimax" => assert_eq!(
                assistant["content"],
                "<think>synthetic thought</think>checking"
            ),
            _ => unreachable!(),
        }
        for event in events {
            assert!(!serde_json::to_string(&event)
                .unwrap()
                .contains("test-key-not-a-secret"));
        }
        kernel.shutdown();
    }
}
