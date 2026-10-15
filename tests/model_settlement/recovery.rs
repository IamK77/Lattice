use super::*;

#[test]
fn two_loops_with_the_same_input_cannot_consume_each_others_outcome() {
    let mut kernel = start_with(false, true, None);
    input(&mut kernel, "shared input");
    let requests = events(&kernel, ce::MODEL_CALL_STARTED);
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].causes, requests[1].causes);
    // Answer the later emission first: both watermark ranges contain it, so
    // causes + sequence alone cannot identify which loop asked this question.
    result(
        &mut kernel,
        &requests[1],
        json!({"status":"ok","text":"second loop"}),
    );
    let replies = events(&kernel, ce::OUTPUT_REPLY);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].source, requests[1].source);
    result(
        &mut kernel,
        &requests[0],
        json!({"status":"ok","text":"first loop"}),
    );
    assert_eq!(events(&kernel, ce::OUTPUT_REPLY).len(), 2);
    assert_eq!(events(&kernel, ce::TURN_COMPLETED).len(), 2);
}

#[test]
fn restart_keeps_late_tools_audited_but_out_of_material_and_execution() {
    for gated in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut kernel = start_with(gated, false, Some(path.clone()));
        let request = input(&mut kernel, "first");
        interrupt(&mut kernel, "model", &request);
        let stale = result(&mut kernel, &request, tool_payload());
        one_turn(&kernel);
        drop(kernel.shutdown());

        let mut resumed = start_with(gated, false, Some(path));
        settle(&mut resumed);
        one_turn(&resumed);
        assert!(events(&resumed, ce::TOOL_EXEC_STARTED).is_empty());
        assert!(resumed.log().reader().get(&stale.id).unwrap().is_some());
        let request = input(&mut resumed, "after restart");
        assert!(
            !request.payload["input"]["parts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|part| part["event"] == stale.id),
            "late tools must not reenter recovered material"
        );
        result(
            &mut resumed,
            &request,
            json!({"status":"ok","text":"new answer"}),
        );
        assert_eq!(events(&resumed, ce::OUTPUT_REPLY).len(), 2);
        assert_eq!(events(&resumed, ce::TURN_COMPLETED).len(), 2);
        assert!(events(&resumed, ce::TOOL_EXEC_STARTED).is_empty());
    }
}
