use super::*;

fn contains(request: &EventEnvelope, event: &EventEnvelope) -> bool {
    request.payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|part| part["event"] == event.id)
}

#[test]
fn sibling_forwarding_result_stays_out_of_material_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let mut kernel = start_with(true, false, Some(path.clone()));
    let first_copy = input(&mut kernel, "first");
    let root = events(&kernel, ce::MODEL_CALL_STARTED)
        .into_iter()
        .find(|event| event.source == "loop")
        .unwrap();
    kernel.injector("gate").emit(
        "forward",
        EventDraft::new(ce::MODEL_CALL_STARTED, &[&root.id], root.payload.clone()),
    );
    settle(&mut kernel);
    let second_copy = latest(&kernel, ce::MODEL_CALL_STARTED);
    assert_ne!(first_copy.id, second_copy.id);
    result(
        &mut kernel,
        &first_copy,
        json!({"status":"ok", "text":"accepted"}),
    );
    let late = result(&mut kernel, &second_copy, tool_payload());
    one_turn(&kernel);
    assert!(events(&kernel, ce::TOOL_EXEC_STARTED).is_empty());
    let before = input(&mut kernel, "before restart");
    assert!(
        !contains(&before, &late),
        "live material rejects the sibling's late tool result"
    );
    result(
        &mut kernel,
        &before,
        json!({"status":"ok", "text":"second turn"}),
    );
    drop(kernel.shutdown());

    let mut resumed = start_with(true, false, Some(path));
    settle(&mut resumed);
    let after = input(&mut resumed, "after restart");
    assert!(
        resumed.log().get(&late.id).unwrap().is_some(),
        "audit retains late result"
    );
    assert!(events(&resumed, ce::TOOL_EXEC_STARTED).is_empty());
    assert!(
        !contains(&after, &late),
        "recovery must not readmit the rejected sibling tool result"
    );
}

#[test]
fn two_loop_recovery_preserves_live_model_outcome_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let mut kernel = start_with(false, true, Some(path.clone()));
    input(&mut kernel, "shared input");
    let requests = events(&kernel, ce::MODEL_CALL_STARTED);
    assert_eq!(requests.len(), 2);
    let outcomes: Vec<_> = requests
        .iter()
        .map(|request| {
            let answer = result(
                &mut kernel,
                request,
                json!({"status":"ok", "text":request.source}),
            );
            (request.source.clone(), answer)
        })
        .collect();
    input(&mut kernel, "before restart");
    let next = events(&kernel, ce::MODEL_CALL_STARTED);
    for request in &next[2..] {
        for (owner, outcome) in &outcomes {
            assert_eq!(
                contains(request, outcome),
                request.source == *owner,
                "live material only contains the loop's own outcome"
            );
        }
        result(
            &mut kernel,
            request,
            json!({"status":"ok", "text":"finished"}),
        );
    }
    drop(kernel.shutdown());

    let mut resumed = start_with(false, true, Some(path));
    settle(&mut resumed);
    input(&mut resumed, "after restart");
    let all = events(&resumed, ce::MODEL_CALL_STARTED);
    for request in &all[4..] {
        for (owner, outcome) in &outcomes {
            assert_eq!(
                contains(request, outcome),
                request.source == *owner,
                "recovered material must preserve live per-loop outcome membership"
            );
        }
    }
}
