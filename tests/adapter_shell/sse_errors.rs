//! Malformed wire bytes must fail before a plausible tool call can escape.
//! These cases use real adapters, not a replacement parser or a fake accumulator.
use super::*;
use lattice::components::responses_model;

fn candidate(name: &str) -> Candidate {
    transport_candidates()
        .into_iter()
        .find(|candidate| candidate.manifest.name == name)
        .unwrap()
}

fn run_wire(name: &str, body: Vec<u8>) -> Value {
    ensure_key();
    let (url, hits, _) = fake_api(vec![Canned::SseBytes(body)]);
    let payload = run_call(candidate(name), &url, None);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "never retry a received body"
    );
    payload
}

fn invalid(payload: &Value, reason: &str) {
    assert_eq!(payload["status"], "error", "{payload}");
    assert_eq!(
        payload["error"]["code"], "provider.invalid_stream",
        "{payload}"
    );
    assert_eq!(payload["error"]["blame"], "provider", "{payload}");
    assert_eq!(payload["error"]["retryable"], false, "{payload}");
    assert_eq!(payload["error"]["transient"], false, "{payload}");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains(reason),
        "{payload}"
    );
    assert!(payload.get("toolCalls").is_none(), "{payload}");
}

fn messages_tool(middle: &str) -> Vec<u8> {
    format!(concat!(
        "data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":3}}}}}}\n\n",
        "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n",
        "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"kept\"}}}}\n\n",
        "data: {{\"type\":\"content_block_start\",\"index\":1,\"content_block\":{{\"type\":\"thinking\",\"thinking\":\"thought\"}}}}\n\n",
        "data: {{\"type\":\"content_block_start\",\"index\":2,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"inspect\",\"input\":{{}}}}}}\n\n",
        "{}",
        "data: {{\"type\":\"content_block_stop\",\"index\":2}}\n\n",
        "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}}}}\n\n",
        "data: {{\"type\":\"message_stop\"}}\n\n"
    ), middle).into_bytes()
}

fn chat_tool(middle: &str) -> Vec<u8> {
    format!(concat!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"kept\",\"reasoning_content\":\"thought\",\"tool_calls\":[{{\"index\":0,\"id\":\"t1\",\"type\":\"function\",\"function\":{{\"name\":\"inspect\",\"arguments\":\"{{}}\"}}}}]}},\"finish_reason\":null}}],\"usage\":{{\"prompt_tokens\":3}}}}\n\n",
        "{}",
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n",
        "data: [DONE]\n\n"
    ), middle).into_bytes()
}

#[test]
fn messages_bad_outer_json_cannot_turn_a_damaged_tool_into_empty_arguments() {
    let valid = run_wire(anthropic_model::NAME, messages_tool(""));
    assert_eq!(valid["toolCalls"][0]["arguments"], json!({}), "{valid}");
    let payload = run_wire(
        anthropic_model::NAME,
        messages_tool("data: {broken-private-data\n\n"),
    );
    invalid(&payload, "JSON");
    assert!(!payload["error"].to_string().contains("broken-private-data"));
    assert_eq!(payload["text"], "kept");
    assert_eq!(payload["usage"]["input_tokens"], 3);
    assert_eq!(payload["reasoning"][0]["text"], "thought");
}

#[test]
fn chat_bad_outer_json_revokes_tools_but_keeps_text_reasoning_and_usage() {
    let valid = run_wire(openai_model::NAME, chat_tool(""));
    assert_eq!(valid["toolCalls"][0]["arguments"], json!({}), "{valid}");
    let payload = run_wire(openai_model::NAME, chat_tool("data: {broken\n\n"));
    invalid(&payload, "JSON");
    assert_eq!(payload["text"], "kept");
    assert_eq!(payload["usage"]["prompt_tokens"], 3);
    assert_eq!(payload["reasoning"][0]["text"], "thought");
}

const RESPONSE_TOOL: &str = "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"id\":\"item1\",\"call_id\":\"t1\",\"name\":\"inspect\",\"arguments\":\"{}\"}]}}\n\n";

#[test]
fn responses_bad_json_before_a_terminal_tool_is_an_error_not_success() {
    let payload = run_wire(
        responses_model::NAME,
        format!("data: broken\n\n{RESPONSE_TOOL}").into_bytes(),
    );
    invalid(&payload, "JSON");
}

