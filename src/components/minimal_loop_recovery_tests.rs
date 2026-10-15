use super::*;
use crate::{EventDraft, EventLog};
use serde_json::json;

#[test]
fn checkpoint_boundaries_and_page_repair_keep_only_the_first_model_outcome() {
    for checkpoint_before_outcome in [false, true] {
        for interrupted_first in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("history.ledger");
            let mut log =
                EventLog::open_segmented(ce::core_event_decls(), "settlement", root.clone(), 1)
                    .unwrap();
            let mut expected = Vec::new();
            // One matrix case also repairs a published page; the other cases
            // need only a small prefix to test checkpoint settlement state.
            let repair_pages = checkpoint_before_outcome && interrupted_first;
            let prior_inputs = if repair_pages { 140 } else { 1 };
            for index in 0..prior_inputs {
                let input = log
                    .append(
                        EventDraft::new(
                            ce::USER_MESSAGE,
                            &[],
                            json!({"text":format!("input {index}")}),
                        ),
                        "ui",
                    )
                    .unwrap();
                expected.push(input.id);
            }
            let request = log
                .append(
                    EventDraft::new(
                        ce::MODEL_CALL_STARTED,
                        &[expected.last().unwrap()],
                        json!({"model":"fixture","input":{"parts":[],"fingerprint":"sha256:test"}}),
                    ),
                    "loop",
                )
                .unwrap();
            let forwarded = log
                .append(
                    EventDraft::new(
                        ce::MODEL_CALL_STARTED,
                        &[&request.id],
                        request.payload.clone(),
                    ),
                    "gate",
                )
                .unwrap();
            let mut original = MinimalLoop::from_config(None);
            if checkpoint_before_outcome {
                original
                    .sync_material(&log.reader(), forwarded.seq)
                    .unwrap();
            }
            let first = if interrupted_first {
                log.append(
                    EventDraft::new(ce::INTERRUPTED, &[&request.id], json!({"by":"deadline"})),
                    "core",
                )
                .unwrap()
            } else {
                log.append(
                    EventDraft::new(
                        ce::MODEL_CALL_COMPLETED,
                        &[&forwarded.id],
                        json!({"status":"ok","text":"first"}),
                    ),
                    "model",
                )
                .unwrap()
            };
            expected.push(first.id.clone());
            if !checkpoint_before_outcome {
                original.sync_material(&log.reader(), first.seq).unwrap();
            }
            let checkpoint = log
                .reader()
                .load_checkpoint::<Checkpoint>(KEY, VERSION, first.seq)
                .unwrap();
            assert!(
                checkpoint.state.is_some(),
                "this must exercise a saved checkpoint"
            );
            let late = log.append(EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&forwarded.id],
                json!({"status":"ok","toolCalls":[{"id":"late","tool":"fixture","arguments":{}}]})), "model").unwrap();
            let duplicate_interrupt = log
                .append(
                    EventDraft::new(ce::INTERRUPTED, &[&request.id], json!({"by":"late"})),
                    "core",
                )
                .unwrap();
            let input = log
                .append(
                    EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"new"})),
                    "ui",
                )
                .unwrap();
            expected.push(input.id.clone());
            let reader = log.reader();
            let mut resumed = MinimalLoop::from_config(None);
            resumed.accept_material(&reader, &input).unwrap();
            assert_eq!(resumed.parts.collect(), expected);
            let mut cold = Material::default();
            cold.advance(&reader, 1, input.seq).unwrap();
            assert_eq!(cold.parts.collect(), expected);
            assert!(resumed.awaiting_model.is_none() && !resumed.round_open());
            assert!(resumed.gathered.is_empty());
            assert!(reader.get(&late.id).unwrap().is_some());
            assert!(reader.get(&duplicate_interrupt.id).unwrap().is_some());

            if !repair_pages {
                continue;
            }
            let before_repair = resumed.material_input().unwrap();
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
            resumed.repair_material_pages(&reader).unwrap();
            assert_eq!(resumed.material_input().unwrap(), before_repair);
            assert_eq!(resumed.parts.collect(), expected);
        }
    }
}

#[test]
fn old_pointer_only_checkpoint_is_rebuilt_instead_of_losing_settlement_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("history.ledger");
    let mut log = EventLog::open_segmented(ce::core_event_decls(), "migration", root, 1).unwrap();
    let input = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"first"})),
            "ui",
        )
        .unwrap();
    let request = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[&input.id],
                json!({"model":"fixture","input":{"parts":[],"fingerprint":"sha256:test"}}),
            ),
            "loop",
        )
        .unwrap();
    let first = log
        .append(
            EventDraft::new(ce::INTERRUPTED, &[&request.id], json!({"by":"deadline"})),
            "core",
        )
        .unwrap();
    let late = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[&request.id],
                json!({"status":"ok","text":"duplicate"}),
            ),
            "model",
        )
        .unwrap();
    let reader = log.reader();
    let mut legacy = material_store::Store::open(&reader, Default::default()).unwrap();
    for event in [&input, &first, &late] {
        legacy
            .push(event.id.clone(), event.event_type == ce::USER_MESSAGE)
            .unwrap();
    }
    reader
        .save_checkpoint(KEY, 3, late.seq, &legacy.snapshot().unwrap())
        .unwrap();
    let mut resumed = MinimalLoop::from_config(None);
    resumed.sync_material(&reader, late.seq).unwrap();
    assert_eq!(resumed.material.parts.collect(), vec![input.id, first.id]);
    assert!(reader.get(&late.id).unwrap().is_some());
    assert!(resumed.awaiting_model.is_none());
}
