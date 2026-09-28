use lattice::contracts::core_events as ce;
use lattice::contracts::event::{EventEnvelope, ENVELOPE_VERSION};
use std::collections::HashSet;

fn event(id: usize, kind: &str, causes: &[usize]) -> EventEnvelope {
    EventEnvelope {
        v: ENVELOPE_VERSION,
        id: format!("e{id}"),
        seq: id as u64 + 1,
        stream: "test".into(),
        time: "2026-09-12T00:00:00Z".into(),
        event_type: kind.into(),
        source: "test".into(),
        causes: causes.iter().map(|id| format!("e{id}")).collect(),
        origin: None,
        reason: None,
        payload: serde_json::Value::Null,
    }
}

// Independent slow oracle: repeatedly expand sets over the full live graph,
// rather than deleting nodes through the production observer's work queues.
fn reference(events: &[EventEnvelope], kind: &str) -> Vec<String> {
    let mut ordered: Vec<_> = events.iter().collect();
    ordered.sort_by_key(|event| event.seq);
    let mut live: Vec<&EventEnvelope> = Vec::new();
    for event in ordered {
        if event.event_type == kind {
            live.push(event);
        } else if ce::is_outcome(&event.event_type) {
            let targets: HashSet<_> = event
                .causes
                .iter()
                .map(String::as_str)
                .filter(|id| live.iter().any(|node| node.id == *id))
                .collect();
            let mut ancestors = targets.clone();
            let mut descendants = targets;
            loop {
                let before = ancestors.len();
                for node in &live {
                    if ancestors.contains(node.id.as_str()) {
                        for parent in &node.causes {
                            if live.iter().any(|node| node.id == *parent) {
                                ancestors.insert(parent.as_str());
                            }
                        }
                    }
                }
                if ancestors.len() == before {
                    break;
                }
            }
            if event.event_type == ce::INTERRUPTED {
                loop {
                    let before = descendants.len();
                    for node in &live {
                        if node
                            .causes
                            .iter()
                            .any(|parent| descendants.contains(parent.as_str()))
                        {
                            descendants.insert(node.id.as_str());
                        }
                    }
                    if descendants.len() == before {
                        break;
                    }
                }
            }
            live.retain(|node| {
                !ancestors.contains(node.id.as_str()) && !descendants.contains(node.id.as_str())
            });
        }
    }
    live.iter()
        .filter(|node| {
            !node.causes.iter().any(|parent| {
                live.iter()
                    .any(|other| other.id != node.id && other.id == *parent)
            })
        })
        .map(|node| node.id.clone())
        .collect()
}

#[test]
fn chain_heads_match_reference_for_every_small_graph_and_ending_set() {
    // Include cycles and self-edges even though valid ledgers are acyclic.
    // This also checks termination and preserves the old defensive behavior.
    for graph in 0..512usize {
        for endings in 0..8usize {
            let mut events: Vec<_> = (0..3)
                .map(|child| {
                    let parents: Vec<_> = (0..3)
                        .filter(|parent| graph & (1 << (child * 3 + parent)) != 0)
                        .collect();
                    event(child, ce::TOOL_EXEC_STARTED, &parents)
                })
                .collect();
            for (node, kind) in [
                ce::TOOL_EXEC_COMPLETED,
                ce::MODEL_CALL_COMPLETED,
                ce::INTERRUPTED,
            ]
            .iter()
            .enumerate()
            {
                if endings & (1 << node) != 0 {
                    events.push(event(10 + node, kind, &[node]));
                }
            }
            assert_eq!(
                ce::hanging_chain_heads(&events, ce::TOOL_EXEC_STARTED),
                reference(&events, ce::TOOL_EXEC_STARTED),
                "graph={graph}, endings={endings}"
            );
        }
    }
}

#[test]
fn head_interruption_does_not_apply_to_later_requests_even_in_shuffled_input() {
    let mut events = vec![
        event(0, ce::TOOL_EXEC_STARTED, &[]),
        event(1, ce::TOOL_EXEC_STARTED, &[0]),
        event(2, ce::INTERRUPTED, &[0]),
        event(3, ce::TOOL_EXEC_STARTED, &[0]),
    ];
    for _ in 0..events.len() {
        events.rotate_left(1);
        assert_eq!(
            ce::hanging_chain_heads(&events, ce::TOOL_EXEC_STARTED),
            ["e3"]
        );
        assert_eq!(
            ce::hanging_chain_heads(&events, ce::TOOL_EXEC_STARTED),
            reference(&events, ce::TOOL_EXEC_STARTED)
        );
    }
}

#[test]
fn settled_parent_does_not_hide_its_unfinished_branch() {
    let events = vec![
        event(0, ce::MODEL_CALL_STARTED, &[]),
        event(1, ce::MODEL_CALL_STARTED, &[0]),
        event(2, ce::MODEL_CALL_STARTED, &[0]),
        event(3, ce::INTERRUPTED, &[1]),
    ];
    assert_eq!(
        ce::hanging_chain_heads(&events, ce::MODEL_CALL_STARTED),
        ["e2"]
    );
}

#[test]
fn joined_copies_and_multi_cause_outcomes_settle_every_parent() {
    let events = vec![
        event(0, ce::TOOL_EXEC_STARTED, &[]),
        event(1, ce::TOOL_EXEC_STARTED, &[]),
        event(2, ce::TOOL_EXEC_STARTED, &[]),
        event(3, ce::TOOL_EXEC_STARTED, &[0, 1]),
        event(4, ce::INTERRUPTED, &[3, 2]),
    ];
    assert!(ce::hanging_chain_heads(&events, ce::TOOL_EXEC_STARTED).is_empty());
    assert!(ce::hanging_chain_heads(&[], ce::TOOL_EXEC_STARTED).is_empty());
}

#[test]
fn copy_walk_stops_at_another_event_type() {
    let events = vec![
        event(0, ce::TOOL_EXEC_STARTED, &[]),
        event(1, ce::MODEL_CALL_STARTED, &[0]),
        event(2, ce::TOOL_EXEC_STARTED, &[1]),
        event(3, ce::TOOL_EXEC_COMPLETED, &[2]),
    ];
    assert_eq!(
        ce::hanging_chain_heads(&events, ce::TOOL_EXEC_STARTED),
        ["e0"]
    );
    assert_eq!(
        ce::hanging_chain_heads(&events, ce::MODEL_CALL_STARTED),
        ["e1"]
    );
}
