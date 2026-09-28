use super::*;
use crate::{EventDraft, EventLog};
use serde_json::json;

fn append(log: &mut EventLog, kind: &str, causes: &[&str], payload: Value) -> crate::EventEnvelope {
    log.append(
        EventDraft::new(kind, causes, payload).with_reason("recovery fixture"),
        "fixture",
    )
    .unwrap()
}

fn request(log: &mut EventLog, purpose: Option<&str>) -> crate::EventEnvelope {
    let mut payload =
        json!({"model":"offline","input":{"parts":[],"fingerprint":"sha256:test"},"tools":[]});
    if let Some(purpose) = purpose {
        payload["purpose"] = json!(purpose);
    }
    append(log, ce::MODEL_CALL_STARTED, &[], payload)
}

#[test]
fn interrupted_roots_clear_forwarded_children_from_legacy_observation_checkpoints() {
    for version in [2, 3] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("interrupted.ledger");
        let mut log =
            EventLog::open_segmented(ce::core_event_decls(), "fixture", root.clone(), 4096)
                .unwrap();
        let first = request(&mut log, Some(CONDENSE_PURPOSE));
        let branch = append(
            &mut log,
            ce::MODEL_CALL_STARTED,
            &[&first.id],
            first.payload.clone(),
        );
        let done = append(
            &mut log,
            ce::INTERRUPTED,
            &[&first.id],
            json!({"by":"restart"}),
        );
        let reader = log.reader();
        let mut state = Recovery::default()
            .read(&reader, done.seq, false, |state| {
                serde_json::to_value(state).unwrap()
            })
            .unwrap();
        let mut stale = ce::PendingCalls::new(ce::MODEL_CALL_STARTED);
        stale.observe(ce::EventRelations {
            id: &branch.id,
            event_type: &branch.event_type,
            causes: &branch.causes,
        });
        state["pending"] = if version == 2 {
            json!({branch.id: null})
        } else {
            serde_json::to_value(stale).unwrap()
        };
        reader
            .save_checkpoint(KEY, version, done.seq, &state)
            .unwrap();
        drop(reader);
        drop(log);
        for _ in 0..2 {
            let log =
                EventLog::open_segmented(ce::core_event_decls(), "fixture", root.clone(), 4096)
                    .unwrap();
            let reader = log.reader();
            assert!(Recovery::default()
                .read(&reader, done.seq, true, |state| state
                    .pending
                    .requests()
                    .is_empty())
                .unwrap());
            assert_eq!(log.len() as u64, done.seq);
            assert!(reader
                .load_checkpoint::<Observations>(KEY, VERSION, done.seq)
                .unwrap()
                .state
                .unwrap()
                .pending
                .requests()
                .is_empty());
        }
    }
}

#[test]
fn recovered_dials_keep_null_presence_partial_profiles_and_current_delivery_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("dials.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "dials", root.clone(), 1).unwrap();
    let config = json!({"modelName":"initial","profile":{"contextWindow":10,"usageFields":{"input":"old","cacheRead":"oldCache"}}});
    let mut live = ContextGate::from_config(Some(&config));
    for payload in [
        json!({"channel":super::super::EFFORT_CHANNEL,"value":null}),
        json!({"channel":MODEL_CHANNEL,"model":"first","contextWindow":100,"nativeCompaction":true,"nativeTarget":null,"usageFields":{"input":"new"}}),
    ] {
        let event = append(&mut log, ce::EXTERNAL_INPUT, &[], payload);
        live.on_dial(&event);
    }
    live.observation(&log.reader(), |_| ()).unwrap();
    let changed = append(
        &mut log,
        ce::EXTERNAL_INPUT,
        &[],
        json!({"channel":MODEL_CHANNEL,"model":"second","usageFields":{}}),
    );
    live.on_dial(&changed);
    let current = append(
        &mut log,
        ce::EXTERNAL_INPUT,
        &[],
        json!({"channel":MODEL_CHANNEL,"model":"not delivered yet","contextWindow":999}),
    );
    drop(log);
    let log = EventLog::open_segmented(ce::core_event_decls(), "dials", root, 1).unwrap();
    let mut resumed = ContextGate::from_config(Some(&config));
    resumed.restore_dials(&log.reader(), current.seq).unwrap();
    assert_eq!(resumed.model_name, live.model_name);
    assert_eq!(resumed.model_name.as_deref(), Some("second"));
    assert_eq!(resumed.context_window, Some(100));
    assert_eq!(resumed.thinking, Some(Value::Null));
    assert_eq!(resumed.native_target, Some(Value::Null));
    assert!(resumed.native_compaction);
    assert_eq!(resumed.usage_input_field, "new");
    assert_eq!(
        resumed.usage_cache_field,
        super::super::DEFAULT_USAGE_CACHE_READ
    );
    resumed.on_dial(&current);
    assert_eq!(resumed.context_window, Some(999));
}

