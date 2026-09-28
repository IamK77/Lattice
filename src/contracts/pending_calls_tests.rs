use super::*;
use std::collections::HashSet;

struct Row {
    id: String,
    kind: &'static str,
    causes: Vec<String>,
}

fn oracle(rows: &[Row]) -> Vec<String> {
    let starts: Vec<_> = rows
        .iter()
        .filter(|row| row.kind == TOOL_EXEC_STARTED)
        .collect();
    let mut settled: HashSet<&str> = rows
        .iter()
        .filter(|row| matches!(row.kind, TOOL_EXEC_COMPLETED | INTERRUPTED))
        .flat_map(|row| row.causes.iter().map(String::as_str))
        .filter(|id| starts.iter().any(|start| start.id == *id))
        .collect();
    loop {
        let before = settled.len();
        for start in &starts {
            if settled.contains(start.id.as_str()) {
                for cause in &start.causes {
                    if starts.iter().any(|parent| parent.id == *cause) {
                        settled.insert(cause);
                    }
                }
            }
        }
        if settled.len() == before {
            break;
        }
    }
    starts
        .iter()
        .filter(|start| {
            !settled.contains(start.id.as_str())
                && !start.causes.iter().any(|cause| {
                    starts.iter().any(|parent| parent.id == *cause)
                        && !settled.contains(cause.as_str())
                })
        })
        .map(|start| start.id.clone())
        .collect()
}

#[test]
fn pending_projection_matches_every_small_dag_prefix_and_survives_checkpoints() {
    // All DAGs on four ordered requests, and every immediate-outcome subset.
    // Outcomes interleave with starts, so a later branch can cite a settled parent.
    for edges in 0..64 {
        for endings in 0..16 {
            let mut rows = Vec::new();
            let mut edge = 0;
            for node in 0..4 {
                let mut causes = Vec::new();
                for parent in 0..node {
                    if edges & (1 << edge) != 0 {
                        causes.push(format!("request-{parent}"));
                    }
                    edge += 1;
                }
                rows.push(Row {
                    id: format!("request-{node}"),
                    kind: TOOL_EXEC_STARTED,
                    causes,
                });
                if endings & (1 << node) != 0 {
                    rows.push(Row {
                        id: format!("outcome-{node}"),
                        kind: if node % 2 == 0 {
                            TOOL_EXEC_COMPLETED
                        } else {
                            INTERRUPTED
                        },
                        causes: vec![format!("request-{node}")],
                    });
                }
            }
            let mut pending = PendingCalls::new(TOOL_EXEC_STARTED);
            for (at, row) in rows.iter().enumerate() {
                pending.observe(EventRelations {
                    id: &row.id,
                    event_type: row.kind,
                    causes: &row.causes,
                });
                assert_eq!(
                    pending.heads(),
                    oracle(&rows[..=at]),
                    "edges={edges} endings={endings} prefix={at}"
                );
                pending = serde_json::from_slice(&serde_json::to_vec(&pending).unwrap()).unwrap();
                assert_eq!(pending.heads(), oracle(&rows[..=at]));
            }
        }
    }
}

#[test]
fn interrupting_a_head_settles_existing_forwards_without_crossing_non_requests() {
    for kind in [TOOL_EXEC_STARTED, MODEL_CALL_STARTED] {
        let mut pending = PendingCalls::new(kind);
        for (id, event_type, causes) in [
            ("root", kind, vec![]),
            ("branch", kind, vec!["root".into()]),
            ("leaf", kind, vec!["branch".into()]),
            ("sibling", kind, vec!["root".into()]),
            ("bridge", USER_MESSAGE, vec!["root".into()]),
            ("independent", kind, vec!["bridge".into()]),
        ] {
            pending.observe(EventRelations {
                id,
                event_type,
                causes: &causes,
            });
            pending = serde_json::from_slice(&serde_json::to_vec(&pending).unwrap()).unwrap();
        }
        let before = pending.clone();
        pending.observe(EventRelations {
            id: "interrupted",
            event_type: INTERRUPTED,
            causes: &["root".into()],
        });
        assert_eq!(pending.requests(), vec!["independent"]);
        assert_eq!(pending.heads(), vec!["independent"]);
        let mut branch_only = before;
        branch_only.observe(EventRelations {
            id: "interrupted",
            event_type: INTERRUPTED,
            causes: &["branch".into()],
        });
        assert_eq!(branch_only.requests(), vec!["sibling", "independent"]);
        assert_eq!(branch_only.heads(), vec!["sibling", "independent"]);
    }
}

#[test]
fn interruption_does_not_walk_back_from_cancelled_descendants() {
    let mut pending = PendingCalls::new(TOOL_EXEC_STARTED);
    for (id, causes) in [
        ("a", vec![]),
        ("b", vec![]),
        ("joined", vec!["a".into(), "b".into()]),
        ("other", vec!["b".into()]),
    ] {
        pending.observe(EventRelations {
            id,
            event_type: TOOL_EXEC_STARTED,
            causes: &causes,
        });
    }
    for order in [["a", "b"], ["b", "a"]] {
        let mut state = pending.clone();
        for target in order {
            state.observe(EventRelations {
                id: "interrupted",
                event_type: INTERRUPTED,
                causes: &[target.into()],
            });
            assert!(!state.requests().contains(&target.to_string()));
            state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        }
        assert!(state.requests().is_empty());
    }
    pending.observe(EventRelations {
        id: "interrupted",
        event_type: INTERRUPTED,
        causes: &["a".into()],
    });
    assert_eq!(pending.requests(), vec!["b", "other"]);
    pending.observe(EventRelations {
        id: "unrelated",
        event_type: INTERRUPTED,
        causes: &["a".into(), "missing".into()],
    });
    assert_eq!(pending.requests(), vec!["b", "other"]);
}

#[test]
fn settlement_stops_at_a_non_request_and_drops_completed_nodes() {
    let mut pending = PendingCalls::new(TOOL_EXEC_STARTED);
    pending.observe(EventRelations {
        id: "a",
        event_type: TOOL_EXEC_STARTED,
        causes: &[],
    });
    pending.observe(EventRelations {
        id: "bridge",
        event_type: MODEL_CALL_STARTED,
        causes: &["a".into()],
    });
    pending.observe(EventRelations {
        id: "b",
        event_type: TOOL_EXEC_STARTED,
        causes: &["bridge".into()],
    });
    pending.observe(EventRelations {
        id: "done",
        event_type: TOOL_EXEC_COMPLETED,
        causes: &["b".into()],
    });
    assert_eq!(pending.heads(), vec!["a"]);
    assert_eq!(pending.pending.len(), 1);
    pending.observe(EventRelations {
        id: "interrupted",
        event_type: INTERRUPTED,
        causes: &["a".into()],
    });
    assert!(pending.pending.is_empty());
}
