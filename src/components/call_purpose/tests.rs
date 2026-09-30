use super::*;
use crate::{EventDraft, EventLog};
use serde_json::json;

#[test]
fn forwarded_auxiliary_results_use_the_matching_call_among_all_causes() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "classification");
    let auxiliary_call = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"tool":"fixture","call":"ui","arguments":{},"purpose":"frontend.test"}),
            ),
            "frontend",
        )
        .unwrap();
    let forwarded = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&auxiliary_call.id],
                json!({"tool":"fixture","call":"ui","arguments":{}}),
            ),
            "gate",
        )
        .unwrap();
    let ordinary = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"tool":"fixture","call":"model","arguments":{}}),
            ),
            "loop",
        )
        .unwrap();
    let result = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&ordinary.id, &forwarded.id],
                json!({"call":"ui","status":"ok","result":{}}),
            ),
            "provider",
        )
        .unwrap();
    assert!(auxiliary(&log.reader(), &result).unwrap());
    let result = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&auxiliary_call.id, &ordinary.id],
                json!({"call":"model","status":"ok","result":{}}),
            ),
            "provider",
        )
        .unwrap();
    assert!(!auxiliary(&log.reader(), &result).unwrap());
    let mixed = log
        .append(
            EventDraft::new(
                ce::INTERRUPTED,
                &[&ordinary.id, &forwarded.id],
                json!({"by":"user"}),
            ),
            "ui",
        )
        .unwrap();
    assert!(!auxiliary(&log.reader(), &mixed).unwrap());
}

#[test]
fn auxiliary_ancestors_do_not_hide_independent_tool_calls() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "classification");
    let parent = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"tool":"fixture","call":"ui","arguments":{},"purpose":null}),
            ),
            "frontend",
        )
        .unwrap();
    let independent = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&parent.id],
                json!({"tool":"fixture","call":"other","arguments":{}}),
            ),
            "loop",
        )
        .unwrap();
    assert!(!auxiliary(&log.reader(), &independent).unwrap());
    let input = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[&parent.id], json!({"text":"Continue"})),
            "ui",
        )
        .unwrap();
    assert!(!auxiliary(&log.reader(), &input).unwrap());
    let interrupted = log
        .append(
            EventDraft::new(ce::INTERRUPTED, &[&parent.id], json!({"by":"restart"})),
            "core",
        )
        .unwrap();
    assert!(auxiliary(&log.reader(), &interrupted).unwrap());
}
