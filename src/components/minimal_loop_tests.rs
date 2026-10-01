use super::*;
use crate::EventLog;
use std::io::Write;

#[test]
fn checkpoint_tail_matches_cold_material_and_never_restores_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("loop.ledger");
    let mut log =
        EventLog::open_segmented(ce::core_event_decls(), "loop", root.clone(), 1).unwrap();
    let typed = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"typed"})),
            "ui",
        )
        .unwrap();
    let mut live = MinimalLoop::from_config(None);
    live.sync_material(&log.reader(), typed.seq).unwrap();
    let forwarded = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[&typed.id], json!({"text":"expanded"})),
            "gate",
        )
        .unwrap();
    let request = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[&forwarded.id],
                json!({"model":"offline","input":{"parts":[],"fingerprint":"sha256:test"},"tools":[]}),
            ),
            "loop",
        )
        .unwrap();
    let completed = log.append(EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&request.id], json!({"status":"ok","text":"","tool_calls":[{"id":"never-replayed","name":"fixture","arguments":{}}]})), "model").unwrap();
    let tool = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&completed.id],
                json!({"call":"c","tool":"fixture","arguments":{}}),
            ),
            "loop",
        )
        .unwrap();
    let interrupted = log
        .append(
            EventDraft::new(ce::INTERRUPTED, &[&tool.id], json!({"by":"restart"}))
                .with_reason("fixture crash"),
            "core",
        )
        .unwrap();
    let side = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_STARTED,
                &[],
                json!({"model":"offline","input":{"parts":[],"fingerprint":"sha256:test"},"tools":[],"purpose":null}),
            ),
            "gate",
        )
        .unwrap();
    let side_result = log
        .append(
            EventDraft::new(
                ce::MODEL_CALL_COMPLETED,
                &[&side.id],
                json!({"status":"ok","text":"not conversation"}),
            ),
            "model",
        )
        .unwrap();
    live.sync_material(&log.reader(), side_result.seq).unwrap();
    let expected = vec![forwarded.id, completed.id, interrupted.id];
    assert_eq!(live.material.parts.collect(), expected);
    drop(log);
    let log = EventLog::open_segmented(ce::core_event_decls(), "loop", root, 1).unwrap();
    let before = log.reader().snapshot_end();
    let mut resumed = MinimalLoop::from_config(None);
    resumed.sync_material(&log.reader(), before).unwrap();
    assert_eq!(resumed.material.parts.collect(), expected);
    assert_eq!(
        MinimalLoop::restore_parts(&log.reader(), before + 1).unwrap(),
        expected
    );
    assert_eq!(log.reader().snapshot_end(), before);
    assert!(!resumed.awaiting_model && !resumed.round_open());
    assert!(resumed.unseen_inputs.is_empty() && resumed.work_inputs.is_empty());
    assert!(resumed.gathered.is_empty());
    // Duplicate delivery is idempotent for the material projection.
    resumed.sync_material(&log.reader(), before).unwrap();
    assert_eq!(resumed.material.parts.collect(), expected);
}

#[test]
fn recovery_excludes_auxiliary_tool_results_and_interruptions() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "auxiliary-tools");
    let input = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"Continue"})),
            "ui",
        )
        .unwrap();
    let request = log
        .append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[&input.id],
                json!({"call":"panel","tool":"fixture","arguments":{},"purpose":"frontend.test"}),
            ),
            "panel",
        )
        .unwrap();
    log.append(
        EventDraft::new(
            ce::TOOL_EXEC_COMPLETED,
            &[&request.id],
            json!({"call":"panel","status":"ok","result":{}}),
        ),
        "provider",
    )
    .unwrap();
    let request = log.append(EventDraft::new(ce::TOOL_EXEC_STARTED, &[], json!({"call":"cancelled-panel","tool":"fixture","arguments":{},"purpose":"frontend.test"})), "panel").unwrap();
    log.append(
        EventDraft::new(ce::INTERRUPTED, &[&request.id], json!({"by":"restart"})),
        "core",
    )
    .unwrap();
    let before = log.reader().snapshot_end();
    let mut resumed = MinimalLoop::from_config(None);
    resumed.sync_material(&log.reader(), before).unwrap();
    assert_eq!(resumed.material.parts.collect(), vec![input.id.clone()]);
    assert_eq!(
        MinimalLoop::restore_parts(&log.reader(), before + 1).unwrap(),
        vec![input.id]
    );
}