#[test]
fn observation_cache_does_not_mix_same_named_equal_length_readers() {
    let mut a = EventLog::in_memory(ce::core_event_decls(), "same");
    let mut b = EventLog::in_memory(ce::core_event_decls(), "same");
    request(&mut a, Some(CONDENSE_PURPOSE));
    request(&mut b, None);
    let gate = ContextGate::from_config(None);
    assert!(gate.condense_in_flight(&a.reader()).unwrap());
    assert!(!gate.condense_in_flight(&b.reader()).unwrap());
    assert!(gate.condense_in_flight(&a.reader()).unwrap());
}

#[test]
fn context_checkpoint_tail_preserves_epochs_pending_promotions_and_target_summaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("context.ledger");
    let mut declarations = ce::core_event_decls();
    declarations.extend(super::super::manifest().events);
    let mut log =
        EventLog::open_segmented(declarations.clone(), "context", root.clone(), 1).unwrap();
    let main = request(&mut log, None);
    let done = append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&main.id],
        json!({"status":"ok","usage":{"input_tokens":17,"alternate":41}}),
    );
    let old = request(&mut log, Some(CONDENSE_PURPOSE));
    let gate = ContextGate::from_config(None);
    assert!(gate.condense_in_flight(&log.reader()).unwrap());
    assert_eq!(
        gate.last_main_usage(&log.reader(), "input_tokens").unwrap(),
        17
    );
    let plain = append(&mut log, SUMMARY, &[], json!({"covers":[],"text":"plain"}));
    let a = append(
        &mut log,
        SUMMARY,
        &[],
        json!({"covers":[],"text":"a","nativeCompaction":{"model":"a","baseUrl":"endpoint"}}),
    );
    let b = append(
        &mut log,
        SUMMARY,
        &[],
        json!({"covers":[],"text":"b","nativeCompaction":{"model":"b","baseUrl":"endpoint"}}),
    );
    append(
        &mut log,
        DECISION,
        &[],
        json!({"scale":"window","action":"promote","promoted":["one","two"]}),
    );
    append(
        &mut log,
        ce::EXTERNAL_INPUT,
        &[],
        json!({"channel":MODEL_CHANNEL,"value":{}}),
    );
    append(
        &mut log,
        ce::MODEL_CALL_COMPLETED,
        &[&old.id],
        json!({"status":"error"}),
    );
    let new = request(&mut log, Some(CONDENSE_PURPOSE));
    append(
        &mut log,
        ce::INTERRUPTED,
        &[&new.id],
        json!({"by":"restart"}),
    );
    drop(log);
    let log = EventLog::open_segmented(declarations, "context", root, 1).unwrap();
    let resumed = ContextGate::from_config(None);
    assert!(!resumed.condense_in_flight(&log.reader()).unwrap());
    assert!(!resumed.condensation_paused(&log.reader()).unwrap());
    assert_eq!(
        resumed.last_main_usage(&log.reader(), "alternate").unwrap(),
        41
    );
    let actual = resumed
        .observation(&log.reader(), |state| {
            assert_eq!(state.main_completion.as_deref(), Some(done.id.as_str()));
            assert_eq!(state.measured_request.as_deref(), Some(main.id.as_str()));
            assert_eq!(
                state.promoted,
                HashSet::from(["one".to_string(), "two".to_string()])
            );
            assert_eq!(state.latest_summary(false, None), Some(plain.id));
            assert_eq!(
                state.latest_summary(true, Some(&json!({"model":"a","baseUrl":"endpoint"}))),
                Some(a.id)
            );
            assert_eq!(state.latest_summary(true, None), Some(b.id));
            serde_json::to_value(state).unwrap()
        })
        .unwrap();
    let mut cold = Observations::default();
    log.reader()
        .try_visit_header_range(1, log.reader().snapshot_end(), |batch| {
            for header in batch {
                cold.observe(&log.reader(), header)?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(
        serde_json::from_value::<Observations>(actual).unwrap(),
        cold
    );
}
