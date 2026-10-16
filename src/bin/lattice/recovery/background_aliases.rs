use super::*;
use lattice::background_aliases::{identity_for_event, Projection};

#[test]
fn background_aliases_match_shared_projection_after_cold_warm_and_reused_ids() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "alias-owner", root, 4096).unwrap();
    let mut receipts = Vec::new();
    for (tool, args, result, key) in [
        (
            "ask",
            json!({"expert":"explorer","background":true}),
            json!({"job":1}),
            "expert:1",
        ),
        (
            "Run",
            json!({"command":"fixture"}),
            json!({"job":"long-call-id","background":true}),
            "background:long-call-id",
        ),
        (
            "Schedule",
            json!({"interval_ms":1000}),
            json!({"timer":1}),
            "timer:1",
        ),
        (
            "Watch",
            json!({"path":"/fixture"}),
            json!({"watch":1}),
            "watch:1",
        ),
    ] {
        let start = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":tool,"tool":tool,"arguments":args}),
        );
        // The identity is the request actually seen by the provider, not an
        // arbitrary ancestor or the first request with this model call ID.
        let forwarded = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[&start.id],
            start.payload.clone(),
        );
        let receipt = append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&forwarded.id],
            json!({"call":tool,"status":"ok","result":result}),
        );
        receipts.push((key, receipt, forwarded.id));
    }
    let reader = log.reader();
    let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let mut first_timer = String::new();
    for warm in [false, true] {
        let mut ui = Ui::replayed(&[]);
        ui.replay_prefix(&reader, reader.snapshot_end()).unwrap();
        assert_eq!(ui.background().len(), 4);
        if warm {
            assert_eq!(ui.recovery.as_ref().unwrap().replayed, 0);
        }
        for (key, receipt, creator) in &receipts {
            let row = ui.background().iter().find(|row| row.key == *key).unwrap();
            let reference = row
                .reference
                .as_ref()
                .expect("real history supplies a display identity");
            let identity = identity_for_event(&reader, receipt).unwrap().unwrap();
            assert_eq!(reference.identity, identity);
            assert_eq!(&reference.identity.created_by, creator);
            assert_eq!(reference.identity.stream, "alias-owner");
            assert_eq!(Some(reference.alias.as_str()), projection.alias(&identity));
            if *key == "timer:1" {
                first_timer = reference.alias.clone();
            }
        }
    }
    let mut notices = Vec::new();
    for (key, receipt, creator) in &receipts {
        let identity = identity_for_event(&reader, receipt).unwrap().unwrap();
        let reference = lattice::background_aliases::Reference {
            alias: projection.alias(&identity).unwrap().to_owned(),
            identity,
        };
        let body = match *key {
            "expert:1" => json!({"job":1}),
            "background:long-call-id" => json!({"job":"long-call-id","exit_code":0}),
            "timer:1" => json!({"timer":1,"fire":1}),
            _ => json!({"watch":1,"fire":1}),
        };
        append(
            &mut log,
            ce::WAKE,
            &[creator],
            json!({"source":key,"summary":"notice","body":body}),
        );
        notices.push(format!("{reference} ↯"));
    }
    for _ in 0..2 {
        let mut ui = Ui::replayed(&[]);
        ui.replay_prefix(&reader, reader.snapshot_end()).unwrap();
        assert_eq!(ui.domain.unseen.lines(), notices);
    }
    let mut delayed =
        json!({"call":"delayed-stop","tool":"Unschedule","arguments":{"timer":first_timer}});
    lattice::background_aliases::operations::Resolver::default().normalize(&reader, &mut delayed);
    assert!(lattice::background_aliases::operations::reference(&delayed)
        .unwrap()
        .is_some());
    let cancel = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"stop","tool":"Unschedule","arguments":{"timer":1}}),
    );
    append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&cancel.id],
        json!({"call":"stop","status":"ok","result":{"cancelled":1}}),
    );
    let start = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"again","tool":"Schedule","arguments":{"interval_ms":2000}}),
    );
    let receipt = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&start.id],
        json!({"call":"again","status":"ok","result":{"timer":1}}),
    );
    let old_creator = &receipts
        .iter()
        .find(|(key, _, _)| *key == "timer:1")
        .unwrap()
        .2;
    append(
        &mut log,
        ce::WAKE,
        &[old_creator],
        json!({"source":"timer:1","body":{"timer":1,"fire":2}}),
    );
    let delayed = append(&mut log, ce::TOOL_EXEC_STARTED, &[], delayed);
    append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&delayed.id],
        json!({"call":"delayed-stop","status":"error","error":{"code":"background.invalid_reference","message":"stale target","blame":"request","retryable":false,"transient":false}}),
    );
    let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let identity = identity_for_event(&reader, &receipt).unwrap().unwrap();
    for _ in 0..2 {
        let mut ui = Ui::replayed(&[]);
        ui.replay_prefix(&reader, reader.snapshot_end()).unwrap();
        let row = ui
            .background()
            .iter()
            .find(|row| row.key == "timer:1")
            .unwrap();
        let reference = row.reference.as_ref().unwrap();
        assert_eq!(Some(reference.alias.as_str()), projection.alias(&identity));
        assert_ne!(reference.alias, first_timer);
        assert_eq!(reference.identity.created_by, start.id);
        assert_eq!(row.key, "timer:1");
        assert_eq!(
            row.fires, 0,
            "an old invocation's wake must not update the reused numeric ID"
        );
    }
    assert!(
        projection.resolve(&first_timer).is_some(),
        "retired reservations survive row removal"
    );
}

#[test]
fn late_old_receipt_never_replaces_a_new_invocation_even_after_its_row_is_gone() {
    for finished in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::open_segmented(
            ce::core_event_decls(),
            "owner",
            dir.path().join("late.ledger"),
            4096,
        )
        .unwrap();
        let old = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"old","tool":"Schedule","arguments":{"interval_ms":1000}}),
        );
        let args = if finished {
            json!({"delay_ms":2000})
        } else {
            json!({"interval_ms":2000})
        };
        let new = append(
            &mut log,
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"new","tool":"Schedule","arguments":args}),
        );
        let receipt = append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&new.id],
            json!({"call":"new","status":"ok","result":{"timer":1}}),
        );
        if finished {
            append(
                &mut log,
                ce::WAKE,
                &[&new.id],
                json!({"source":"timer:1","body":{"timer":1,"fire":1}}),
            );
        }
        append(
            &mut log,
            ce::TOOL_EXEC_COMPLETED,
            &[&old.id],
            json!({"call":"old","status":"ok","result":{"timer":1}}),
        );
        if !finished {
            append(
                &mut log,
                ce::WAKE,
                &[&new.id],
                json!({"source":"timer:1","body":{"timer":1,"fire":1}}),
            );
        }
        let reader = log.reader();
        let identity = identity_for_event(&reader, &receipt).unwrap().unwrap();
        for _ in 0..2 {
            let mut ui = Ui::replayed(&[]);
            ui.replay_prefix(&reader, reader.snapshot_end()).unwrap();
            if finished {
                assert!(
                    ui.background().is_empty(),
                    "a late old receipt cannot revive a superseded invocation"
                );
            } else {
                let row = &ui.background()[0];
                assert_eq!(row.reference.as_ref().unwrap().identity, identity);
                assert_eq!(row.label, "every 2s");
                assert_eq!(row.fires, 1);
            }
        }
    }
}
