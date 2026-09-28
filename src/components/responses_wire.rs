//! Pure Responses wire encoding. Provider-owned reasoning is never translated.
use serde_json::{json, Value};

/// Convert conversational messages after neutral materialization. Responses
/// reasoning items are supplied separately from their originating ledger event.
pub fn input_items(messages: &[Value]) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    for message in messages {
        if let Some(native) = message.get("_native_compaction") {
            items.extend(compact_output(native)?);
            continue;
        }
        if let Some(output) = message["_responses_output"].as_array() {
            items.extend(output.iter().cloned());
            continue;
        }
        match message["role"].as_str() {
            Some("tool") => items.push(json!({
                "type": "function_call_output", "call_id": message["tool_call_id"],
                "output": message["content"],
            })),
            Some(role @ ("user" | "assistant" | "system" | "developer")) => {
                if let Some(parts) = message["_reasoning"].as_array() {
                    for part in parts {
                        if let Some(original) = part["opaque"].get("responses") {
                            items.push(original.clone());
                        } else if let Some(text) = part["text"].as_str() {
                            items.push(json!({"type":"reasoning", "summary":[{"type":"summary_text","text":text}]}));
                        }
                    }
                }
                let mut content = Vec::new();
                if let Some(text) = message["content"].as_str() {
                    if !text.is_empty() {
                        content.push(json!({"type": if role == "assistant" {"output_text"} else {"input_text"}, "text": text}));
                    }
                } else if let Some(parts) = message["content"].as_array() {
                    for part in parts {
                        match part["type"].as_str() {
                            Some("text") => content.push(json!({"type": if role == "assistant" {"output_text"} else {"input_text"}, "text": part["text"]})),
                            Some("image_url") => content.push(json!({"type": "input_image", "image_url": part["image_url"]["url"]})),
                            _ => return Err("unsupported Responses content part".into()),
                        }
                    }
                }
                if !content.is_empty() {
                    items.push(json!({"type": "message", "role": role, "content": content}));
                }
                if let Some(calls) = message["tool_calls"].as_array() {
                    for call in calls {
                        items.push(json!({"type": "function_call", "call_id": call["id"],
                            "name": call["function"]["name"], "arguments": call["function"]["arguments"]}));
                    }
                }
            }
            _ => return Err("unsupported Responses message role".into()),
        }
    }
    Ok(items)
}

pub fn request(model: &str, input: Vec<Value>, tools: &[Value], max_tokens: u64) -> Value {
    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function", "name": tool["name"], "description": tool["description"],
                "parameters": tool["parameters"], "strict": false,
            })
        })
        .collect();
    json!({"model": model, "input": input, "tools": tools,
        "max_output_tokens": max_tokens, "stream": true, "store": false,
        "include": ["reasoning.encrypted_content"]})
}

/// Hosted tools are provider work, never function calls for the local loop.
pub fn enable_web_search(body: &mut Value) {
    body["tools"]
        .as_array_mut()
        .expect("request tools array")
        .push(json!({"type": "web_search"}));
    body["include"]
        .as_array_mut()
        .expect("request include array")
        .push(json!("web_search_call.action.sources"));
}

/// Native compaction is a separate JSON endpoint, not a summarization turn.
pub fn compact_request(model: &str, input: Vec<Value>, instructions: Option<&str>) -> Value {
    let mut body = json!({"model": model, "input": input});
    if let Some(instructions) = instructions {
        body["instructions"] = json!(instructions);
    }
    body
}

