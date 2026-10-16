use super::*;
use crate::{EventDraft, EventLog};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

fn open(root: &Path) -> EventLog {
    let mut declarations = ce::core_event_decls();
    declarations.extend(crate::components::subagent::manifest().events);
    EventLog::open_segmented(declarations, "alias-stream", root.into(), 4096).unwrap()
}

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    log.append(EventDraft::new(kind, causes, payload), "fixture")
        .unwrap()
}

fn start(log: &mut EventLog, tool: &str) -> EventEnvelope {
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({
            "call":"reused-call", "tool":tool, "arguments":{}
        }),
    )
}

fn receipt(log: &mut EventLog, request: &EventEnvelope, result: Value) -> EventEnvelope {
    append(
        log,
        ce::TOOL_EXEC_COMPLETED,
        &[&request.id],
        json!({
            "call":"reused-call", "status":"ok", "result":result
        }),
    )
}

fn wake(log: &mut EventLog, request: &EventEnvelope, body: Value) -> EventEnvelope {
    append(
        log,
        ce::WAKE,
        &[&request.id],
        json!({
            "source":"not-an-identity", "summary":"finished", "body":body
        }),
    )
}

fn forced(registry: &mut Registry, identity: Identity) -> Result<String, AssignmentError> {
    registry.assign_digest(identity, [0xab; 32])
}

#[test]
fn receipts_and_wakes_share_identity_across_all_kinds() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    let mut pairs = Vec::new();
    for (tool, result, body) in [
        ("ask", json!({"job":1}), json!({"job":1})),
        (
            "Run",
            json!({"job":"call","background":true,"continuation":"wait"}),
            json!({"job":"call"}),
        ),
        ("Schedule", json!({"timer":1}), json!({"timer":1,"fire":1})),
        ("Watch", json!({"watch":1}), json!({"watch":1,"fire":1})),
    ] {
        let request = start(&mut log, tool);
        let result = receipt(&mut log, &request, result);
        let finished = wake(&mut log, &request, body);
        pairs.push((request, result, finished));
    }
    let reader = log.reader();
    let view = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    assert_eq!(view.identities().len(), 4);
    assert_eq!(view.bodies_read, 8);
    for (request, result, finished) in pairs {
        let identity = identity_for_event(&reader, &result).unwrap().unwrap();
        assert_eq!(identity.created_by, request.id);
        assert_eq!(
            identity_for_event(&reader, &finished).unwrap(),
            Some(identity.clone())
        );
        let alias = view.alias(&identity).unwrap();
        assert_eq!(alias.len(), 8);
        assert_eq!(view.resolve(alias), Some(&identity));
    }
}

#[test]
fn late_receipts_and_early_wakes_keep_collision_assignments_after_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("ledger");
    let mut log = open(&root);
    let old_request = start(&mut log, "Schedule");
    let newer_request = start(&mut log, "Watch");
    let newer_result = receipt(&mut log, &newer_request, json!({"watch":1}));
    let reader = log.reader();
    let mut live = Projection::recover_with(&reader, reader.snapshot_end(), forced).unwrap();
    let newer = identity_for_event(&reader, &newer_result).unwrap().unwrap();
    assert_eq!(live.alias(&newer), Some("abababab"));
    assert!(live.save(&reader).unwrap());

    let early_wake = wake(&mut log, &old_request, json!({"timer":1,"fire":1}));
    let old_result = receipt(&mut log, &old_request, json!({"timer":1}));
    wake(&mut log, &newer_request, json!({"watch":1,"fire":1}));
    let third_request = start(&mut log, "Schedule");
    let third_result = receipt(&mut log, &third_request, json!({"timer":1}));
    live.advance(&reader, reader.snapshot_end()).unwrap();
    let older = identity_for_event(&reader, &early_wake).unwrap().unwrap();
    let third = identity_for_event(&reader, &third_result).unwrap().unwrap();
    assert_eq!(
        identity_for_event(&reader, &old_result).unwrap(),
        Some(older.clone())
    );
    let expected = [
        (newer, "abababab"),
        (older, "ababababa"),
        (third, "ababababab"),
    ];
    let warm = Projection::recover_with(&reader, reader.snapshot_end(), forced).unwrap();
    for (identity, alias) in &expected {
        assert_eq!(live.alias(identity), Some(*alias));
        assert_eq!(warm.alias(identity), Some(*alias));
    }
    warm.save(&reader).unwrap();
    drop(reader);
    drop(log);

    let reopened = open(&root);
    let reader = reopened.reader();
    let cached = Projection::recover_with(&reader, reader.snapshot_end(), forced).unwrap();
    assert_eq!(cached.headers_read, 0);
    assert_eq!(cached.bodies_read, 0);
    let cache = root.join(format!("checkpoint-{:x}.json", Sha256::digest(CONSUMER)));
    std::fs::remove_file(cache).unwrap();
    let cold = Projection::recover_with(&reader, reader.snapshot_end(), forced).unwrap();
    assert!(cold.headers_read > 0);
    for (identity, alias) in expected {
        assert_eq!(cached.resolve(alias), Some(&identity));
        assert_eq!(cold.resolve(alias), Some(&identity));
    }
}

