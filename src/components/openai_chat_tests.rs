use super::*;
use crate::contracts::core_events::core_event_decls;
use crate::kernel::log::EventLog;

#[test]
fn configured_disable_and_unset_controls_have_distinct_wire_shapes() {
    let rungs = vec!["none".into(), "low".into(), "high".into()];
    for host in ["openrouter.ai", "api.together.ai", "api.groq.com"] {
        let base = format!("https://{host}/v1");
        let mut unset = build_request("synthetic", 100, None, &rungs, None, vec![], None);
        assert!(extensions::configure_thinking(&mut unset, &base, None, &rungs).is_none());
        for field in ["thinking", "reasoning", "reasoning_effort"] {
            assert!(unset.get(field).is_none());
        }
        let mut off = build_request(
            "synthetic",
            100,
            Some(&Thinking::Off),
            &rungs,
            None,
            vec![],
            None,
        );
        assert!(
            extensions::configure_thinking(&mut off, &base, Some(&Thinking::Off), &rungs).is_none()
        );
        assert!(off.get("thinking").is_none());
        if host == "api.groq.com" {
            assert_eq!(off["reasoning_effort"], "none");
        } else {
            assert_eq!(off["reasoning"], json!({"enabled":false}));
            assert!(off.get("reasoning_effort").is_none());
        }
    }
    for value in [
        json!({}),
        json!({"reasoning":"","reasoning_details":[],"content":null}),
    ] {
        assert!(!extensions::has_content(&json!({"chatMessage":value})));
    }
}

