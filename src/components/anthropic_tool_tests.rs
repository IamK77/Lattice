use super::*;

fn start(input: Value) -> Accumulator {
    let mut acc = Accumulator::default();
    acc.apply(&json!({"type": "message_start", "message": {"usage": {"input_tokens": 7}}}));
    acc.apply(&json!({"type": "content_block_start", "index": 0,
        "content_block": {"type": "thinking", "thinking": "check the input", "signature": "synthetic"}}));
    acc.apply(&json!({"type": "content_block_stop", "index": 0}));
    acc.apply(&json!({"type": "content_block_start", "index": 1,
        "content_block": {"type": "text", "text": ""}}));
    acc.apply(&json!({"type": "content_block_delta", "index": 1,
        "delta": {"type": "text_delta", "text": "Before the tool"}}));
    acc.apply(&json!({"type": "content_block_stop", "index": 1}));
    acc.apply(&tool_start(2, "tool_1", input));
    acc
}

fn tool_start(index: u64, id: &str, input: Value) -> Value {
    json!({"type": "content_block_start", "index": index,
        "content_block": {"type": "tool_use", "id": id, "name": "inspect", "input": input}})
}

fn delta(acc: &mut Accumulator, fragment: Value) {
    acc.apply(&json!({"type": "content_block_delta", "index": 2,
        "delta": {"type": "input_json_delta", "partial_json": fragment}}));
}

fn stop_block(acc: &mut Accumulator) {
    acc.apply(&json!({"type": "content_block_stop", "index": 2}));
}

fn stop_message(acc: &mut Accumulator, reason: &str) {
    acc.apply(
        &json!({"type": "message_delta", "delta": {"stop_reason": reason},
        "usage": {"output_tokens": 11}}),
    );
    acc.apply(&json!({"type": "message_stop"}));
}

fn assert_invalid(acc: &Accumulator) {
    let payload = acc.finish();
    assert_eq!(payload["status"], "error", "{payload}");
    assert_eq!(
        payload["error"]["code"], "provider.invalid_tool_call",
        "{payload}"
    );
    assert_eq!(payload["error"]["blame"], "provider");
    assert_eq!(payload["error"]["retryable"], false);
    assert!(!payload["error"]["message"].as_str().unwrap().is_empty());
    assert!(payload.get("toolCalls").is_none(), "{payload}");
    assert_eq!(payload["text"], "Before the tool");
    assert_eq!(payload["usage"]["input_tokens"], 7);
    assert_eq!(payload["reasoning"][0]["text"], "check the input");
}

#[test]
fn invalid_json_never_becomes_empty_arguments() {
    for fragment in ["{broken", "{\"path\":", "{\"path\":\"x\"", " "] {
        let mut acc = start(json!({}));
        delta(&mut acc, json!(fragment));
        stop_block(&mut acc);
        stop_message(&mut acc, "tool_use");
        assert_invalid(&acc);
        let payload = acc.finish();
        assert!(payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains("JSON"));
        assert_eq!(payload["stopReason"], "tool_use");
        assert_eq!(payload["usage"]["output_tokens"], 11);
    }
}

#[test]
fn non_object_arguments_are_not_executable() {
    for fragment in ["null", "[]", "[1]", "true", "4", "\"text\""] {
        let mut acc = start(json!({}));
        delta(&mut acc, json!(fragment));
        stop_block(&mut acc);
        stop_message(&mut acc, "tool_use");
        assert_invalid(&acc);
    }
}

#[test]
fn initial_objects_and_independent_delta_buffers_are_preserved() {
    for (input, fragments, expected) in [
        (json!({}), vec![], json!({})),
        (json!({}), vec![""], json!({})),
        (json!({}), vec!["", "{}"], json!({})),
        (json!({"initial": 1}), vec![], json!({"initial": 1})),
        (json!({"initial": 1}), vec![""], json!({"initial": 1})),
        (
            json!({"initial": 1}),
            vec!["{\"path\":", "\"中文\"}"],
            json!({"path": "中文"}),
        ),
    ] {
        let mut acc = start(input);
        for fragment in fragments {
            delta(&mut acc, json!(fragment));
        }
        stop_block(&mut acc);
        stop_message(&mut acc, "tool_use");
        let payload = acc.finish();
        assert_eq!(payload["status"], "ok", "{payload}");
        assert_eq!(
            payload["toolCalls"],
            json!([{"id": "tool_1", "tool": "inspect", "arguments": expected}])
        );
    }
}

