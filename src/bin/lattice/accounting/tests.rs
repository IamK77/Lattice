use super::*;

#[test]
fn growth_separates_what_is_there_from_what_this_turn_added() {
    let mut g = Growth::default();
    g.absorb(&[("model replies", 100), ("tool results", 50)]);
    assert_eq!(
        Growth::sorted(&g.turn),
        vec![("model replies", 100), ("tool results", 50)]
    );
    g.absorb(&[("model replies", 100), ("tool results", 950)]);
    let turn: HashMap<_, _> = Growth::sorted(&g.turn).into_iter().collect();
    assert_eq!(turn["model replies"], 100, "not counted twice");
    assert_eq!(turn["tool results"], 950, "50 then 900 more");
    g.new_turn();
    g.absorb(&[("model replies", 300), ("tool results", 950)]);
    let turn: HashMap<_, _> = Growth::sorted(&g.turn).into_iter().collect();
    let ever: HashMap<_, _> = Growth::sorted(&g.session).into_iter().collect();
    assert_eq!(turn.get("tool results"), None, "this turn added no results");
    assert_eq!(turn["model replies"], 200, "only what this turn added");
    assert_eq!(ever["model replies"], 300, "the conversation has seen 300");
    assert_eq!(ever["tool results"], 950);
    g.absorb(&[("model replies", 300), ("condensed summaries", 20)]);
    let ever: HashMap<_, _> = Growth::sorted(&g.session).into_iter().collect();
    assert_eq!(
        ever["tool results"], 950,
        "dropped from the window, not from what it cost"
    );
}

#[test]
fn a_kind_absent_from_one_snapshot_is_not_counted_again_when_it_returns() {
    let mut g = Growth::default();
    g.absorb(&[("tool results", 500), ("model replies", 100)]);
    g.absorb(&[("model replies", 100)]);
    g.absorb(&[("tool results", 500), ("model replies", 100)]);
    let ever: HashMap<_, _> = Growth::sorted(&g.session).into_iter().collect();
    assert_eq!(
        ever["tool results"], 500,
        "counted once, not once per disappearance"
    );
}

#[test]
fn the_preamble_is_never_reported_as_accumulating() {
    assert!(!accumulates("system prompt"));
    assert!(!accumulates("tool declarations"));
    assert!(accumulates("tool results") && accumulates("model replies"));
    let mut g = Growth::default();
    for _ in 0..25 {
        g.absorb(&[("system prompt", 3_858), ("tool results", 100)]);
        g.absorb(&[("tool results", 100)]);
    }
    let ever: HashMap<_, _> = Growth::sorted(&g.session).into_iter().collect();
    assert_eq!(ever.get("system prompt"), None, "not a thing that piles up");
    assert_eq!(
        ever["tool results"], 100,
        "and the real material is counted once"
    );
}

#[test]
fn new_turn_and_model_invalidation_preserve_distinct_scopes() {
    let mut state = Accounting::default();
    let usage = Usage {
        prompt: 100,
        output: 20,
        calls: 1,
        ..Default::default()
    };
    state.record_completion(1, usage);
    state.measurements.parts = vec![("tool results", 50)];
    state.measurements.growth.absorb(&[("tool results", 50)]);
    state.new_turn();
    assert_eq!(state.last_call(), Some(usage));
    assert_eq!(state.turn_total(), Usage::default());
    assert_eq!(state.session_total(), usage);
    assert!(state.turn_growth().is_empty());
    assert_eq!(state.previous().get("tool results"), Some(&50));
    assert_eq!(state.session_growth().get("tool results"), Some(&50));
    assert_eq!(state.reference_history(), &[(1, 100)]);
    assert_eq!(state.parts(), &[("tool results", 50)]);
    state.record_completion(2, usage);
    state.invalidate_context_measurement();
    assert!(state.report().is_none());
    assert_eq!(state.turn_total(), usage);
    assert_eq!(state.session_total().calls, 2);
    assert_eq!(state.previous().get("tool results"), Some(&50));
    assert_eq!(state.reference_history(), &[(1, 100), (2, 100)]);
}

#[test]
fn restoring_measurements_preserves_both_reference_history_and_bound_peaks() {
    let restored = || {
        Measurements::restored(
            None,
            Usage::default(),
            Usage {
                calls: 7,
                ..Default::default()
            },
            vec![],
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
    };
    let mut state = Accounting::default();
    state.record_completion(
        1,
        Usage {
            prompt: 100,
            ..Default::default()
        },
    );
    state.restore(restored());
    assert_eq!(state.session_total().calls, 7);
    assert_eq!(state.reference_history(), &[(1, 100)]);
    let dir = tempfile::tempdir().unwrap();
    let log = lattice::EventLog::open_segmented(
        ce::core_event_decls(),
        "accounting",
        dir.path().join("test.ledger"),
        4096,
    )
    .unwrap();
    state.bind_peaks(&log.reader()).unwrap();
    assert!(state.reference_history().is_empty());
    assert_eq!(state.indexed_history().unwrap(), vec![(1, 100)]);
    state.restore(restored());
    assert_eq!(state.session_total().calls, 7);
    assert_eq!(state.indexed_history().unwrap(), vec![(1, 100)]);
    assert!(
        state.history().is_empty(),
        "a frame does not reload the peak index"
    );
}
