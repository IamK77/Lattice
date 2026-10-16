use super::*;
use crate::{core_events as ce, EventDraft, EventLog};

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> crate::EventEnvelope {
    log.append(EventDraft::new(kind, causes, payload), "fixture")
        .unwrap()
}

#[test]
fn aliases_resolve_exactly_before_execution_and_legacy_numbers_are_untouched() {
    for (create, cancel, field, kind) in [
        ("Schedule", "Unschedule", "timer", Kind::Timer),
        ("Watch", "Unwatch", "watch", Kind::Watch),
        ("ask", "CancelExpert", "job", Kind::Expert),
    ] {
        let mut log = EventLog::in_memory(ce::core_event_decls(), "owner");
        let start = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"start","tool":create,"arguments":{}}),
        );
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&start.id],
            json!({"call":"start","status":"ok","result":{(field):1}}),
        );
        let reader = log.reader();
        let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
        let identity = projection.identities()[0].clone();
        let alias = projection.alias(&identity).unwrap();
        let mut resolver = Resolver::default();
        let mut payload =
            json!({"call":"stop","tool":cancel,"arguments":{(field):alias,"reason":"requested"}});
        resolver.normalize(&reader, &mut payload);
        assert_eq!(payload["arguments"][field], 1);
        assert_eq!(payload["arguments"]["reason"], "requested");
        assert_eq!(reference(&payload).unwrap().unwrap().identity, identity);
        assert!(validate(&payload, "owner", kind, Some(1), Some(&start.id)).is_ok());
        for raw in [json!(0), json!(1), json!(999), json!(null)] {
            let mut payload = json!({"call":"legacy","tool":cancel,"arguments":{(field):raw}});
            let before = payload.clone();
            resolver.normalize(&reader, &mut payload);
            assert_eq!(payload, before);
        }
        for invalid in [
            "00000001".to_owned(),
            alias[..7].to_owned(),
            format!("{alias}0"),
        ] {
            let mut payload = json!({"tool":cancel,"arguments":{(field):invalid}});
            resolver.normalize(&reader, &mut payload);
            assert_eq!(payload["arguments"][field], invalid);
            assert!(payload[RESOLUTION_ERROR]
                .as_str()
                .unwrap()
                .contains("unknown background reference"));
            assert!(validate(&payload, "owner", kind, Some(1), Some(&start.id)).is_err());
        }
        let mut wrong = json!({"tool":if kind == Kind::Watch { "Unschedule" } else { "Unwatch" },"arguments":{"timer":alias,"watch":alias}});
        resolver.normalize(&reader, &mut wrong);
        assert!(wrong[RESOLUTION_ERROR].as_str().unwrap().contains("not"));
    }
}

#[test]
fn late_receipt_cannot_make_an_older_incarnation_current_again() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "owner");
    let old = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"old","tool":"Schedule","arguments":{}}),
    );
    let new = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"new","tool":"Schedule","arguments":{}}),
    );
    for start in [&new, &old] {
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&start.id],
            json!({"call":start.payload["call"],"status":"ok","result":{"timer":1}}),
        );
    }
    let reader = log.reader();
    let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let mut resolver = Resolver::default();
    for identity in projection.identities() {
        let alias = projection.alias(identity).unwrap();
        let mut request = json!({"tool":"Unschedule","arguments":{"timer":alias}});
        resolver.normalize(&reader, &mut request);
        if identity.created_by == old.id {
            assert_eq!(request["arguments"]["timer"], alias);
            assert!(request[RESOLUTION_ERROR]
                .as_str()
                .unwrap()
                .contains("stale"));
        } else {
            assert_eq!(request["arguments"]["timer"], 1);
            assert!(request.get(RESOLUTION_ERROR).is_none());
        }
    }
}

#[test]
fn operation_metadata_cannot_allocate_an_alias_or_hide_missing_evidence() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "owner");
    let identity = super::super::Identity {
        stream: "owner".into(),
        kind: Kind::Timer,
        original_id: OriginalId::Number(1),
        created_by: "never-created".into(),
    };
    let request = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"stop","tool":"Unschedule","arguments":{"timer":1}, (REFERENCE):Reference {alias:"12345678".into(), identity}}),
    );
    let receipt = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&request.id],
        json!({"call":"stop","status":"error","error":{"code":"background.invalid_reference","message":"no target","blame":"request","retryable":false,"transient":false}}),
    );
    let reader = log.reader();
    let mut projection = None;
    assert!(super::super::reference_for_event(
        &mut projection,
        &reader,
        &receipt,
        reader.snapshot_end()
    )
    .unwrap()
    .is_none());
    assert!(projection.unwrap().identities().is_empty());
    let mut broken = receipt;
    broken.causes = vec!["missing-source".into()];
    assert!(receipt_identity(&reader, &broken).is_err());
}

#[test]
fn execution_guard_rechecks_creation_scope_and_kind_without_reading_history() {
    let identity = super::super::Identity {
        stream: "owner".into(),
        kind: Kind::Timer,
        original_id: OriginalId::Number(1),
        created_by: "old".into(),
    };
    let request = json!({(REFERENCE):Reference {alias:"12345678".into(),identity}});
    assert!(validate(&request, "owner", Kind::Timer, Some(1), Some("old")).is_ok());
    for (stream, kind, id, creation) in [
        ("owner", Kind::Timer, Some(1), Some("new")),
        ("owner", Kind::Timer, Some(1), None),
        ("other", Kind::Timer, Some(1), Some("old")),
        ("owner", Kind::Watch, Some(1), Some("old")),
        ("owner", Kind::Timer, Some(2), Some("old")),
    ] {
        assert!(validate(&request, stream, kind, id, creation).is_err());
    }
    assert!(
        validate(&json!({}), "owner", Kind::Timer, Some(999), None).is_ok(),
        "legacy future-ID behavior is unchanged"
    );
    let targets = LiveTargets::default();
    let old = targets.track(1, "old");
    assert_eq!(targets.creation(Some(1)).as_deref(), Some("old"));
    let new = targets.track(1, "new");
    drop(old);
    assert_eq!(targets.creation(Some(1)).as_deref(), Some("new"));
    drop(new);
    assert_eq!(targets.creation(Some(1)), None);
}