#[test]
fn distinct_signatures_keep_adjacent_same_index_blocks_separate() {
    let mut acc = Accumulator::default();
    for (text, signature) in [("first", "SYNTHETIC_S1"), ("second", "SYNTHETIC_S2")] {
        acc.apply(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0,"text":text,"signature":signature}]}}]}));
    }
    acc.apply(&json!({"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}));
    let messages = round_trip(acc.finish(), false);
    assert_eq!(
        messages[0]["reasoning_details"],
        json!([
            {"type":"reasoning.text","index":0,"text":"first","signature":"SYNTHETIC_S1"},
            {"type":"reasoning.text","index":0,"text":"second","signature":"SYNTHETIC_S2"},
        ])
    );
}

#[test]
fn extension_only_assistant_turns_are_not_dropped_before_restoration() {
    for delta in [
        json!({"reasoning_details":[{"type":"reasoning.encrypted","data":"SYNTHETIC_SEALED"}]}),
        json!({"content":[{"type":"thinking","thinking":[{"type":"text","text":"synthetic thought"}]}]}),
    ] {
        let mut acc = Accumulator::default();
        acc.apply(&json!({"choices":[{"delta":delta,"finish_reason":"stop"}]}));
        let payload = acc.finish();
        let messages = round_trip(payload.clone(), false);
        assert_eq!(messages.len(), 1);
        for (key, value) in payload["chatMessage"].as_object().unwrap() {
            assert_eq!(&messages[0][key], value);
        }
    }
}

#[test]
fn mistral_mixed_content_and_minimax_inline_thinking_survive_the_tool_round_trip() {
    let mut acc = Accumulator::default();
    for content in [
        json!([{"type":"thinking","thinking":[{"type":"text","text":"Check "}]}]),
        json!([{"type":"thinking","thinking":[{"type":"text","text":"units."}]},{"type":"text","text":"Result: "}]),
        json!("7."),
    ] {
        acc.apply(&json!({"choices":[{"delta":{"content":content}}]}));
    }
    acc.apply(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}));
    let payload = acc.finish();
    assert_eq!(payload["text"], "Result: 7.");
    assert_eq!(ce::reasoning_text(&payload), "Check units.");
    let messages = round_trip(payload, false);
    assert_eq!(
        messages[0]["content"],
        json!([
            {"type":"thinking","thinking":[{"type":"text","text":"Check "}]},
            {"type":"thinking","thinking":[{"type":"text","text":"units."}]},
            {"type":"text","text":"Result: 7."}
        ])
    );
    assert!(messages[0].get("reasoning_content").is_none());
    let mut inline = Accumulator::default();
    inline.apply(&json!({"choices":[{"delta":{"content":"<think>private thought</think>answer","tool_calls":[{"index":0,"id":"inline","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}));
    let payload = inline.finish();
    assert_eq!(payload["text"], "answer");
    let messages = round_trip(payload, false);
    assert_eq!(
        messages[0]["content"],
        "<think>private thought</think>answer"
    );
    assert!(messages[0].get("reasoning_content").is_none());
}

#[test]
fn reasoning_type_transitions_and_encrypted_blocks_are_not_merged_by_reused_index() {
    let mut message = extensions::Message::default();
    for details in [
        json!([{"type":"reasoning.text","index":0,"text":"a","signature":null}]),
        json!([{"type":"reasoning.text","index":0,"signature":"sig"}]),
        json!([{"type":"reasoning.text","index":0,"signature":"sig"}]),
        json!([{"type":"reasoning.encrypted","index":0,"data":"first"}]),
        json!([{"type":"reasoning.encrypted","index":0,"data":"second"}]),
        json!([{"type":"reasoning.text","index":0,"text":"b"}]),
    ] {
        message.apply(&json!({"reasoning_details":details}));
    }
    assert_eq!(
        message.value()["reasoning_details"],
        json!([
            {"type":"reasoning.text","index":0,"text":"a","signature":"sig"},
            {"type":"reasoning.encrypted","index":0,"data":"first"},
            {"type":"reasoning.encrypted","index":0,"data":"second"},
            {"type":"reasoning.text","index":0,"text":"b"}
        ])
    );
    let mut signature = json!({"google":{"thought_signature":"sig"}});
    extensions::merge(
        &mut signature,
        &json!({"google":{"thought_signature":"sig"}}),
    );
    assert_eq!(signature["google"]["thought_signature"], "sig");
}

#[test]
fn known_services_do_not_receive_deepseek_controls_or_fake_disable_claims() {
    for host in [
        "generativelanguage.googleapis.com",
        "api.groq.com",
        "api.mistral.ai",
        "api.moonshot.cn",
        "api.minimax.cn",
        "dashscope.aliyuncs.com",
    ] {
        let mut body = build_request(
            "synthetic",
            100,
            Some(&Thinking::Off),
            &[],
            None,
            vec![],
            None,
        );
        assert!(extensions::configure_thinking(
            &mut body,
            &format!("https://{host}/v1"),
            Some(&Thinking::Off),
            &[]
        )
        .is_some());
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }
    let rungs = vec!["low".into(), "high".into()];
    let setting = Thinking::Raw("high".into());
    for (base, nested) in [
        ("https://openrouter.ai/api/v1", true),
        ("https://api.groq.com/openai/v1", false),
    ] {
        let mut body = build_request("synthetic", 100, Some(&setting), &rungs, None, vec![], None);
        assert!(extensions::configure_thinking(&mut body, base, Some(&setting), &rungs).is_none());
        assert!(body.get("thinking").is_none());
        if nested {
            assert_eq!(body["reasoning"]["effort"], "high");
            assert!(body.get("reasoning_effort").is_none());
        } else {
            assert_eq!(body["reasoning_effort"], "high");
        }
    }
    let mut legacy = build_request(
        "synthetic",
        100,
        Some(&Thinking::Off),
        &[],
        None,
        vec![],
        None,
    );
    assert!(extensions::configure_thinking(
        &mut legacy,
        "https://api.deepseek.com",
        Some(&Thinking::Off),
        &[]
    )
    .is_none());
    assert_eq!(legacy["thinking"]["type"], "disabled");
}

fn round_trip(payload: Value, preserve_reasoning: bool) -> Vec<Value> {
    let mut log = EventLog::in_memory(core_event_decls(), "provider-fixture");
    let reply = log
        .append(
            EventDraft::new(ce::MODEL_CALL_COMPLETED, &[], payload.clone()),
            "model",
        )
        .unwrap();
    let mut parts = vec![json!({"event":reply.id})];
    for call in payload["toolCalls"].as_array().into_iter().flatten().rev() {
        let result = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&reply.id],
                    json!({"call":call["id"],"status":"ok","result":{"synthetic":true}}),
                ),
                "tools",
            )
            .unwrap();
        parts.push(json!({"event":result.id}));
    }
    materialize_with_reasoning(&parts, &log.reader(), None, preserve_reasoning).unwrap()
}

#[test]
fn gemini_signature_stays_on_its_call_across_stream_ledger_and_next_request() {
    let mut acc = Accumulator::default();
    for delta in [
        json!({"tool_calls":[{"index":1,"id":"second","function":{"name":"lookup","arguments":"{\"b\":"}},{"index":0,"id":"first","function":{"name":"lookup","arguments":"{ \"a\" : "}}]}),
        json!({"tool_calls":[{"index":0,"extra_content":{"google":{"thought_signature":"SYNTHETIC_SIGNATURE"}}}]}),
        json!({"tool_calls":[{"index":1,"function":{"arguments":"2}"}},{"index":0,"function":{"arguments":"1 }"}}]}),
    ] {
        acc.apply(&json!({"choices":[{"delta":delta}]}));
    }
    acc.apply(&json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}));
    let payload = acc.finish();
    assert_eq!(payload["status"], "ok");
    let messages = round_trip(payload.clone(), false);
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[0]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "SYNTHETIC_SIGNATURE"
    );
    assert_eq!(
        messages[0]["tool_calls"][0]["function"]["arguments"],
        "{ \"a\" : 1 }"
    );
    assert!(messages[0]["tool_calls"][1].get("extra_content").is_none());
    assert_eq!(messages[1]["tool_call_id"], "first");
    assert_eq!(messages[2]["tool_call_id"], "second");
    let other = round_trip(payload, true);
    assert!(other[0]["tool_calls"][0].get("extra_content").is_none());
}

#[test]
fn openrouter_indexed_reasoning_details_and_together_field_names_survive() {
    let mut acc = Accumulator::default();
    for delta in [
        json!({"reasoning":"first ","reasoning_details":[{"index":0,"type":"reasoning.text","id":"r0","format":"synthetic","text":"first "}]}),
        json!({"reasoning":"second","reasoning_details":[{"index":0,"type":"reasoning.text","id":"r0","format":"synthetic","text":"second"},{"index":1,"type":"reasoning.encrypted","data":"SYNTHETIC_ENCRYPTED"}]}),
        json!({"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{}"}}]}),
    ] {
        acc.apply(&json!({"choices":[{"delta":delta}]}));
    }
    acc.apply(&json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}));
    let messages = round_trip(acc.finish(), false);
    assert_eq!(messages[0]["reasoning"], "first second");
    assert!(messages[0].get("reasoning_content").is_none());
    assert_eq!(
        messages[0]["reasoning_details"],
        json!([
            {"index":0,"type":"reasoning.text","id":"r0","format":"synthetic","text":"first second"},
            {"index":1,"type":"reasoning.encrypted","data":"SYNTHETIC_ENCRYPTED"}
        ])
    );
}

#[test]
fn original_reasoning_field_is_preserved_on_non_tool_turns_too() {
    for key in ["reasoning", "reasoning_content"] {
        let mut delta = json!({"content":"synthetic answer"});
        delta[key] = json!("synthetic reasoning");
        let mut acc = Accumulator::default();
        acc.apply(&json!({"choices":[{"delta":delta,"finish_reason":"stop"}]}));
        let messages = round_trip(acc.finish(), false);
        assert_eq!(messages[0][key], "synthetic reasoning");
    }
}

#[test]
fn malformed_or_unfinished_tools_never_turn_into_executable_empty_arguments() {
    for (arguments, reason) in [
        ("{", Some("tool_calls")),
        ("[]", Some("tool_calls")),
        ("{}", None),
        ("{}", Some("content_filter")),
    ] {
        let mut acc = Accumulator::default();
        acc.apply(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":arguments}}]},"finish_reason":reason}]}));
        let payload = acc.finish();
        assert_eq!(payload["status"], "error", "{arguments:?} / {reason:?}");
        assert!(payload.get("toolCalls").is_none());
    }
}