#[test]
fn responses_terminal_still_wins_over_later_bytes() {
    let payload = run_wire(
        responses_model::NAME,
        format!("{RESPONSE_TOOL}data: broken\n\ndata: {{").into_bytes(),
    );
    assert_eq!(payload["status"], "ok", "{payload}");
    assert_eq!(payload["toolCalls"][0]["arguments"], json!({}), "{payload}");
}

#[test]
fn invalid_utf8_is_not_replaced_even_in_an_unknown_event() {
    for name in [
        anthropic_model::NAME,
        openai_model::NAME,
        responses_model::NAME,
    ] {
        let mut body = b"data: {\"type\":\"future-event\",\"text\":\"\xff\"}\n\n".to_vec();
        body.extend_from_slice(happy_sse(name).as_bytes());
        invalid(&run_wire(name, body), "UTF-8");
    }
}

#[test]
fn clean_http_eof_with_an_unfinished_line_is_explicit() {
    for name in [
        anthropic_model::NAME,
        openai_model::NAME,
        responses_model::NAME,
    ] {
        for tail in ["data: {", "data: {}", "data: [DONE]"] {
            invalid(&run_wire(name, tail.as_bytes().to_vec()), "unfinished");
        }
    }
}

#[test]
fn messages_and_chat_still_read_past_provider_stop_markers() {
    for (name, body) in [
        (anthropic_model::NAME, messages_tool("")),
        (openai_model::NAME, chat_tool("")),
    ] {
        for (suffix, reason) in [("data: bad\n\n", "JSON"), ("data: {", "unfinished")] {
            let mut body = body.clone();
            body.extend_from_slice(suffix.as_bytes());
            invalid(&run_wire(name, body), reason);
        }
    }
}

#[test]
fn valid_unknown_events_metadata_done_and_chat_usage_keep_working() {
    for name in [
        anthropic_model::NAME,
        openai_model::NAME,
        responses_model::NAME,
    ] {
        let body = format!(": heartbeat\r\nevent: future\r\nid: 1\r\nretry: 1000\r\ndata: {{\"type\":\"future-event\"}}\r\n\r\ndata: [DONE]\n\n{}", happy_sse(name));
        let payload = run_wire(name, body.into_bytes());
        assert_eq!(payload["status"], "ok", "{payload}");
        assert_eq!(payload["text"], "ok", "{payload}");
        if name == openai_model::NAME {
            assert_eq!(payload["usage"]["prompt_tokens"], 3);
            assert_eq!(payload["usage"]["completion_tokens"], 1);
        }
    }
    let payload = run_wire(responses_model::NAME, b"data: [DONE]\n\n".to_vec());
    assert_eq!(
        payload["error"]["code"], "transport.incomplete",
        "{payload}"
    );
}

#[test]
fn trigger_compaction_uses_the_same_error_and_terminal_boundaries() {
    ensure_key();
    let terminal = "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"compaction\",\"encrypted_content\":\"sealed\"}]}}\n\n";
    for (body, reason) in [
        (format!("data: bad\n\n{terminal}"), Some("JSON")),
        ("data: {".to_string(), Some("unfinished")),
        (format!("{terminal}data: bad\n\n"), None),
    ] {
        let candidate = Candidate {
            manifest: responses_model::manifest(),
            factory: Box::new(|config| {
                let mut config = config.unwrap().clone();
                config["compactionProtocol"] = json!("trigger");
                Box::new(responses_model::ResponsesModel::from_config(Some(&config)))
            }),
        };
        let (url, hits, _) = fake_api(vec![Canned::SseBytes(body.into_bytes())]);
        let payload = run_call_with_purpose(
            candidate,
            &url,
            None,
            Some("RULES"),
            Some("context.compact.responses"),
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        if let Some(reason) = reason {
            invalid(&payload, reason);
            assert!(payload.get("nativeCompaction").is_none(), "{payload}");
        } else {
            assert_eq!(payload["status"], "ok", "{payload}");
            assert_eq!(
                payload["nativeCompaction"]["output"][0]["encrypted_content"],
                "sealed"
            );
        }
    }
}

#[test]
fn cancellation_after_sending_half_a_line_is_not_misreported_as_eof() {
    ensure_key();
    for candidate in transport_candidates() {
        let (tx, rx) = mpsc::channel();
        let (url, hits, _) = fake_api(vec![Canned::PartialSse(tx)]);
        let payload = run_call(candidate, &url, Some(rx));
        assert_eq!(payload["status"], "cancelled", "{payload}");
        assert!(payload.get("toolCalls").is_none(), "{payload}");
        assert!(payload.get("error").is_none(), "{payload}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
