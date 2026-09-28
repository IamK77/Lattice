//! Restore subscription state by reduction, not by retaining every notification.

mod checkpoint;
pub(super) use checkpoint::{recover, Kind};

use serde_json::Value;
#[cfg(test)]
use {
    crate::{core_events as ce, LogReader},
    std::collections::{HashMap, HashSet},
    std::io,
};

pub(super) struct Subscription {
    pub id: u64,
    pub fired: u64,
    pub arguments: Value,
    pub cause: String,
}

pub(super) struct Restored {
    pub next_id: u64,
    pub live: Vec<Subscription>,
}

#[cfg(test)]
pub(super) fn restore(
    reader: &LogReader,
    start_tool: &str,
    stop_tool: &str,
    id_field: &str,
    eligible: impl Fn(&Value) -> bool,
) -> io::Result<Restored> {
    let mut cancelled = HashSet::new();
    let mut fired: HashMap<u64, u64> = HashMap::new();
    let mut assigned = HashMap::new();
    reader.scan_back_types(
        &[ce::TOOL_EXEC_STARTED, ce::TOOL_EXEC_COMPLETED, ce::WAKE],
        |event, _| {
            match event.event_type.as_str() {
                ce::TOOL_EXEC_STARTED if event.payload["tool"] == stop_tool => {
                    if let Some(id) = event.payload["arguments"][id_field].as_u64() {
                        cancelled.insert(id);
                    }
                }
                ce::TOOL_EXEC_COMPLETED => {
                    if let Some(id) = event.payload["result"][id_field].as_u64() {
                        // Old recovery selected the first completion in ledger order.
                        // Reverse traversal overwrites later answers with the first.
                        for cause in &event.causes {
                            assigned.insert(cause.clone(), id);
                        }
                    }
                }
                ce::WAKE => {
                    if let (Some(id), Some(n)) = (
                        event.payload["body"][id_field].as_u64(),
                        event.payload["body"]["fire"].as_u64(),
                    ) {
                        let high = fired.entry(id).or_default();
                        *high = (*high).max(n);
                    }
                }
                _ => {}
            }
            Ok(None::<()>)
        },
    )?;
    let mut highest = 0u64;
    let mut live = Vec::new();
    reader.scan_back_types(&[ce::TOOL_EXEC_STARTED], |event, _| {
        if event.payload["tool"] == start_tool {
            if let Some(&id) = assigned.get(&event.id) {
                highest = highest.max(id);
                let arguments = &event.payload["arguments"];
                let done = fired.get(&id).copied().unwrap_or(0);
                let limit = arguments["max_fires"].as_u64().unwrap_or(100).max(1);
                if !cancelled.contains(&id) && done < limit && eligible(arguments) {
                    live.push(Subscription {
                        id,
                        fired: done,
                        arguments: arguments.clone(),
                        cause: event.id.clone(),
                    });
                }
            }
        }
        Ok(None::<()>)
    })?;
    let next_id = highest
        .checked_add(1)
        .ok_or_else(|| io::Error::other("subscription identifier space exhausted"))?;
    live.reverse();
    Ok(Restored { next_id, live })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventDraft, EventLog, EventTypeDecl};
    use serde_json::json;

    /// Read-only diagnostic: isolate recovery scans without rearming sources.
    #[test]
    #[ignore = "requires LATTICE_RECOVERY_FIXTURE pointing to a segmented ledger copy"]
    fn offline_recovery_history_scan_cost() {
        let root = std::path::PathBuf::from(
            std::env::var_os("LATTICE_RECOVERY_FIXTURE").expect("set LATTICE_RECOVERY_FIXTURE"),
        );
        let reader = LogReader::segmented_snapshot(&root).unwrap();
        // Retain the pre-checkpoint shell query pattern as a baseline, without
        // emitting settlements or running commands. Keep its scan count visible.
        let began = std::time::Instant::now();
        let mut acks = Vec::new();
        reader
            .scan_back_types(&[ce::TOOL_EXEC_COMPLETED], |event, _| {
                if event.payload["result"]["background"] == true {
                    if let Some(started) = event.causes.first() {
                        acks.push(started.clone());
                    }
                }
                Ok(None::<()>)
            })
            .unwrap();
        eprintln!(
            "shell acknowledgements: {}, scan {:?}",
            acks.len(),
            began.elapsed()
        );
        let began = std::time::Instant::now();
        let mut checked = 0u64;
        let mut unresolved = 0;
        for started in &acks {
            let checked_here = std::cell::Cell::new(0u64);
            if !reader
                .any_header(|event| {
                    checked_here.set(checked_here.get() + 1);
                    event.event_type == ce::WAKE && event.causes.contains(started)
                })
                .unwrap()
            {
                unresolved += 1;
            }
            checked += checked_here.get();
        }
        eprintln!(
            "shell completion queries: {:?}, headers {checked}, unresolved {unresolved}",
            began.elapsed()
        );
        for (start, stop, field) in [
            ("Schedule", "Unschedule", "timer"),
            ("Watch", "Unwatch", "watch"),
        ] {
            let before = reader.memory_stats().unwrap();
            let began = std::time::Instant::now();
            let state = restore(&reader, start, stop, field, |_| true).unwrap();
            eprintln!(
                "{start}: {:?}, next {}, live {}, before {:?}, after {:?}",
                began.elapsed(),
                state.next_id,
                state.live.len(),
                before,
                reader.memory_stats().unwrap(),
            );
        }
    }

    fn log() -> EventLog {
        EventLog::in_memory(
            [ce::TOOL_EXEC_STARTED, ce::TOOL_EXEC_COMPLETED, ce::WAKE]
                .into_iter()
                .map(|kind| EventTypeDecl::new(kind, "fixture"))
                .collect(),
            "subscriptions",
        )
    }
    fn schedule(log: &mut EventLog, id: u64, arguments: Value) {
        let start = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[],
                    json!({"tool": "Schedule", "arguments": arguments}),
                ),
                "timer",
            )
            .unwrap();
        log.append(
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&start.id],
                json!({"result": {"timer": id}}),
            ),
            "timer",
        )
        .unwrap();
    }

    #[test]
    fn repeated_notifications_reduce_to_one_live_subscription_and_highest_count() {
        let mut log = log();
        schedule(
            &mut log,
            7,
            json!({"interval_ms": 1000, "max_fires": 2000, "note": "retain this"}),
        );
        for n in (1..=1000).rev() {
            log.append(
                EventDraft::new(
                    ce::WAKE,
                    &[],
                    json!({"body": {"timer": 7, "fire": n}, "padding": "discard this".repeat(100)}),
                ),
                "timer",
            )
            .unwrap();
        }
        let state = restore(&log.reader(), "Schedule", "Unschedule", "timer", |_| true).unwrap();
        assert_eq!(state.live.len(), 1);
        assert_eq!(state.live[0].id, 7);
        assert_eq!(state.live[0].fired, 1000);
        assert_eq!(state.live[0].arguments["note"], "retain this");
        assert_eq!(state.next_id, 8);
        assert_eq!(
            log.cost().events.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "recovery must not deep-copy notification history"
        );
    }

    #[test]
    fn cancelled_completed_and_ineligible_subscriptions_stay_dead_without_reusing_ids() {
        let mut log = log();
        schedule(&mut log, 1, json!({"interval_ms": 10}));
        schedule(&mut log, 2, json!({"interval_ms": 10, "max_fires": 1}));
        schedule(&mut log, 3, json!({"after_ms": 10}));
        log.append(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"tool": "Unschedule", "arguments": {"timer": 1}}),
            ),
            "timer",
        )
        .unwrap();
        log.append(
            EventDraft::new(ce::WAKE, &[], json!({"body": {"timer": 2, "fire": 1}})),
            "timer",
        )
        .unwrap();
        let state = restore(&log.reader(), "Schedule", "Unschedule", "timer", |args| {
            args["interval_ms"].as_u64().is_some()
        })
        .unwrap();
        assert!(state.live.is_empty());
        assert_eq!(state.next_id, 4);
    }

    #[test]
    fn exhausted_identifiers_fail_before_any_restored_state_is_published() {
        let mut log = log();
        schedule(&mut log, u64::MAX, json!({"interval_ms": 10}));
        assert!(restore(&log.reader(), "Schedule", "Unschedule", "timer", |_| true).is_err());
    }
}
