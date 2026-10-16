use super::*;
use crate::components::subscription_history::cancellation::{self, LiveCancellations};

fn create(log: &mut EventLog, kind: Kind, id: u64) {
    let (tool, _, field) = kind.tools();
    let request = append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({
            "call":"start","tool":tool,"arguments":{"interval_ms":1000,"max_fires":5,"path":"/fixture"}
        }),
    );
    append(
        log,
        ce::TOOL_EXEC_COMPLETED,
        &[&request.id],
        json!({
            "call":"start","status":"ok","result":{(field):id}
        }),
    );
}

fn stop(log: &mut EventLog, kind: Kind, id: u64) -> EventEnvelope {
    let (_, tool, field) = kind.tools();
    append(
        log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({
            "call":"stop","tool":tool,"arguments":{(field):id}
        }),
    )
}

fn activate(log: &mut EventLog, kind: Kind, legacy_through: u64) {
    log.append(cancellation::activation(kind, legacy_through), "fixture")
        .unwrap();
}

fn success(log: &mut EventLog, kind: Kind, request: &EventEnvelope, id: u64) {
    let mut payload = cancellation::receipt(kind, id);
    payload["call"] = json!("stop");
    append(log, ce::TOOL_EXEC_COMPLETED, &[&request.id], payload);
}

fn recovered(log: &EventLog, kind: Kind) -> Recovery {
    crate::components::subscription_history::recover(&log.reader(), kind).unwrap()
}

#[test]
fn modern_denied_interrupted_and_unanswered_requests_do_not_remove_subscriptions() {
    for kind in [Kind::Timer, Kind::Watch] {
        for outcome in ["missing", "denied", "interrupted"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("ledger");
            let mut log = open(&root);
            activate(&mut log, kind, 0);
            create(&mut log, kind, 1);
            assert_eq!(recovered(&log, kind).state.live.len(), 1);
            let request = stop(&mut log, kind, 1);
            match outcome {
                "denied" => {
                    append(
                        &mut log,
                        ce::TOOL_EXEC_COMPLETED,
                        &[&request.id],
                        json!({
                            "call":"stop","status":"error","error":{
                                "code":"policy.denied","message":"denied","blame":"request"
                            }
                        }),
                    );
                }
                "interrupted" => {
                    log.append(
                        EventDraft::new(ce::INTERRUPTED, &[&request.id], json!({"by":"restart"}))
                            .with_reason("fixture interruption"),
                        "core",
                    )
                    .unwrap();
                }
                _ => {}
            }
            assert_eq!(recovered(&log, kind).state.live.len(), 1, "{outcome}");
            drop(log);
            let log = open(&root);
            assert_eq!(
                recovered(&log, kind).state.live.len(),
                1,
                "reopened {outcome}"
            );
        }
    }
}

#[test]
fn committed_cancellation_survives_without_worker_observation_and_preserves_future_ids() {
    for kind in [Kind::Timer, Kind::Watch] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ledger");
        let mut log = open(&root);
        activate(&mut log, kind, 0);
        create(&mut log, kind, 1);
        let current = stop(&mut log, kind, 1);
        success(&mut log, kind, &current, 1);
        let future = stop(&mut log, kind, 999);
        success(&mut log, kind, &future, 999);
        drop(log);
        let log = open(&root);
        for _ in 0..2 {
            let restored = recovered(&log, kind);
            assert!(restored.state.live.is_empty());
            assert!(restored.state.future_cancelled.contains(&999));
            let live = LiveCancellations::new(
                log.reader(),
                kind,
                restored.through,
                restored.state.protocol,
                restored.state.future_cancelled,
            );
            assert!(live.stopped(999).unwrap());
        }
    }
}

#[test]
fn first_upgrade_keeps_old_request_semantics_but_not_new_requests_before_activation_delivery() {
    for kind in [Kind::Timer, Kind::Watch] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ledger");
        let mut log = open(&root);
        create(&mut log, kind, 1);
        stop(&mut log, kind, 1);
        stop(&mut log, kind, 999);
        let old = recovered(&log, kind);
        assert!(old.state.live.is_empty());
        assert!(old.state.future_cancelled.contains(&999));
        let cutoff = old.through;
        create(&mut log, kind, 2);
        stop(&mut log, kind, 2);
        activate(&mut log, kind, cutoff);
        let new = recovered(&log, kind);
        assert_eq!(new.state.live.len(), 1);
        assert_eq!(new.state.live[0].id, 2);
        assert_eq!(new.state.protocol.legacy_through, Some(cutoff));
        assert!(new.state.future_cancelled.contains(&999));
        drop(log);
        let log = open(&root);
        let warm = recovered(&log, kind);
        assert_eq!(warm.state.live.len(), 1);
        assert_eq!(warm.headers, 0);
        let cache = root.join(format!(
            "checkpoint-{:x}.json",
            Sha256::digest(kind.consumer())
        ));
        std::fs::remove_file(cache).unwrap();
        let cold = recovered(&log, kind);
        assert_eq!(cold.state.live.len(), 1);
        assert_eq!(cold.state.live[0].id, 2);
    }
}
