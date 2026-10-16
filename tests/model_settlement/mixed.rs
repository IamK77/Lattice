use super::*;

fn mixed_interrupt(
    kernel: &mut Kernel,
    request: &EventEnvelope,
    other: &EventEnvelope,
) -> EventEnvelope {
    kernel.injector("model").emit(
        "fault",
        EventDraft::new(
            ce::INTERRUPTED,
            &[&request.id, &other.id],
            json!({"by":"mixed-test"}),
        ),
    );
    settle(kernel);
    latest(kernel, ce::INTERRUPTED)
}

#[test]
fn extra_non_request_cause_does_not_readmit_a_stale_model_interruption() {
    let mut kernel = start(false);
    let request = input(&mut kernel, "first");
    let answer = result(&mut kernel, &request, json!({"status":"ok","text":"done"}));
    let stale = mixed_interrupt(&mut kernel, &request, &answer);
    one_turn(&kernel);
    let next = input(&mut kernel, "next");
    assert!(
        !next.payload["input"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["event"] == stale.id),
        "an extra cause must not admit a duplicate model outcome into live material"
    );
    assert!(kernel.log().get(&stale.id).unwrap().is_some());
}

#[test]
fn stale_model_target_does_not_hide_a_live_tool_interruption_or_repeat_its_advance() {
    let mut kernel = start(false);
    let request = input(&mut kernel, "first");
    result(&mut kernel, &request, tool_payload());
    let tool = latest(&kernel, ce::TOOL_EXEC_STARTED);
    let ended = mixed_interrupt(&mut kernel, &request, &tool);
    let next = latest(&kernel, ce::MODEL_CALL_STARTED);
    assert_ne!(
        next.id, request.id,
        "the interrupted tool round must advance"
    );
    assert_eq!(events(&kernel, ce::MODEL_CALL_STARTED).len(), 2);
    assert!(events(&kernel, ce::OUTPUT_REPLY).is_empty());
    assert!(next.payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|part| part["event"] == ended.id));
    mixed_interrupt(&mut kernel, &request, &tool);
    assert_eq!(events(&kernel, ce::MODEL_CALL_STARTED).len(), 2);
    assert!(events(&kernel, ce::OUTPUT_REPLY).is_empty());
    result(
        &mut kernel,
        &next,
        json!({"status":"ok","text":"after the tool"}),
    );
    one_turn(&kernel);
    assert_eq!(events(&kernel, ce::TOOL_EXEC_STARTED).len(), 1);
}
