use super::*;
use lattice::components::{anthropic_model, openai_model, responses_model};

fn materialize(wire: &str, b: &Building) -> Result<Vec<Value>, String> {
    let reader = b.log.reader();
    match wire {
        "anthropic" => anthropic_model::materialize(&b.parts, &reader, None),
        "openai" => openai_model::materialize(&b.parts, &reader, None),
        "responses" => responses_model::materialize(
            &b.parts,
            &reader,
            None,
            "fixture",
            "http://example.invalid",
        ),
        _ => unreachable!(),
    }
}

fn slots(wire: &str, messages: &[Value]) -> Vec<Slot> {
    match wire {
        "anthropic" => anthropic_slots(messages),
        "openai" => openai_slots(messages),
        "responses" => messages
            .iter()
            .filter_map(|item| match item["type"].as_str() {
                Some("function_call") => Some(Slot::Call(item["call_id"].as_str().unwrap().into())),
                Some("function_call_output") => {
                    Some(Slot::Answer(item["call_id"].as_str().unwrap().into()))
                }
                _ => None,
            })
            .collect(),
        _ => unreachable!(),
    }
}

fn add_non_tool_interruptions(b: &mut Building) {
    // A stream-level stop has no request to settle.
    b.material(
        EventDraft::new(ce::INTERRUPTED, &[], json!({"by":"user"})),
        "ui",
    );
    let started = b.put(
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({
                "model":"fixture", "input":{"parts":[],"fingerprint":"synthetic"}
            }),
        ),
        "loop",
    );
    // A model request is a real request, but never a tool request.
    b.interrupted(&started);
}

fn check_non_tool_interruptions(wire: &str) {
    for with_tool in [false, true] {
        for late_completion in [false, true] {
            let mut b = Building::new();
            b.user("keep this question");
            if with_tool {
                let asked = b.reply(None, &["real_call".into()]);
                let started = b.started(&asked, "real_call");
                b.interrupted(&started);
                if late_completion {
                    b.completed(&started, "real_call");
                }
            }
            let baseline = materialize(wire, &b).unwrap();
            let expected_slots = if with_tool {
                vec![
                    Slot::Call("real_call".into()),
                    Slot::Answer("real_call".into()),
                ]
            } else {
                vec![]
            };
            assert_eq!(
                slots(wire, &baseline),
                expected_slots,
                "{wire}: retain the genuine tool outcome"
            );
            if with_tool {
                let text = serde_json::to_string(&baseline).unwrap();
                assert_eq!(
                    text.contains("[interrupted:"),
                    !late_completion,
                    "{wire}: the tool's own completion must supersede the interruption"
                );
            }
            add_non_tool_interruptions(&mut b);
            let before = b.log.replay(1).unwrap();
            let parts = b.parts.clone();
            assert_eq!(
                materialize(wire, &b).unwrap(),
                baseline,
                "{wire}: non-tool interruptions must not fabricate tool replies"
            );
            assert_eq!(b.parts, parts);
            assert_eq!(
                serde_json::to_value(b.log.replay(1).unwrap()).unwrap(),
                serde_json::to_value(before).unwrap()
            );
        }
    }
    let mut b = Building::new();
    add_non_tool_interruptions(&mut b);
    assert!(
        materialize(wire, &b).is_err(),
        "{wire}: control-only material must not fabricate a conversation"
    );
}

#[test]
fn anthropic_non_tool_interruptions_are_not_tool_replies() {
    check_non_tool_interruptions("anthropic");
}

#[test]
fn openai_non_tool_interruptions_are_not_tool_replies() {
    check_non_tool_interruptions("openai");
}

#[test]
fn responses_non_tool_interruptions_are_not_tool_replies() {
    check_non_tool_interruptions("responses");
}
