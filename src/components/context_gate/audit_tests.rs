use super::*;

#[test]
fn coverage_keeps_parallel_answers_and_interruptions_with_the_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coverage.jsonl");
    let declarations: Vec<_> = [
        ce::MODEL_CALL_COMPLETED,
        ce::TOOL_EXEC_COMPLETED,
        ce::TOOL_EXEC_STARTED,
        ce::INTERRUPTED,
    ]
    .into_iter()
    .map(|name| EventTypeDecl::new(name, "test"))
    .collect();
    let mut log =
        crate::EventLog::open(declarations.clone(), "coverage", Some(path.clone())).unwrap();
    let invocation = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[],
                json!({
                    "toolCalls": [{"id": "a"}, {"id": "b"}]
                }),
            ),
            "model",
        )
        .unwrap();
    let first = log
        .append(
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[], json!({"call": "a"})),
            "tool",
        )
        .unwrap();
    let started = log
        .append(
            EventDraft::new(ce::TOOL_EXEC_STARTED, &[], json!({"call": "b"})),
            "loop",
        )
        .unwrap();
    let second = log
        .append(
            EventDraft::new(ce::INTERRUPTED, &[&started.id], json!({})),
            "core",
        )
        .unwrap();
    let duplicate = log
        .append(
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[], json!({"call": "b"})),
            "tool",
        )
        .unwrap();
    drop(log);
    let log = crate::EventLog::open(declarations, "coverage", Some(path.clone())).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(0)
        .unwrap();
    let ids = [invocation.id, first.id, second.id, duplicate.id];
    let parts: Vec<Value> = ids.iter().map(|id| json!({"event": id})).collect();
    let all: HashSet<String> = ids.iter().cloned().collect();
    assert_eq!(
        complete_exchange_coverage(&parts, all.clone(), &log.reader()).unwrap(),
        all
    );
    for mask in 0..((1 << ids.len()) - 1) {
        let covered = ids
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, id)| id.clone())
            .collect();
        assert!(
            complete_exchange_coverage(&parts, covered, &log.reader())
                .unwrap()
                .is_empty(),
            "partial coverage {mask} must retain the entire exchange"
        );
    }
    let index = crate::components::model_common::answer_index(&parts, &log.reader()).unwrap();
    assert_eq!(index["a"], 1);
    assert_eq!(
        index["b"], 3,
        "the tool reply wins over an interruption without reading either body"
    );
    let missing_answer = &parts[..2];
    assert!(
        complete_exchange_coverage(missing_answer, all, &log.reader())
            .unwrap()
            .is_disjoint(&ids[..2].iter().cloned().collect())
    );
}
