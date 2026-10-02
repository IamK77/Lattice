//! Resolve work provenance, never arbitrary conversational ancestry.
//! Model material is history; `workInputs` names only inputs consumed by the
//! active work. Authorization answers and cross-stream origins confer nothing.

use std::collections::{BTreeSet, HashSet};
use std::io;

use serde::Serialize;

use crate::{core_events as ce, EventEnvelope, LogReader};

use super::current_snapshot;

#[derive(Clone, Debug, Serialize)]
pub struct PermissionEvidence {
    pub state: String,
    pub interfaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Origin {
    interface: String,
    controller: String,
}

pub fn allowance(
    reader: &LogReader,
    request: &EventEnvelope,
    source: &str,
) -> io::Result<Option<PermissionEvidence>> {
    let snapshot = current_snapshot(reader, source)?;
    let Some((id, state)) = snapshot else {
        return Ok(None);
    };
    if !state
        .interfaces
        .values()
        .any(|interface| interface.open && interface.enabled)
    {
        return Ok(None);
    }
    let origins = origins(request, |id| reader.get(id))?;
    let interfaces = origins
        .into_iter()
        .filter(|origin| {
            state
                .interfaces
                .get(&origin.interface)
                .is_some_and(|interface| {
                    interface.owner == origin.controller && state.permits(&origin.interface)
                })
        })
        .map(|origin| origin.interface)
        .collect::<Vec<_>>();
    Ok((!interfaces.is_empty()).then_some(PermissionEvidence {
        state: id,
        interfaces,
    }))
}

fn origins(
    request: &EventEnvelope,
    mut get: impl FnMut(&str) -> io::Result<Option<EventEnvelope>>,
) -> io::Result<BTreeSet<Origin>> {
    let mut pending = vec![request.clone()];
    let mut seen = HashSet::new();
    let mut origins = BTreeSet::new();
    while let Some(event) = pending.pop() {
        if !seen.insert(event.id.clone()) {
            continue;
        }
        let read = |id: &str, get: &mut dyn FnMut(&str) -> io::Result<Option<EventEnvelope>>| {
            get(id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("missing authorization provenance event {id}"),
                )
            })
        };
        if event.event_type == ce::USER_MESSAGE
            || (event.event_type == ce::EXTERNAL_INPUT && event.payload["workInput"] == true)
        {
            if let Some(interface) = event.payload["interface"].as_str() {
                origins.insert(Origin {
                    interface: interface.into(),
                    controller: event.source.clone(),
                });
            }
        }
        if event.event_type == ce::MODEL_CALL_STARTED {
            if let Some(inputs) = event.payload.get("workInputs") {
                let inputs: Vec<String> = serde_json::from_value(inputs.clone())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                for id in inputs {
                    pending.push(read(&id, &mut get)?);
                }
                // Stop here even for an empty set: causes and input.parts are
                // not fallback sources for a request with explicit provenance.
                continue;
            }
        }
        for id in &event.causes {
            let parent = read(id, &mut get)?;
            let follows = match event.event_type.as_str() {
                ce::USER_MESSAGE => parent.event_type == ce::USER_MESSAGE,
                ce::MODEL_CALL_STARTED => parent.event_type == ce::MODEL_CALL_STARTED,
                ce::MODEL_CALL_COMPLETED => parent.event_type == ce::MODEL_CALL_STARTED,
                ce::TOOL_EXEC_STARTED => {
                    (parent.event_type == ce::TOOL_EXEC_STARTED
                        && parent.payload["call"] == event.payload["call"]
                        && parent.payload["tool"] == event.payload["tool"])
                        || parent.event_type == ce::MODEL_CALL_COMPLETED
                        || (parent.event_type == ce::EXTERNAL_INPUT
                            && parent.payload["workInput"] == true)
                }
                ce::TOOL_EXEC_COMPLETED => {
                    parent.event_type == ce::TOOL_EXEC_STARTED
                        && parent.payload["call"] == event.payload["call"]
                }
                ce::WAKE => matches!(parent.event_type.as_str(), ce::TOOL_EXEC_STARTED | ce::WAKE),
                _ => false,
            };
            if follows {
                pending.push(parent);
            }
        }
    }
    Ok(origins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    fn event(id: &str, kind: &str, source: &str, causes: &[&str], payload: Value) -> EventEnvelope {
        EventEnvelope {
            v: 1,
            id: id.into(),
            seq: 1,
            stream: "flow".into(),
            time: String::new(),
            event_type: kind.into(),
            source: source.into(),
            causes: causes.iter().map(|s| (*s).into()).collect(),
            origin: None,
            reason: None,
            payload,
        }
    }

    fn resolve(events: Vec<EventEnvelope>, request: &EventEnvelope) -> BTreeSet<String> {
        let events: HashMap<_, _> = events.into_iter().map(|e| (e.id.clone(), e)).collect();
        origins(request, |id| Ok(events.get(id).cloned()))
            .unwrap()
            .into_iter()
            .map(|origin| origin.interface)
            .collect()
    }

    #[test]
    fn material_and_old_completed_work_do_not_supply_permission() {
        let a = event("a", ce::USER_MESSAGE, "ui", &[], json!({"interface":"a"}));
        let b = event("b", ce::USER_MESSAGE, "ui", &[], json!({"interface":"b"}));
        let q1 = event(
            "q1",
            ce::MODEL_CALL_STARTED,
            "loop",
            &["a"],
            json!({"workInputs":["a"]}),
        );
        let r1 = event("r1", ce::MODEL_CALL_COMPLETED, "model", &["q1"], json!({}));
        let q2 = event(
            "q2",
            ce::MODEL_CALL_STARTED,
            "loop",
            &["r1", "b"],
            json!({"workInputs":["b"], "input":{"parts":["a","b"]}}),
        );
        assert_eq!(
            resolve(vec![a, b, q1, r1], &q2),
            BTreeSet::from(["b".into()])
        );
    }

    #[test]
    fn merged_work_sources_survive_request_forwarding() {
        let a = event("a", ce::USER_MESSAGE, "ui", &[], json!({"interface":"a"}));
        let b = event("b", ce::USER_MESSAGE, "ui", &[], json!({"interface":"b"}));
        let q = event(
            "q",
            ce::MODEL_CALL_STARTED,
            "loop",
            &["a", "b"],
            json!({"workInputs":["a","b"]}),
        );
        let r = event("r", ce::MODEL_CALL_COMPLETED, "model", &["q"], json!({}));
        let call = json!({"call":"t", "tool":"Run"});
        let t = event("t", ce::TOOL_EXEC_STARTED, "loop", &["r"], call.clone());
        let answer = event(
            "answer",
            ce::EXTERNAL_INPUT,
            "ui",
            &[],
            json!({"interface":"observer","approve":true}),
        );
        let copy = event(
            "copy",
            ce::TOOL_EXEC_STARTED,
            "trust",
            &["t", "answer"],
            call,
        );
        assert_eq!(
            resolve(vec![a, b, q, r, t, answer], &copy),
            BTreeSet::from(["a".into(), "b".into()])
        );
    }

    #[test]
    fn background_wake_follows_its_creator_not_recent_speakers() {
        let a = event("a", ce::USER_MESSAGE, "ui", &[], json!({"interface":"a"}));
        let q = event(
            "q",
            ce::MODEL_CALL_STARTED,
            "loop",
            &["a"],
            json!({"workInputs":["a"]}),
        );
        let r = event("r", ce::MODEL_CALL_COMPLETED, "model", &["q"], json!({}));
        let t = event(
            "t",
            ce::TOOL_EXEC_STARTED,
            "loop",
            &["r"],
            json!({"call":"t","tool":"Schedule"}),
        );
        let wake = event("wake", ce::WAKE, "timer", &["t"], json!({}));
        let q2 = event(
            "q2",
            ce::MODEL_CALL_STARTED,
            "loop",
            &["wake"],
            json!({"workInputs":["wake"]}),
        );
        assert_eq!(
            resolve(vec![a, q, r, t, wake], &q2),
            BTreeSet::from(["a".into()])
        );
    }

    #[test]
    fn legacy_requests_and_cross_stream_origins_do_not_guess_permission() {
        let a = event("a", ce::USER_MESSAGE, "ui", &[], json!({"interface":"a"}));
        let legacy = event("legacy", ce::MODEL_CALL_STARTED, "loop", &["a"], json!({}));
        assert!(resolve(vec![a], &legacy).is_empty());
        let mut child = event("child", ce::USER_MESSAGE, "ui", &[], json!({"text":"work"}));
        child.origin = Some(crate::contracts::event::StreamRef {
            stream: "parent".into(),
            event: "a".into(),
        });
        assert!(resolve(vec![], &child).is_empty());
    }

    #[test]
    fn unreadable_provenance_is_an_error_not_an_absent_event() {
        let q = event(
            "q",
            ce::MODEL_CALL_STARTED,
            "loop",
            &[],
            json!({"workInputs":["missing"]}),
        );
        assert!(origins(&q, |_| Ok(None)).is_err());
        assert!(origins(&q, |_| Err(io::Error::other("damaged history"))).is_err());
    }
}
