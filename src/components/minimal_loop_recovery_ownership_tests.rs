use super::*;
use crate::{EventDraft, EventEnvelope, EventLog};
use serde_json::json;

fn request(log: &mut EventLog, owner: &str, causes: &[&str]) -> EventEnvelope {
    log.append(
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            causes,
            json!({"model":"fixture","input":{"parts":[],"fingerprint":"sha256:test"}}),
        ),
        owner,
    )
    .unwrap()
}

fn save_both(log: &EventLog) -> u64 {
    let through = log.reader().snapshot_end();
    for owner in ["left", "right"] {
        let mut projection = MinimalLoop::from_config(None);
        projection
            .sync_material(&log.reader(), through, owner)
            .unwrap();
    }
    through
}

#[test]
fn owned_originals_survive_checkpoint_cuts_without_readmitting_siblings_or_other_loops() {
    // Save before either outcome, after the first original settles, or after
    // both originals and the rejected sibling have completed.
    for cut in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("history.ledger");
        let mut log =
            EventLog::open_segmented(ce::core_event_decls(), "owners", root.clone(), 1024 * 1024)
                .unwrap();
        let input = log
            .append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"shared"})),
                "ui",
            )
            .unwrap();
        let left = request(&mut log, "left", &[&input.id]);
        let right = request(&mut log, "right", &[&input.id]);
        let first_copy = request(&mut log, "gate", &[&left.id]);
        let sibling = request(&mut log, "gate", &[&left.id]);
        let mut boundary = 0;
        if cut == 0 {
            boundary = save_both(&log);
        }
        let accepted = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&first_copy.id],
                    json!({"status":"ok","text":"left answer"}),
                ),
                "model",
            )
            .unwrap();
        if cut == 1 {
            boundary = save_both(&log);
        }
        let other = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&right.id],
                    json!({"status":"ok","text":"right answer"}),
                ),
                "model",
            )
            .unwrap();
        let late = log.append(EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&sibling.id],
            json!({"status":"ok","toolCalls":[{"id":"late","tool":"fixture","arguments":{}}]})), "model").unwrap();
        if cut == 2 {
            boundary = save_both(&log);
        }
        let mut padding = Vec::new();
        if cut == 1 {
            // Put the owner's outcome in a published page so page repair must
            // reproduce ownership, not merely a shared prefix of user inputs.
            for index in 0..140 {
                padding.push(
                    log.append(
                        EventDraft::new(
                            ce::USER_MESSAGE,
                            &[],
                            json!({"text":format!("padding {index}")}),
                        ),
                        "ui",
                    )
                    .unwrap()
                    .id,
                );
            }
        }
        let delivered = log
            .append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"next"})),
                "ui",
            )
            .unwrap();
        drop(log);
        let mut log =
            EventLog::open_segmented(ce::core_event_decls(), "owners", root.clone(), 1024 * 1024)
                .unwrap();
        let reader = log.reader();
        assert_ne!(checkpoint_key("left"), checkpoint_key("right"));
        for (owner, own) in [("left", &accepted), ("right", &other)] {
            let checkpoint = reader
                .load_checkpoint::<Checkpoint>(&checkpoint_key(owner), VERSION, delivered.seq - 1)
                .unwrap();
            assert_eq!(
                checkpoint.through, boundary,
                "must resume the intended checkpoint"
            );
            assert_eq!(
                checkpoint.state.unwrap().models.owner(),
                owner,
                "owners must not overwrite each other's cache"
            );
            let mut expected = vec![input.id.clone(), own.id.clone()];
            expected.extend(padding.iter().cloned());
            expected.push(delivered.id.clone());
            let mut resumed = MinimalLoop::from_config(None);
            resumed.accept_material(&reader, &delivered, owner).unwrap();
            assert_eq!(
                resumed.parts.collect(),
                expected,
                "warm material for {owner}, cut {cut}"
            );
            let mut cold = MinimalLoop::restore_parts(&reader, delivered.seq, owner).unwrap();
            cold.push(delivered.id.clone());
            assert_eq!(cold, expected, "cold and cached ownership must agree");
            assert!(reader.get(&late.id).unwrap().is_some());
            assert!(resumed.awaiting_model.is_none() && !resumed.round_open());
            if cut == 1 {
                let before = resumed.material_input().unwrap();
                let state = serde_json::to_value(resumed.parts.snapshot().unwrap()).unwrap();
                let digest = state["pages"][0]["digest"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|byte| format!("{:02x}", byte.as_u64().unwrap()))
                    .collect::<String>();
                std::fs::write(
                    root.join(format!("cards-{digest}.json")),
                    b"broken derived page",
                )
                .unwrap();
                assert!(resumed.material_input().is_err());
                log.append(
                    EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"not delivered"})),
                    "ui",
                )
                .unwrap();
                let end = reader.snapshot_end();
                resumed.repair_material_pages(&reader).unwrap();
                assert_eq!(resumed.material_input().unwrap(), before);
                assert_eq!(reader.snapshot_end(), end, "repair must not emit work");
            }
        }
    }
}

#[test]
fn wrong_owner_cache_is_rebuilt_and_live_owner_cannot_change() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("history.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "owner-cache", root, 1024 * 1024).unwrap();
    let input = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"first"})),
            "ui",
        )
        .unwrap();
    let call = request(&mut log, "left", &[&input.id]);
    let result = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[&call.id],
                json!({"status":"ok","text":"left"}),
            ),
            "model",
        )
        .unwrap();
    save_both(&log);
    let reader = log.reader();
    let wrong = reader
        .load_checkpoint::<Checkpoint>(&checkpoint_key("right"), VERSION, result.seq)
        .unwrap()
        .state
        .unwrap();
    reader
        .save_checkpoint(&checkpoint_key("left"), VERSION, result.seq, &wrong)
        .unwrap();
    let delivered = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"next"})),
            "ui",
        )
        .unwrap();
    let mut resumed = MinimalLoop::from_config(None);
    resumed
        .accept_material(&reader, &delivered, "left")
        .unwrap();
    assert_eq!(
        resumed.parts.collect(),
        vec![input.id, result.id, delivered.id.clone()]
    );
    assert!(resumed
        .accept_material(&reader, &delivered, "right")
        .is_err());
    let repaired = reader
        .load_checkpoint::<Checkpoint>(&checkpoint_key("left"), VERSION, result.seq)
        .unwrap()
        .state
        .unwrap();
    assert_eq!(repaired.models.owner(), "left");
}
