use super::*;
use crate::components::model_test_server;

#[test]
fn compatible_messages_http_round_trip_keeps_signature_and_uses_endpoint_auth() {
    let first = [
        json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"original thought"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SYNTHETIC_SIGNATURE"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_synthetic","name":"Inspect","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"value\":1}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ].iter().map(|event| format!("data: {event}\n\n")).collect::<String>();
    let second = [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
        json!({"type":"message_stop"}),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();
    for (host, prefix, bearer) in [
        ("api.minimax.cn", "/anthropic", true),
        ("api.minimax.io", "/anthropic", true),
        ("openrouter.ai", "/api", true),
        ("api.moonshot.cn", "/anthropic", true),
        ("api.moonshot.ai", "/anthropic", true),
        ("api.siliconflow.cn", "", true),
        ("api.siliconflow.com", "", true),
        ("ark.cn-beijing.volces.com", "/api/compatible", false),
        ("dashscope.aliyuncs.com", "/apps/anthropic", false),
        ("dashscope-intl.aliyuncs.com", "/apps/anthropic", false),
        ("dashscope-us.aliyuncs.com", "/apps/anthropic", false),
    ] {
        let requests = model_test_server::round_trip(
            manifest(),
            move |address| {
                let config = json!({"baseUrl":format!("http://{host}:{}{prefix}", address.port()), "model":"synthetic", "thinking":"high"});
                let mut model = AnthropicModel::from_config(Some(&config));
                model.api_key = Some("SYNTHETIC_KEY".into());
                model.client = model_test_server::client(host, address);
                Box::new(model)
            },
            first.clone(),
            second.clone(),
        );
        for (headers, body) in &requests {
            assert!(headers.starts_with(&format!("post {prefix}/v1/messages http/1.1\r\n")));
            assert_eq!(
                headers.contains("authorization: bearer synthetic_key\r\n"),
                bearer
            );
            assert_eq!(headers.contains("x-api-key: synthetic_key\r\n"), !bearer);
            assert!(body.get("output_config").is_none());
            assert!(body.get("thinking").is_none());
        }
        let messages = requests[1].1["messages"].as_array().unwrap();
        assert_eq!(
            messages[1]["content"][0],
            json!({"type":"thinking","thinking":"original thought","signature":"SYNTHETIC_SIGNATURE"})
        );
        assert_eq!(messages[1]["content"][1]["input"], json!({"value":1}));
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_synthetic");
    }
}