#[test]
fn cancellation_foreground_run_errors_and_unrelated_json_do_not_create_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    for tool in ["Unschedule", "Unwatch", "CancelExpert", "Read"] {
        let request = start(&mut log, tool);
        receipt(&mut log, &request, json!({"timer":1,"watch":1,"job":1}));
    }
    let run = start(&mut log, "Run");
    receipt(&mut log, &run, json!({"job":"foreground","exit_code":0}));
    let failed = start(&mut log, "Schedule");
    append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&failed.id],
        json!({
            "call":"reused-call", "status":"error", "result":{"timer":1},
            "error":{"code":"fixture.failure","message":"not scheduled","blame":"request","retryable":false}
        }),
    );
    append(
        &mut log,
        ce::WAKE,
        &[],
        json!({
            "source":"fswatch:internal", "summary":"internal", "body":{"watch":1}
        }),
    );
    let reader = log.reader();
    let view = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    assert!(view.identities().is_empty());
    // Only Run and failed Schedule have potentially relevant creating requests.
    assert_eq!(view.bodies_read, 2);
}

#[test]
fn foreground_expert_request_establishes_the_same_identity_as_its_final_result() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    let request = start(&mut log, "ask");
    let launched = append(
        &mut log,
        EXPERT_REQUESTED,
        &[&request.id],
        json!({
            "job":7,"expert":"fixture","prompt":"work","background":false,"call":"reused-call"
        }),
    );
    let reader = log.reader();
    let mut view = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let identity = identity_for_event(&reader, &launched).unwrap().unwrap();
    let alias = view.alias(&identity).unwrap().to_owned();
    let result = receipt(&mut log, &request, json!({"job":7,"text":"done"}));
    view.advance(&reader, reader.snapshot_end()).unwrap();
    assert_eq!(
        identity_for_event(&reader, &result).unwrap(),
        Some(identity.clone())
    );
    assert_eq!(view.alias(&identity), Some(alias.as_str()));
    assert_eq!(view.identities().len(), 1);
}

#[test]
fn warm_projection_skips_old_headers_and_unrelated_bodies() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    let request = start(&mut log, "Schedule");
    receipt(&mut log, &request, json!({"timer":1}));
    let reader = log.reader();
    let first = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    first.save(&reader).unwrap();
    for _ in 0..20 {
        let request = start(&mut log, "Read");
        receipt(&mut log, &request, json!({"text":"x".repeat(4096)}));
    }
    let warm = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    assert_eq!(warm.headers_read, 40);
    assert_eq!(warm.bodies_read, 0);
    assert_eq!(warm.identities(), first.identities());
}

#[test]
fn ambiguous_sources_fail_without_publishing_an_advanced_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    let first = start(&mut log, "Schedule");
    let second = start(&mut log, "Schedule");
    let reader = log.reader();
    let mut view = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let previous = view.through();
    let mut result = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&first.id, &second.id],
        json!({
            "call":"reused-call","status":"ok","result":{"timer":1}
        }),
    );
    let error = view.advance(&reader, reader.snapshot_end()).unwrap_err();
    assert!(error.to_string().contains("ambiguous creating requests"));
    assert_eq!(view.through(), previous);
    assert!(view.identities().is_empty());
    view.save(&reader).unwrap();
    let checkpoint = reader
        .load_checkpoint::<State>(CONSUMER, VERSION, reader.snapshot_end())
        .unwrap();
    assert_eq!(checkpoint.through, previous);
    result.causes = vec!["absent-event".into()];
    assert!(identity_for_event(&reader, &result)
        .unwrap_err()
        .to_string()
        .contains("cause is absent"));
}

#[test]
fn a_projection_cannot_be_advanced_or_saved_against_another_opened_history() {
    let temp = tempfile::tempdir().unwrap();
    let a = open(&temp.path().join("a"));
    let b = open(&temp.path().join("b"));
    let reader = a.reader();
    let mut view = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    assert!(view
        .advance(&b.reader(), b.reader().snapshot_end())
        .is_err());
    assert!(view.save(&b.reader()).is_err());
    assert!(view.save_best_effort(&b.reader()).is_err());
    assert!(Projection::recover(&reader, reader.snapshot_end() + 1).is_err());
}

#[test]
fn a_foreign_event_cannot_borrow_local_creating_requests() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    let request = start(&mut log, "Schedule");
    let mut result = receipt(&mut log, &request, json!({"timer":1}));
    result.stream = "another-stream".into();
    let error = identity_for_event(&log.reader(), &result).unwrap_err();
    assert!(error.to_string().contains("another stream"));
}

#[test]
fn invalid_derived_identities_rebuild_instead_of_becoming_authority() {
    let temp = tempfile::tempdir().unwrap();
    let mut log = open(&temp.path().join("ledger"));
    let request = start(&mut log, "Schedule");
    let result = receipt(&mut log, &request, json!({"timer":1}));
    let reader = log.reader();
    let identity = identity_for_event(&reader, &result).unwrap().unwrap();
    let mut foreign = identity.clone();
    foreign.stream = "another-stream".into();
    for identities in [vec![foreign], vec![identity.clone(), identity.clone()]] {
        reader
            .save_checkpoint(
                CONSUMER,
                VERSION,
                reader.snapshot_end(),
                &State { identities },
            )
            .unwrap();
        let rebuilt = Projection::recover(&reader, reader.snapshot_end()).unwrap();
        assert!(rebuilt
            .cold_reason
            .as_deref()
            .unwrap()
            .contains("invalid background alias checkpoint"));
        assert_eq!(rebuilt.identities(), std::slice::from_ref(&identity));
        assert!(rebuilt.headers_read > 0);
    }
}