#[test]
fn unfinished_or_late_tool_events_fail_the_whole_call() {
    for boundary in 0..8 {
        let mut acc = start(json!({}));
        delta(&mut acc, json!("{}"));
        if boundary != 0 {
            stop_block(&mut acc);
        }
        match boundary {
            0 => stop_message(&mut acc, "tool_use"),
            1 => { /* Complete block, but EOF before message termination. */ }
            2 => {
                acc.apply(&json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));
            }
            3 => {
                acc.apply(&json!({"type": "message_stop"}));
            }
            4 => stop_message(&mut acc, "end_turn"),
            5 => {
                delta(&mut acc, json!(""));
                stop_message(&mut acc, "tool_use");
            }
            6 => {
                stop_message(&mut acc, "tool_use");
                delta(&mut acc, json!(""));
            }
            7 => {
                stop_message(&mut acc, "tool_use");
                acc.apply(&tool_start(3, "late", json!({})));
                acc.apply(&json!({"type": "content_block_stop", "index": 3}));
            }
            _ => unreachable!(),
        }
        assert_invalid(&acc);
    }
}

#[test]
fn malformed_shapes_and_duplicate_identity_are_rejected() {
    for input in [Value::Null, json!([]), json!("{}"), json!(1)] {
        let mut acc = start(input);
        delta(&mut acc, json!("{}"));
        stop_block(&mut acc);
        stop_message(&mut acc, "tool_use");
        assert_invalid(&acc);
    }
    for fragment in [Value::Null, json!({}), json!(7)] {
        let mut acc = start(json!({}));
        delta(&mut acc, fragment);
        stop_block(&mut acc);
        stop_message(&mut acc, "tool_use");
        assert_invalid(&acc);
    }
    for bad in [
        tool_start(2, "duplicate-index", json!({})),
        tool_start(3, "tool_1", json!({})),
        tool_start(3, "", json!({})),
        json!({"type": "content_block_start", "index": 3, "content_block": {"type": "tool_use", "id": "other", "input": {}}}),
        json!({"type": "content_block_start", "content_block": {"type": "tool_use", "id": "other", "name": "inspect", "input": {}}}),
        json!({"type": "content_block_delta", "index": 99, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
        json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
    ] {
        let mut acc = start(json!({}));
        stop_block(&mut acc);
        acc.apply(&bad);
        acc.apply(&json!({"type": "content_block_stop", "index": 3}));
        stop_message(&mut acc, "tool_use");
        assert_invalid(&acc);
    }
}

#[test]
fn invalid_delta_kinds_cannot_silently_leave_executable_initial_arguments() {
    for change in [
        Value::Null,
        json!({}),
        json!({"type": "text_delta", "text": "not arguments"}),
        json!({"type": "thinking_delta", "thinking": "not arguments"}),
        json!({"type": "signature_delta", "signature": "not arguments"}),
        json!({"type": "unsupported_argument_delta", "partial_json": "{}"}),
    ] {
        let mut acc = start(json!({}));
        acc.apply(&json!({"type": "content_block_delta", "index": 2, "delta": change}));
        stop_block(&mut acc);
        stop_message(&mut acc, "tool_use");
        assert_invalid(&acc);
    }
}

#[test]
fn complete_multiple_tools_remain_ordered_and_cancellation_discards_them() {
    let mut acc = start(json!({"first": 1}));
    delta(&mut acc, json!(""));
    stop_block(&mut acc);
    acc.apply(&tool_start(3, "tool_2", json!({})));
    for fragment in ["{\"second\":", "2}"] {
        acc.apply(&json!({"type": "content_block_delta", "index": 3,
            "delta": {"type": "input_json_delta", "partial_json": fragment}}));
    }
    acc.apply(&json!({"type": "content_block_stop", "index": 3}));
    stop_message(&mut acc, "tool_use");
    let payload = acc.finish();
    assert_eq!(payload["status"], "ok");
    assert_eq!(
        payload["toolCalls"],
        json!([
            {"id": "tool_1", "tool": "inspect", "arguments": {"first": 1}},
            {"id": "tool_2", "tool": "inspect", "arguments": {"second": 2}},
        ])
    );
    let cancelled = acc.finish_cancelled();
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled.get("toolCalls").is_none());
}

#[test]
fn one_bad_tool_blocks_other_valid_tools_and_terminal_failures_stay_nonexecutable() {
    let mut acc = start(json!({}));
    delta(&mut acc, json!("{broken"));
    stop_block(&mut acc);
    acc.apply(&tool_start(3, "valid", json!({"path": "x"})));
    acc.apply(&json!({"type": "content_block_stop", "index": 3}));
    stop_message(&mut acc, "tool_use");
    assert_invalid(&acc);
    for payload in [
        acc.finish_cancelled(),
        acc.finish_error("synthetic transport failure"),
    ] {
        assert_ne!(payload["status"], "ok");
        assert!(payload.get("toolCalls").is_none());
        assert_eq!(payload["text"], "Before the tool");
        assert_eq!(payload["reasoning"][0]["text"], "check the input");
    }
    acc.stop_reason = Some("max_tokens".into());
    assert_eq!(
        acc.finish()["error"]["code"],
        "provider.truncated_tool_call"
    );
    assert!(acc.finish().get("toolCalls").is_none());
    acc.apply(&json!({"type": "error", "error": {"message": "synthetic API failure"}}));
    assert_eq!(acc.finish()["error"]["code"], "provider.api_error");
    assert!(acc.finish().get("toolCalls").is_none());
}
