use super::*;

const INVALID_JSON: &str = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"inspect","input":{}}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{broken"}}

data: {"type":"content_block_stop","index":0}

data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}

data: {"type":"message_stop"}

"#;

const NON_OBJECT: &str = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"inspect","input":{}}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"null"}}

data: {"type":"content_block_stop","index":0}

data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}

data: {"type":"message_stop"}

"#;

const INVALID_DELTA: &str = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"inspect","input":{}}}

data: {"type":"content_block_delta","index":0,"delta":{"partial_json":"{broken"}}

data: {"type":"content_block_stop","index":0}

data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}

data: {"type":"message_stop"}

"#;

const INCOMPLETE: &str = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"inspect","input":{}}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}

data: {"type":"content_block_stop","index":0}

"#;

const EMPTY_OBJECT: &str = r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"inspect","input":{}}}

data: {"type":"content_block_stop","index":0}

data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}

data: {"type":"message_stop"}

"#;

fn candidate() -> Candidate {
    candidates()
        .into_iter()
        .find(|candidate| candidate.manifest.name == anthropic_model::NAME)
        .unwrap()
}

#[test]
fn anthropic_invalid_or_unfinished_tools_have_one_error_outcome_and_no_retry() {
    ensure_key();
    for body in [INVALID_JSON, NON_OBJECT, INVALID_DELTA, INCOMPLETE] {
        let (url, hits, _) = fake_api(vec![Canned::Sse(body)]);
        let payload = run_call(candidate(), &url, None);
        assert_eq!(payload["status"], "error", "{payload}");
        assert_eq!(
            payload["error"]["code"], "provider.invalid_tool_call",
            "{payload}"
        );
        assert!(payload.get("toolCalls").is_none(), "{payload}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn anthropic_complete_empty_tool_and_interrupted_transport_remain_distinct() {
    ensure_key();
    let (url, hits, _) = fake_api(vec![Canned::Sse(EMPTY_OBJECT)]);
    let payload = run_call(candidate(), &url, None);
    assert_eq!(payload["status"], "ok", "{payload}");
    assert_eq!(
        payload["toolCalls"],
        json!([{"id": "tool_1", "tool": "inspect", "arguments": {}}])
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let (url, hits, _) = fake_api(vec![Canned::CutMidStream(INCOMPLETE)]);
    let payload = run_call(candidate(), &url, None);
    assert_eq!(
        payload["error"]["code"], "provider.stream_interrupted",
        "{payload}"
    );
    assert!(payload.get("toolCalls").is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
