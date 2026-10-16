use super::*;
use crate::background_aliases::{operations::Resolver, Projection};

#[test]
fn delayed_alias_request_never_removes_a_reused_numeric_subscription_on_reopen() {
    for (create, cancel, field, kind) in [
        ("Schedule", "Unschedule", "timer", Kind::Timer),
        ("Watch", "Unwatch", "watch", Kind::Watch),
    ] {
        for warm in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let mut log = open(&temp.path().join("ledger"));
            let old = append(
                &mut log,
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"old","tool":create,"arguments":{"interval_ms":1000,"path":"/fixture"}}),
            );
            append(
                &mut log,
                ce::TOOL_EXEC_COMPLETED,
                &[&old.id],
                json!({"call":"old","status":"ok","result":{(field):1}}),
            );
            let reader = log.reader();
            let aliases = Projection::recover(&reader, reader.snapshot_end()).unwrap();
            let alias = aliases.alias(&aliases.identities()[0]).unwrap();
            let mut request = json!({"call":"stop","tool":cancel,"arguments":{(field):alias}});
            Resolver::default().normalize(&reader, &mut request);
            assert_eq!(request["arguments"][field], 1);
            if warm {
                super::super::recover(&reader, kind).unwrap();
            }
            // While this normalized operation waits upstream, a different
            // incarnation acquires the numeric ID. Its creation is not a cause
            // of the cancellation and must never be removed by that request.
            let new = append(
                &mut log,
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call":"new","tool":create,"arguments":{"interval_ms":1000,"path":"/fixture"}}),
            );
            append(
                &mut log,
                ce::TOOL_EXEC_COMPLETED,
                &[&new.id],
                json!({"call":"new","status":"ok","result":{(field):1}}),
            );
            let stop = append(&mut log, ce::TOOL_EXEC_STARTED, &[], request);
            append(
                &mut log,
                ce::TOOL_EXEC_COMPLETED,
                &[&stop.id],
                json!({"call":"stop","status":"error","error":{"code":"background.invalid_reference","message":"stale","blame":"request","retryable":false}}),
            );
            for _ in 0..2 {
                let recovered = super::super::recover(&reader, kind).unwrap();
                let causes: Vec<_> = recovered
                    .state
                    .live
                    .iter()
                    .map(|s| s.cause.as_str())
                    .collect();
                assert_eq!(causes, vec![new.id.as_str()]);
            }
        }
    }
}