#[test]
fn online_material_does_not_admit_inputs_still_waiting_behind_a_gate() {
    let mut log = EventLog::in_memory(ce::core_event_decls(), "delivery");
    let first = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"first"})),
            "ui",
        )
        .unwrap();
    let mut loop_ = MinimalLoop::from_config(None);
    loop_.accept_material(&log.reader(), &first).unwrap();
    let blocked = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"not yet expanded"})),
            "ui",
        )
        .unwrap();
    // Force a recovery-cache refresh while the original input remains gated.
    for _ in 0..256 {
        log.append(
            EventDraft::new(
                ce::WAKE,
                &[],
                json!({"source":"fixture","summary":"unrouted","body":{}}),
            ),
            "fixture",
        )
        .unwrap();
    }
    let delivered = log
        .append(
            EventDraft::new(
                ce::WAKE,
                &[],
                json!({"source":"fixture","summary":"delivered","body":{}}),
            ),
            "fixture",
        )
        .unwrap();
    loop_.accept_material(&log.reader(), &delivered).unwrap();
    assert_eq!(loop_.parts.collect(), [first.id, delivered.id]);
    assert!(
        loop_.material.parts.collect().contains(&blocked.id),
        "checkpoint describes history, not live delivery"
    );
    let expanded = log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[&blocked.id], json!({"text":"expanded"})),
            "gate",
        )
        .unwrap();
    loop_.accept_material(&log.reader(), &expanded).unwrap();
    assert_eq!(loop_.parts.collect().last(), Some(&expanded.id));
    assert!(!loop_.parts.collect().contains(&blocked.id));
}

#[test]
fn material_projection_cannot_mix_two_readers_with_the_same_stream_and_length() {
    let mut a = EventLog::in_memory(ce::core_event_decls(), "same");
    let mut b = EventLog::in_memory(ce::core_event_decls(), "same");
    let first = a
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"a"})),
            "ui",
        )
        .unwrap();
    let second = b
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"b"})),
            "ui",
        )
        .unwrap();
    let mut loop_ = MinimalLoop::from_config(None);
    loop_.sync_material(&a.reader(), 1).unwrap();
    assert_eq!(loop_.material.parts.collect(), [first.id]);
    loop_.sync_material(&b.reader(), 1).unwrap();
    assert_eq!(loop_.material.parts.collect(), [second.id]);
}

#[test]
fn cold_long_history_restores_exact_material_without_reading_bodies() {
    // Build a long history without per-event fsync. Reopen below exercises the
    // same validated metadata index as a real restart, with a cold body cache.
    let mut source = EventLog::in_memory(ce::core_event_decls(), "restore");
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let mut append = |kind: &str, causes: &[&str], payload: Value| {
        let event = source
            .append(EventDraft::new(kind, causes, payload), "fixture")
            .unwrap();
        serde_json::to_writer(file.as_file_mut(), &event).unwrap();
        file.write_all(b"\n").unwrap();
        event
    };
    let original = append(ce::USER_MESSAGE, &[], json!({"text":"original"}));
    let forwarded = append(
        ce::USER_MESSAGE,
        &[&original.id],
        json!({"text":"expanded"}),
    );
    let mut expected = vec![forwarded.id.clone()];
    for n in 0..2048 {
        let mut payload = json!({"model":"offline", "input":{"parts":[], "fingerprint":"sha256:test"}, "tools":[]});
        // Null still counts as presence, matching the previous restoration.
        if n % 2 == 0 {
            payload["purpose"] = Value::Null;
        }
        let request = append(ce::MODEL_CALL_STARTED, &[], payload);
        let reply = append(
            ce::MODEL_CALL_COMPLETED,
            &[&request.id],
            json!({"status":"ok", "text":"reply"}),
        );
        if n % 2 != 0 {
            expected.push(reply.id);
        }
    }
    let tool = append(
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({"call":"c", "tool":"fixture", "arguments":{}}),
    );
    let result = append(
        ce::TOOL_EXEC_COMPLETED,
        &[&tool.id],
        json!({"call":"c", "status":"ok", "result":"result"}),
    );
    expected.push(result.id);
    let wake = append(
        ce::WAKE,
        &[],
        json!({"source":"fixture", "summary":"wake", "body":{}}),
    );
    expected.push(wake.id);
    let typed = append(ce::USER_MESSAGE, &[], json!({"text":"current"}));
    let delivery = append(
        ce::USER_MESSAGE,
        &[&typed.id],
        json!({"text":"current expanded"}),
    );
    drop(source);
    let log = EventLog::open(
        ce::core_event_decls(),
        "restore",
        Some(file.path().to_owned()),
    )
    .unwrap();
    // Make every cold body unavailable. Identities remain valid, but an
    // accidental body scan fails deterministically, without a timing limit.
    file.as_file().set_len(0).unwrap();
    let reader = log.reader();
    assert_eq!(
        MinimalLoop::restore_parts(&reader, delivery.seq).unwrap(),
        expected
    );
    // Metadata restoration must not fabricate readable material after damage;
    // the adapter's later body lookup still reports the underlying failure.
    assert!(reader.get(&forwarded.id).is_err());
}