/// Preserve the entire replacement input, not just its encrypted item: the
/// endpoint may also retain ordinary messages. No plaintext fallback is invented.
pub fn compact_output(response: &Value) -> Result<Vec<Value>, String> {
    let output = response["output"]
        .as_array()
        .ok_or("missing compact output")?;
    if !output.iter().any(|item| {
        item["type"] == "compaction"
            && item["encrypted_content"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
    }) {
        return Err("compact output has no sealed compaction item".into());
    }
    Ok(output.clone())
}

/// Trigger compaction must produce exactly one sealed state. Other output
/// items are audit data, not executable calls or replacement history.
pub fn trigger_output(output: &[Value]) -> Result<Vec<Value>, String> {
    let states: Vec<Value> = output
        .iter()
        .filter(|item| item["type"] == "compaction")
        .cloned()
        .collect();
    if states.len() != 1 {
        return Err("trigger compaction must return exactly one compaction item".into());
    }
    compact_output(&json!({"output":states}))
}

/// Only terminal provider output becomes a ledger result. Deltas are display
/// notifications, never a second source for tool argument assembly.
pub fn completed(response: &Value) -> Result<Value, String> {
    if !matches!(
        response["status"].as_str(),
        Some("completed" | "incomplete")
    ) {
        return Err("Responses output has no successful terminal status".into());
    }
    let output = response["output"]
        .as_array()
        .ok_or("Responses output is not an array")?;
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut reasoning = Vec::new();
    let mut citations = Vec::new();
    for item in output {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"]
                    .as_array()
                    .ok_or("message content is not an array")?
                {
                    match part["type"].as_str() {
                        Some("output_text") => {
                            text.push_str(part["text"].as_str().ok_or("output text is missing")?);
                            for annotation in part["annotations"].as_array().into_iter().flatten() {
                                if annotation["type"] == "url_citation" {
                                    if let Some(url) = annotation["url"].as_str() {
                                        if (url.starts_with("https://")
                                            || url.starts_with("http://"))
                                            && !url.contains(['\n', '\r', '<', '>'])
                                            && !citations.iter().any(|seen| seen == url)
                                        {
                                            citations.push(url.to_owned());
                                        }
                                    }
                                }
                            }
                        }
                        Some("refusal") => text
                            .push_str(part["refusal"].as_str().ok_or("refusal text is missing")?),
                        _ => return Err("unsupported Responses output content".into()),
                    }
                }
            }
            Some("function_call") => {
                if item
                    .get("status")
                    .is_some_and(|status| status != "completed")
                    || (response["status"] == "incomplete" && item["status"] != "completed")
                {
                    return Err("tool call is not complete".into());
                }
                // An interrupted argument string must never execute as an empty object.
                let arguments: Value = serde_json::from_str(
                    item["arguments"].as_str().ok_or("missing tool arguments")?,
                )
                .map_err(|_| "invalid tool arguments JSON")?;
                if !arguments.is_object() {
                    return Err("tool arguments must be an object".into());
                }
                calls.push(json!({"id": item["call_id"].as_str().ok_or("missing call_id")?,
                    "tool": item["name"].as_str().ok_or("missing function name")?, "arguments": arguments}));
            }
            // Executed remotely; preserve its action and sources in the raw
            // output, but never ask the local tool loop to execute it again.
            Some("web_search_call") => {}
            Some("image_generation_call") => {
                if item.get("result").is_some() || item["image"]["file"].as_str().is_none() {
                    return Err("generated image must be stored before completing the call".into());
                }
            }
            Some("reasoning") => {
                let summary = item["summary"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let mut part = if summary.is_empty() {
                    json!({"kind":"hidden"})
                } else {
                    json!({"kind":"text", "text":summary})
                };
                part["opaque"] = json!({"responses": item});
                reasoning.push(part);
            }
            _ => return Err("unsupported Responses output item".into()),
        }
    }
    for item in output
        .iter()
        .filter(|item| item["type"] == "image_generation_call")
    {
        let path = item["savedPath"]
            .as_str()
            .ok_or("generated image path is missing")?;
        text.push_str(&format!("\n\nImage saved: {path}"));
    }
    // Frontends consume text, not provider annotations. Expose the cited URLs
    // there as well; the original offsets and titles remain in responsesOutput.
    for url in citations {
        text.push_str(&format!("\n\n<{url}>"));
    }
    let mut result = json!({"status":"ok", "text":text,
        "stopReason": if response["status"] == "incomplete" {"length"} else if calls.is_empty() {"stop"} else {"tool_calls"},
        "toolCalls":calls, "reasoning":reasoning, "responsesOutput":output});
    if let Some(usage) = response.get("usage").filter(|v| !v.is_null()) {
        result["usage"] = usage.clone();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_search_preserves_sources_without_local_execution() {
        let mut body = request("model", vec![], &[], 100);
        assert_eq!(body["tools"], json!([]));
        enable_web_search(&mut body);
        assert_eq!(body["tools"], json!([{"type":"web_search"}]));
        assert!(body["include"]
            .as_array()
            .unwrap()
            .contains(&json!("web_search_call.action.sources")));
        let output = json!([
            {"type":"web_search_call","id":"ws_1","status":"completed",
             "action":{"type":"search","query":"weather","sources":[{"type":"url","url":"https://example.com"}]}},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Weather report",
             "annotations":[{"type":"url_citation","start_index":0,"end_index":14,"url":"https://example.com","title":"Weather"}]}]}
        ]);
        let result = completed(&json!({"status":"completed","output":output})).unwrap();
        assert_eq!(result["toolCalls"], json!([]));
        assert_eq!(result["stopReason"], "stop");
        assert_eq!(result["text"], "Weather report\n\n<https://example.com>");
        assert_eq!(result["responsesOutput"], output);
        assert_eq!(
            input_items(&[json!({"_responses_output":output})]).unwrap(),
            output.as_array().unwrap().clone()
        );
    }

    #[test]
    fn trigger_requires_one_sealed_state_not_an_ordinary_turn() {
        let sealed = json!({"type":"compaction","encrypted_content":"sealed"});
        assert_eq!(
            trigger_output(std::slice::from_ref(&sealed)).unwrap(),
            vec![sealed.clone()]
        );
        assert!(trigger_output(&[]).is_err());
        assert!(trigger_output(&[sealed.clone(), sealed]).is_err());
        assert!(trigger_output(&[json!({"type":"function_call"})]).is_err());
        let state = json!({"type":"compaction","encrypted_content":"sealed"});
        assert_eq!(
            trigger_output(&[
                json!({"type":"message"}),
                state.clone(),
                json!({"type":"function_call"})
            ])
            .unwrap(),
            vec![state]
        );
        assert!(trigger_output(&[json!({"type":"compaction","encrypted_content":""})]).is_err());
    }

    #[test]
    fn compact_retains_full_replacement_without_chat_fields() {
        let output = json!([
            {"type":"message","role":"user","content":[{"type":"input_text","text":"original request"}]},
            {"type":"compaction","id":"cmp_1","encrypted_content":"sealed-state"}
        ]);
        assert_eq!(
            json!(compact_output(&json!({"output":output})).unwrap()),
            output
        );
        let body = compact_request("model", vec![], Some("rules"));
        assert_eq!(body["instructions"], "rules");
        assert!(body.get("stream").is_none());
        assert!(compact_output(&json!({"output":[{"type":"message"}]})).is_err());
    }

    #[test]
    fn reasoning_round_trip_keeps_original_provider_item_before_call() {
        let sealed = json!({"type":"reasoning", "id":"rs_original", "summary":[], "encrypted_content":"opaque-original"});
        let decoded = completed(&json!({"status":"completed","output":[sealed.clone(),
            {"type":"function_call","call_id":"call_1","name":"Read","arguments":"{}"}]}))
        .unwrap();
        let encoded = input_items(
            &[json!({"role":"assistant","_reasoning":decoded["reasoning"],
            "tool_calls":[{"id":"call_1","function":{"name":"Read","arguments":"{}"}}]})],
        )
        .unwrap();
        assert_eq!(encoded[0], sealed);
        assert_eq!(encoded[1]["type"], "function_call");
    }

    #[test]
    fn completed_keeps_sealed_reasoning_and_call_identity() {
        let sealed =
            json!({"type":"reasoning", "id":"rs_1", "summary":[], "encrypted_content":"sealed"});
        let result = completed(&json!({"status":"completed", "output":[sealed.clone(),
            {"type":"function_call","id":"fc_1","call_id":"call_1","name":"Read","arguments":"{\"path\":\"x\"}"}],
            "usage":{"input_tokens":20,"output_tokens":3}})).unwrap();
        assert_eq!(result["reasoning"][0]["opaque"]["responses"], sealed);
        assert_eq!(result["toolCalls"][0]["id"], "call_1");
        assert_eq!(result["usage"]["input_tokens"], 20);
    }

    #[test]
    fn truncated_arguments_never_become_an_executable_call() {
        let result = completed(&json!({"status":"incomplete", "output":[
            {"type":"function_call","call_id":"call_1","name":"Run","arguments":"{"}]}));
        assert!(result.is_err());
    }

    #[test]
    fn unfinished_calls_with_valid_json_are_not_executable() {
        for status in ["completed", "incomplete"] {
            assert!(completed(&json!({"status":status,"output":[{
                "type":"function_call","status":"in_progress", "call_id":"c",
                "name":"Run","arguments":"{}"
            }]}))
            .is_err());
        }
    }

    #[test]
    fn native_output_round_trip_preserves_order_ids_and_phase() {
        let output = json!([
            {"type":"message","id":"m","role":"assistant","phase":"commentary",
             "content":[{"type":"output_text","text":"checking"}]},
            {"type":"reasoning","id":"r1","summary":[],"encrypted_content":"one"},
            {"type":"function_call","id":"f1","call_id":"c1","name":"Read","arguments":"{}"},
            {"type":"reasoning","id":"r2","summary":[],"encrypted_content":"two"},
            {"type":"function_call","id":"f2","call_id":"c2","name":"Read","arguments":"{}"}
        ]);
        let decoded = completed(&json!({"status":"completed","output":output})).unwrap();
        let encoded = input_items(&[
            json!({"role":"assistant", "_responses_output":decoded["responsesOutput"]}),
        ])
        .unwrap();
        assert_eq!(json!(encoded), output);
    }

    #[test]
    fn nonterminal_response_is_not_a_completed_turn() {
        assert!(completed(&json!({"status":"in_progress","output":[]})).is_err());
    }

    #[test]
    fn calls_use_call_id_not_response_item_id() {
        let input = input_items(&[
            json!({"role":"assistant","tool_calls":[{"id":"call_1","function":{"name":"Read","arguments":"{\"path\":\"x\"}"}}]}),
            json!({"role":"tool","tool_call_id":"call_1","content":"file text"}),
        ]).unwrap();
        assert_eq!(input[0]["call_id"], "call_1");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[1]["type"], "function_call_output");
        assert!(input[0].get("id").is_none());
    }

    #[test]
    fn images_and_assistant_text_have_responses_content_types() {
        let input = input_items(&[
            json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,eA=="}}]}),
            json!({"role":"assistant","content":"answer"}),
        ]).unwrap();
        assert_eq!(input[0]["content"][0]["type"], "input_image");
        assert_eq!(input[1]["content"][0]["type"], "output_text");
    }

    #[test]
    fn tool_effects_are_not_provider_fields() {
        let body = request(
            "model",
            vec![],
            &[json!({"name":"Read","parameters":{"type":"object"},"effects":{"reads":["*"]}})],
            100,
        );
        assert!(body["tools"][0].get("effects").is_none());
        assert_eq!(body["store"], false);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
    }
}
