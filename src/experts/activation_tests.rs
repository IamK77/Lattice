use super::activation::{Activation, Activations, ACTIVATE};
use super::tests::{definition, fixture, put};
use super::*;
use crate::EventEnvelope;
use serde_json::json;

fn approved(candidate: &Candidate) -> EventEnvelope {
    serde_json::from_value(json!({
        "v":1,"id":"approved-1","seq":7,"stream":"parent","time":"synthetic",
        "type":crate::core_events::TOOL_EXEC_STARTED,"source":"trust","causes":["request-1","answer-1"],
        "payload":{"call":"activate-1","tool":ACTIVATE,"arguments":{
            "operation":"activate","target":candidate.identity,"definition":candidate.definition,
            "fileVersion":candidate.file_version,"reason":"Enable the reviewed expert"
        }}
    })).unwrap()
}

#[test]
fn permission_alone_does_not_create_activation_and_restart_only_reads_committed_state() {
    let (directory, definitions) = fixture();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&definition()).unwrap(),
    );
    let candidate = definitions.read(Scope::Project, "reviewer").unwrap();
    let store = Activations::new(directory.path());
    let event = approved(&candidate);
    let record = Activation::approved(
        &candidate,
        &event,
        &directory.path().join("parent.ledger"),
        "trust",
    )
    .unwrap();
    assert!(store.read(&candidate.identity).unwrap().is_none());
    store.commit(&record, None).unwrap();
    drop(store);
    let reopened = Activations::new(directory.path());
    let restored = reopened.read(&candidate.identity).unwrap().unwrap();
    assert_eq!(restored, record);
    restored.verify(&candidate, &event, "trust").unwrap();
    // An absent final tool reply does not undo an already committed state file.
    // Recovery does not write another state record or issue another call.
    assert_eq!(event.event_type, crate::core_events::TOOL_EXEC_STARTED);
}

#[test]
fn a_manual_revision_change_is_pending_even_when_the_old_revision_was_activated() {
    let (directory, definitions) = fixture();
    let original = definition();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&original).unwrap(),
    );
    let candidate = definitions.read(Scope::Project, "reviewer").unwrap();
    let event = approved(&candidate);
    let record = Activation::approved(
        &candidate,
        &event,
        &directory.path().join("parent.ledger"),
        "trust",
    )
    .unwrap();
    let store = Activations::new(directory.path());
    store.commit(&record, None).unwrap();
    let mut changed = original;
    changed.instructions.push_str(" Also edit files.");
    changed.capabilities.push(Capability::Write);
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&changed).unwrap(),
    );
    let current = definitions.read(Scope::Project, "reviewer").unwrap();
    let restored = Activations::new(directory.path())
        .read(&candidate.identity)
        .unwrap()
        .unwrap();
    assert!(!restored.matches(&current));
    assert!(restored.verify(&current, &event, "trust").is_err());
    assert!(Activation::approved(
        &current,
        &event,
        &directory.path().join("parent.ledger"),
        "trust"
    )
    .is_err());
}

#[test]
fn activation_cannot_borrow_approval_from_another_scope_operation_source_or_body() {
    let (directory, definitions) = fixture();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&definition()).unwrap(),
    );
    let candidate = definitions.read(Scope::Project, "reviewer").unwrap();
    let event = approved(&candidate);
    let ledger = directory.path().join("parent.ledger");
    let mut wrong = event.clone();
    wrong.source = "model".into();
    assert!(Activation::approved(&candidate, &wrong, &ledger, "trust").is_err());
    for (field, value) in [
        ("operation", json!("delete")),
        (
            "target",
            json!(definitions.identity(Scope::Personal, "reviewer").unwrap()),
        ),
        ("definition", json!({"approved":true})),
        ("fileVersion", json!("changed")),
    ] {
        let mut wrong = event.clone();
        wrong.payload["arguments"][field] = value;
        assert!(
            Activation::approved(&candidate, &wrong, &ledger, "trust").is_err(),
            "accepted {field}"
        );
    }
    let record = Activation::approved(&candidate, &event, &ledger, "trust").unwrap();
    let mut other = event.clone();
    other.id = "unrelated".into();
    assert!(record.verify(&candidate, &other, "trust").is_err());
    other = event;
    other.stream = "another-stream".into();
    assert!(record.verify(&candidate, &other, "trust").is_err());
}

#[test]
fn concurrent_managed_activation_conflicts_instead_of_overwriting_the_winner() {
    let (directory, definitions) = fixture();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&definition()).unwrap(),
    );
    let candidate = definitions.read(Scope::Project, "reviewer").unwrap();
    let event = approved(&candidate);
    let first = Activation::approved(
        &candidate,
        &event,
        &directory.path().join("parent.ledger"),
        "trust",
    )
    .unwrap();
    let store = Activations::new(directory.path());
    store.commit(&first, None).unwrap();
    let mut second = first.clone();
    second.authorization.event = "approved-2".into();
    assert!(store.commit(&second, None).is_err());
    assert_eq!(store.read(&candidate.identity).unwrap().unwrap(), first);
    store.commit(&second, Some(&first)).unwrap();
    assert_eq!(store.read(&candidate.identity).unwrap().unwrap(), second);
}

#[test]
fn formatting_changes_do_not_revoke_semantic_approval() {
    let (directory, definitions) = fixture();
    let original = definition();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&original).unwrap(),
    );
    let candidate = definitions.read(Scope::Project, "reviewer").unwrap();
    let event = approved(&candidate);
    let record = Activation::approved(
        &candidate,
        &event,
        &directory.path().join("parent.ledger"),
        "trust",
    )
    .unwrap();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec_pretty(&original).unwrap(),
    );
    let current = definitions.read(Scope::Project, "reviewer").unwrap();
    assert_ne!(current.file_version, candidate.file_version);
    record.verify(&current, &event, "trust").unwrap();
}
