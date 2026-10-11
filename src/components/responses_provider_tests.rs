use super::*;
use crate::components::model_test_server;

#[test]
fn compatible_responses_http_round_trip_omits_unsupported_fields_and_preserves_reasoning() {
    // Independent expectations: cache key, encrypted include, explicit store:false.
    for (host, prefix, cache, encrypted, store) in [
        ("api.groq.com", "/openai/v1", false, false, true),
        ("api.deepseek.com", "", false, false, true),
        ("api.x.ai", "/v1", false, true, true),
        ("us.api.x.ai", "/v1", false, true, true),
        ("openrouter.ai", "/api/v1", true, true, true),
        ("api.minimax.cn", "/v1", true, false, false),
        ("api.minimax.io", "/v1", true, false, false),
        ("api.moonshot.cn", "/v1", true, false, false),
        ("api.moonshot.ai", "/v1", true, false, false),
        ("ark.cn-beijing.volces.com", "/api/v3", true, true, true),
        ("open.bigmodel.cn", "/api/v1", true, false, true),
        (
            "dashscope.aliyuncs.com",
            "/compatible-mode/v1",
            false,
            false,
            true,
        ),
        (
            "dashscope-intl.aliyuncs.com",
            "/compatible-mode/v1",
            false,
            false,
            true,
        ),
        (
            "dashscope-us.aliyuncs.com",
            "/compatible-mode/v1",
            false,
            false,
            true,
        ),
    ] {
        let thought = json!({"type":"reasoning_text","text":"original thought"});
        let text = json!({"type":"output_text","text":"ok"});
        let single = host == "open.bigmodel.cn";
        let reasoning = json!({"type":"reasoning","id":"rs_synthetic","status":"completed","summary":[],
            "content": if single { thought } else { json!([thought]) }});
        let first = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":{"status":"completed","output":[reasoning,
            {"type":"function_call","id":"fc_synthetic","call_id":"call_synthetic","name":"Inspect","arguments":"{\"value\":1}","status":"completed"}]}})
        );
        let second = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":{"status":"completed","output":[
            {"type":"message","role":"assistant","content":if single { text } else { json!([text]) }}]}})
        );
        let requests = model_test_server::round_trip(
            manifest(),
            move |address| {
                let config = json!({"baseUrl":format!("http://{host}:{}{prefix}", address.port()), "model":"synthetic", "thinking":"high"});
                let mut model = ResponsesModel::from_config(Some(&config));
                model.key = Some("SYNTHETIC_KEY".into());
                model.client = model_test_server::client(host, address);
                Box::new(model)
            },
            first,
            second,
        );
        for (headers, body) in &requests {
            assert!(headers.starts_with(&format!("post {prefix}/responses http/1.1\r\n")));
            assert!(headers.contains("authorization: bearer synthetic_key\r\n"));
            assert_eq!(
                body.get("store"),
                store.then_some(&Value::Bool(false)),
                "{host}"
            );
            assert_eq!(body.get("prompt_cache_key").is_some(), cache, "{host}");
            assert!(body.get("reasoning").is_none(), "{host}");
            assert_eq!(body.get("include").is_some(), encrypted, "{host}");
        }
        let input = requests[1].1["input"].as_array().unwrap();
        assert!(
            input.contains(&reasoning),
            "the original reasoning item must survive: {host} {input:?}"
        );
        assert!(input
            .iter()
            .any(|item| item["type"] == "function_call" && item["call_id"] == "call_synthetic"));
        assert!(input
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["call_id"] == "call_synthetic"));
    }
}
