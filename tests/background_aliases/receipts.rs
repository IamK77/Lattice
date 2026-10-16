use super::*;

fn tool_text(messages: &[Value]) -> Vec<String> {
    let mut text = Vec::new();
    for message in messages {
        if message["role"] == "tool" {
            text.push(message["content"].as_str().unwrap().to_owned());
        }
        if message["type"] == "function_call_output" {
            text.push(message["output"].as_str().unwrap().to_owned());
        }
        if let Some(blocks) = message["content"].as_array() {
            for block in blocks {
                if block["type"] == "tool_result" {
                    text.push(block["content"].as_str().unwrap().to_owned());
                }
            }
        }
    }
    text
}

#[test]
fn full_concise_and_digested_receipts_keep_the_same_reference_and_original_fields() {
    for (tool, kind, result) in [
        ("ask", "Expert", json!({"job":1})),
        (
            "Run",
            "Run",
            json!({"job":"long-call-id","background":true}),
        ),
        ("Schedule", "Timer", json!({"timer":1})),
        ("Watch", "Watch", json!({"watch":1})),
    ] {
        for concise in [false, true] {
            let mut log = EventLog::in_memory(ce::core_event_decls(), "owner-stream");
            let model = append(
                &mut log,
                ce::MODEL_CALL_COMPLETED,
                &[],
                json!({"status":"ok","toolCalls":[{"id":"start","tool":tool,"arguments":{}}]}),
            );
            let request = append(
                &mut log,
                ce::TOOL_EXEC_STARTED,
                &[&model.id],
                json!({"call":"start","tool":tool,"arguments":{}}),
            );
            let mut result = result.clone();
            result["_backgroundReference"] = json!("provider-owned field");
            let mut payload = json!({"call":"start","status":"ok","result":result});
            if concise {
                payload["modelText"] = json!("Subscription accepted.");
            }
            let receipt = append(&mut log, ce::TOOL_EXEC_COMPLETED, &[&request.id], payload);
            let reader = log.reader();
            let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
            let identity = identity_for_event(&reader, &receipt).unwrap().unwrap();
            let reference = format!(
                "{kind} {}; stream=\"owner-stream\"",
                projection.alias(&identity).unwrap()
            );
            for digest in [false, true] {
                let note = format!("Brief start receipt; original: {}", receipt.id);
                let result_part = if digest {
                    json!({"digest":{"of":receipt.id,"text":note}})
                } else {
                    json!({"event":receipt.id})
                };
                // With the assistant present the adapters pull results through
                // their answer helpers; without it they use standalone paths.
                for with_model in [true, false] {
                    let mut parts = Vec::new();
                    if with_model {
                        parts.push(json!({"event":model.id}));
                    }
                    parts.push(result_part.clone());
                    let anthropic = anthropic_model::materialize(&parts, &reader, None).unwrap();
                    let openai = openai_model::materialize(&parts, &reader, None).unwrap();
                    let openai_text = tool_text(&openai);
                    assert_eq!(openai_text.len(), 1);
                    assert_eq!(tool_text(&responses(&parts, &reader, None)), openai_text);
                    if digest || concise {
                        assert!(openai_text[0].contains(&reference), "{tool}, digest={digest}, concise={concise}, with_model={with_model}: {openai_text:?}");
                    }
                    if with_model || !digest {
                        assert_eq!(tool_text(&anthropic), openai_text);
                    } else {
                        let text = texts(&anthropic);
                        assert_eq!(text.len(), 1);
                        assert!(text[0].contains(&reference));
                        assert!(text[0].contains(&note));
                    }
                    if digest {
                        assert!(openai_text[0].contains(&note));
                        assert!(!openai_text[0].contains("provider-owned field"));
                        assert!(!openai_text[0].contains("Subscription accepted."));
                    } else if concise {
                        assert!(openai_text[0].contains("Subscription accepted."));
                        assert!(openai_text[0].contains(&receipt.id));
                        assert!(!openai_text[0].contains("provider-owned field"));
                    } else {
                        // JSON remains JSON; downstream image extraction and
                        // original numeric/string IDs must remain reachable.
                        let mut rendered: Value = serde_json::from_str(&openai_text[0]).unwrap();
                        assert_eq!(rendered["__backgroundReference"], reference);
                        rendered
                            .as_object_mut()
                            .unwrap()
                            .remove("__backgroundReference");
                        assert_eq!(rendered, result);
                    }
                    assert_eq!(
                        reader.get(&receipt.id).unwrap().unwrap().payload,
                        receipt.payload
                    );
                }
            }
        }
    }
}

#[test]
fn digested_wakes_keep_the_alias_without_restoring_the_omitted_body() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "owner-stream");
    let request = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"start","tool":"Schedule","arguments":{"delay_ms":1}}),
    );
    let wake = append(
        &mut log,
        ce::WAKE,
        &[&request.id],
        json!({"source":"timer:1","summary":"fired","body":{"timer":1,"details":"omitted-large-body"}}),
    );
    let reader = log.reader();
    let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let identity = identity_for_event(&reader, &wake).unwrap().unwrap();
    let reference = format!(
        "Timer {}; stream=\"owner-stream\"",
        projection.alias(&identity).unwrap()
    );
    let note = format!("Timer reported; original: {}", wake.id);
    let parts = [json!({"digest":{"of":wake.id,"text":note}})];
    let anthropic = texts(&anthropic_model::materialize(&parts, &reader, None).unwrap());
    let openai = texts(&openai_model::materialize(&parts, &reader, None).unwrap());
    assert_eq!(anthropic, openai);
    assert_eq!(texts(&responses(&parts, &reader, None)), openai);
    assert_eq!(openai.len(), 1);
    assert!(openai[0].contains(&reference));
    assert!(openai[0].contains(&note));
    assert!(!openai[0].contains("omitted-large-body"));
    assert_eq!(reader.get(&wake.id).unwrap().unwrap().payload, wake.payload);
}
