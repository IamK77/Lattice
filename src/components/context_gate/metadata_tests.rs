use super::*;
use crate::EventLog;
use std::os::unix::fs::FileExt;

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, payload), "fixture")
        .unwrap()
}

#[test]
fn classification_uses_cold_metadata_but_selected_bodies_still_fail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("classification.jsonl");
    let mut log =
        EventLog::open(ce::core_event_decls(), "classification", Some(path.clone())).unwrap();
    let mut requests = Vec::new();
    let mut request = |purpose: Option<Value>| {
        let offset = std::fs::metadata(&path).unwrap().len();
        let mut payload = json!({"model":"fixture", "input":{"parts":[], "fingerprint":"sha256:test"}, "tools":[]});
        if let Some(purpose) = purpose {
            payload["purpose"] = purpose;
        }
        let event = append(&mut log, ce::MODEL_CALL_STARTED, &[], payload);
        requests.push((event.id.clone(), offset));
        event
    };
    let main = request(None);
    let null = request(Some(Value::Null));
    let number = request(Some(json!(7)));
    let other = request(Some(json!("other")));
    let condensed = request(Some(json!(CONDENSE_PURPOSE)));
    let compacted = request(Some(json!("context.compact.responses")));
    let main_offset = std::fs::metadata(&path).unwrap().len();
    let main_reply = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&main.id],
        json!({"status":"ok", "usage":{"input_tokens":19}}),
    );
    for event in [null, number, other] {
        append(
            &mut log,
            ce::MODEL_CALL_COMPLETED,
            &[&event.id],
            json!({"status":"ok", "usage":{"input_tokens":999}}),
        );
    }
    let failed_offset = std::fs::metadata(&path).unwrap().len();
    let failed = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&condensed.id],
        json!({"status":"error"}),
    );
    drop(log);
    let mut log =
        EventLog::open(ce::core_event_decls(), "classification", Some(path.clone())).unwrap();
    let disk = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    // Break every request after indexing. Classification must not read even
    // the newest request; selected usage/status bodies remain intact.
    for (_, offset) in &requests {
        disk.write_all_at(b"!", *offset).unwrap();
    }
    let gate = ContextGate::from_config(None);
    assert_eq!(
        gate.last_main_usage(&log.reader(), "input_tokens").unwrap(),
        19
    );
    assert!(gate.condense_in_flight(&log.reader()).unwrap());
    assert!(gate.condensation_paused(&log.reader()).unwrap());
    append(
        &mut log,
        ce::INTERRUPTED,
        &[&compacted.id],
        json!({"by":"fixture"}),
    );
    assert!(!gate.condense_in_flight(&log.reader()).unwrap());
    for (id, _) in &requests {
        assert!(log.reader().get(id).is_err());
    }

    // Use fresh cold readers for required-body failure checks. Opening the
    // already damaged JSONL would rightly fail, so first restore each changed
    // opening byte, reopen, then corrupt the selected completion instead.
    for (_, offset) in &requests {
        disk.write_all_at(b"{", *offset).unwrap();
    }
    drop(log);
    let log = EventLog::open(ce::core_event_decls(), "classification", Some(path.clone())).unwrap();
    disk.write_all_at(b"!", main_offset).unwrap();
    let error = gate
        .last_main_usage(&log.reader(), "input_tokens")
        .unwrap_err();
    assert!(error.contains(&main_reply.id), "{error}");
    disk.write_all_at(b"{", main_offset).unwrap();
    drop(log);
    let mut log =
        EventLog::open(ce::core_event_decls(), "classification", Some(path.clone())).unwrap();
    disk.write_all_at(b"!", failed_offset).unwrap();
    assert!(gate
        .condensation_paused(&log.reader())
        .unwrap_err()
        .contains(&failed.id));
    // Retaining a failure across later cancellations requires observing its
    // status. A newer profile cannot hide a required, unreadable observation
    // while the shared committed prefix is being rebuilt.
    append(
        &mut log,
        ce::EXTERNAL_INPUT,
        &[],
        json!({"channel":MODEL_CHANNEL, "value":{}}),
    );
    assert!(gate
        .condensation_paused(&log.reader())
        .unwrap_err()
        .contains(&failed.id));
    disk.write_all_at(b"{", failed_offset).unwrap();
    assert!(!gate.condensation_paused(&log.reader()).unwrap());
}

#[test]
fn missing_tool_ids_and_multi_cause_interruptions_keep_their_meaning() {
    let declarations = [
        ce::MODEL_CALL_COMPLETED,
        ce::TOOL_EXEC_STARTED,
        ce::INTERRUPTED,
    ]
    .into_iter()
    .map(|kind| EventTypeDecl::new(kind, "fixture"))
    .collect();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calls.jsonl");
    let mut log = EventLog::open(declarations, "calls", Some(path.clone())).unwrap();
    let invocation = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[],
        json!({"toolCalls":[{"id":"a"}, {}, {"id":7}]}),
    );
    let a = append(&mut log, ce::TOOL_EXEC_STARTED, &[], json!({"call":"a"}));
    let b = append(&mut log, ce::TOOL_EXEC_STARTED, &[], json!({"call":"b"}));
    let interrupted = append(&mut log, ce::INTERRUPTED, &[&a.id, &b.id], json!({}));
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(0)
        .unwrap();
    let parts = vec![
        json!({"event":invocation.id}),
        json!({"digest":{"of":interrupted.id,"text":"unknown"}}),
    ];
    let covered = [invocation.id, interrupted.id].into_iter().collect();
    assert!(complete_exchange_coverage(&parts, covered, &log.reader())
        .unwrap()
        .is_empty());
    let index = crate::components::model_common::answer_index(&parts, &log.reader()).unwrap();
    assert_eq!(index["a"], 1);
    assert_eq!(index["b"], 1);
}
