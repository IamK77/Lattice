use super::*;
use crate::terminal_host::{activity, Ui};
use lattice::view::View;
use serde_json::{json, Value};
use std::time::Duration;

#[path = "streaming_tests.rs"]
mod streaming_tests;

fn event(seq: u64, kind: &str, causes: &[&str], payload: Value) -> EventEnvelope {
    serde_json::from_value(json!({"v":1,"id":format!("e{seq}"),"seq":seq,
        "stream":"summary-test","time":format!("2026-10-18T00:00:{seq:02}Z"),
        "type":kind,"source":"fixture","causes":causes,"payload":payload}))
    .unwrap()
}
fn input(seq: u64) -> EventEnvelope {
    event(seq, ce::USER_MESSAGE, &[], json!({"text":"hello"}))
}
fn start(seq: u64, causes: &[&str]) -> EventEnvelope {
    event(
        seq,
        ce::MODEL_CALL_STARTED,
        causes,
        json!({"model":"synthetic","input":{"parts":[],"fingerprint":"synthetic"}}),
    )
}
fn done(seq: u64, cause: &str, usage: Value) -> EventEnvelope {
    event(
        seq,
        ce::MODEL_CALL_COMPLETED,
        &[cause],
        json!({"status":"ok","usage":usage}),
    )
}
fn fold(summary: &mut Summary, e: &EventEnvelope) {
    summary.observe(
        e,
        e.event_type == ce::USER_MESSAGE && e.causes.is_empty(),
        e.event_type == ce::TURN_COMPLETED,
        "synthetic",
        "openai",
    );
}
fn view(summary: &Summary) -> TurnSummary {
    summary.snapshot(1_792_281_620_000).unwrap()
}
fn measured(value: u64, complete: bool) -> TokenCount {
    TokenCount {
        known: Some(value),
        complete,
    }
}

#[test]
fn multiple_calls_accumulate_once_and_forwarding_is_not_an_extra_bill() {
    let mut s = Summary::default();
    fold(&mut s, &input(1));
    fold(&mut s, &start(2, &["e1"]));
    fold(&mut s, &start(3, &["e2"]));
    assert_eq!(view(&s).input, TokenCount::default());
    fold(
        &mut s,
        &done(4, "e3", json!({"prompt_tokens":100,"completion_tokens":8})),
    );
    assert_eq!(view(&s).input, measured(100, true));
    fold(
        &mut s,
        &done(
            5,
            "e2",
            json!({"prompt_tokens":900,"completion_tokens":999}),
        ),
    );
    assert_eq!(
        view(&s).output,
        measured(8, true),
        "duplicate outcome is not another call"
    );
    fold(&mut s, &start(6, &["tool-result"]));
    assert_eq!(
        view(&s).input,
        measured(100, false),
        "in-flight call makes the subtotal incomplete"
    );
    fold(
        &mut s,
        &done(7, "e6", json!({"input_tokens":140,"output_tokens":12})),
    );
    assert_eq!(view(&s).input, measured(240, true));
    assert_eq!(view(&s).output, measured(20, true));
    assert!(!view(&s).model_pending);
    assert!(s.aliases.is_empty() && s.roots.is_empty());
}

#[test]
fn missing_partial_zero_and_cancelled_counts_are_distinct() {
    let mut s = Summary::default();
    fold(&mut s, &input(1));
    fold(&mut s, &start(2, &[]));
    fold(&mut s, &done(3, "e2", json!({"input_tokens":0})));
    assert_eq!(view(&s).input, measured(0, true));
    assert_eq!(view(&s).output, TokenCount::default());
    fold(&mut s, &start(4, &[]));
    let mut result = done(5, "e4", json!({"input_tokens":30,"output_tokens":5}));
    result.payload["status"] = json!("cancelled");
    fold(&mut s, &result);
    assert_eq!(view(&s).input, measured(30, false));
    assert_eq!(view(&s).output, measured(5, false));
    fold(&mut s, &start(6, &[]));
    fold(&mut s, &done(7, "e6", json!({})));
    assert_eq!(view(&s).input, measured(30, false));
    fold(&mut s, &input(8));
    assert_eq!(view(&s).input, TokenCount::default());
}

#[test]
fn old_call_forwarding_and_late_outcomes_never_join_the_new_turn() {
    let mut s = Summary::default();
    fold(&mut s, &input(1));
    fold(&mut s, &start(2, &[]));
    fold(&mut s, &input(3));
    fold(&mut s, &start(4, &["e2"]));
    fold(
        &mut s,
        &done(5, "e4", json!({"input_tokens":500,"output_tokens":80})),
    );
    assert_eq!(view(&s).input, TokenCount::default());
    fold(&mut s, &start(6, &["e3"]));
    fold(
        &mut s,
        &done(7, "e6", json!({"input_tokens":20,"output_tokens":3})),
    );
    assert_eq!(view(&s).input, measured(20, true));
    assert_eq!(view(&s).output, measured(3, true));
}

#[test]
fn auxiliary_calls_and_forwarded_input_do_not_reset_or_add_usage() {
    let mut s = Summary::default();
    fold(&mut s, &input(1));
    fold(&mut s, &start(2, &[]));
    fold(
        &mut s,
        &done(3, "e2", json!({"input_tokens":15,"output_tokens":2})),
    );
    let mut forwarded = input(4);
    forwarded.causes.push("e1".into());
    fold(&mut s, &forwarded);
    let mut auxiliary = start(5, &["e2"]);
    auxiliary.payload["purpose"] = json!("condense");
    fold(&mut s, &auxiliary);
    fold(
        &mut s,
        &done(6, "e5", json!({"input_tokens":999,"output_tokens":999})),
    );
    assert_eq!(view(&s).input, measured(15, true));
    assert_eq!(s.id.as_deref(), Some("e1"));
}

#[test]
fn interruption_settles_the_forwarded_chain_without_fabricating_usage() {
    let mut s = Summary::default();
    for e in [
        input(1),
        start(2, &[]),
        done(3, "e2", json!({"input_tokens":5,"output_tokens":1})),
        start(4, &[]),
        start(5, &["e4"]),
    ] {
        fold(&mut s, &e);
    }
    fold(
        &mut s,
        &event(6, ce::INTERRUPTED, &["e4"], json!({"by":"deadline"})),
    );
    assert_eq!(view(&s).input, measured(5, false));
    assert!(!view(&s).model_pending);
    fold(
        &mut s,
        &done(7, "e5", json!({"input_tokens":400,"output_tokens":100})),
    );
    assert_eq!(view(&s).input, measured(5, false));
    fold(&mut s, &event(8, ce::TURN_COMPLETED, &["e6"], json!({})));
    let end = s.snapshot(i64::MAX).unwrap();
    assert_eq!(end.elapsed_ms, Some(7_000));
    assert!(!end.running);
    assert_eq!(s.snapshot(0), Some(end));
}

#[test]
fn request_dialect_survives_model_swap_and_includes_cache_exactly_once() {
    let mut s = Summary::default();
    fold(&mut s, &input(1));
    s.observe(&start(2, &[]), false, false, "synthetic", "anthropic");
    s.observe(&done(3,"e2",json!({"input_tokens":10,"cache_read_input_tokens":40,"cache_creation_input_tokens":5,"output_tokens":3})),false,false,"different-model","openai");
    assert_eq!(view(&s).input, measured(55, true));
    fold(&mut s, &start(4, &[]));
    fold(
        &mut s,
        &done(
            5,
            "e4",
            json!({"prompt_tokens":60,"prompt_tokens_details":{"cached_tokens":40},"completion_tokens":4}),
        ),
    );
    assert_eq!(view(&s).input, measured(115, true));
}

#[test]
fn provider_field_presence_does_not_invent_zero_or_double_count_reasoning() {
    for usage in [
        json!(null),
        json!({}),
        json!({"input_tokens":"12","output_tokens":-1}),
    ] {
        assert_eq!(
            accounting::token_counts(&json!({"usage":usage}), "synthetic", "openai"),
            (TokenCount::default(), TokenCount::default())
        );
    }
    for (usage, expected) in [
        (
            json!({"input_tokens":0,"output_tokens":0}),
            (measured(0, true), measured(0, true)),
        ),
        (
            json!({"prompt_tokens":120,"prompt_tokens_details":{"cached_tokens":80},"completion_tokens":30,"completion_tokens_details":{"reasoning_tokens":20}}),
            (measured(120, true), measured(30, true)),
        ),
        (
            json!({"input_tokens":0,"prompt_tokens":12,"completion_tokens":7}),
            (measured(12, true), measured(7, true)),
        ),
    ] {
        assert_eq!(
            accounting::token_counts(&json!({"usage":usage}), "synthetic", "openai"),
            expected
        );
    }
    assert_eq!(
        accounting::token_counts(
            &json!({"usage":{"cache_read_input_tokens":10}}),
            "synthetic",
            "anthropic"
        ),
        (measured(10, false), TokenCount::default())
    );
    assert_eq!(
        accounting::token_counts(
            &json!({"usage":{"input_tokens":u64::MAX,"cache_read_input_tokens":1,"output_tokens":0}}),
            "synthetic",
            "anthropic"
        ),
        (measured(u64::MAX, false), measured(0, true))
    );
    assert_eq!(
        accounting::read_usage(
            &json!({"usage":{"cache_read_input_tokens":10}}),
            "synthetic",
            "anthropic"
        )
        .unwrap()
        .prompt,
        10,
        "legacy panel semantics remain intact"
    );
}

#[test]
fn counter_overflow_is_a_lower_bound_not_an_exact_total() {
    let mut total = Total::default();
    total.add(Some(u64::MAX), true);
    total.add(Some(1), true);
    assert_eq!(total.count(2), measured(u64::MAX, false));
}

#[test]
fn monotonic_submission_clock_includes_waits_and_freezes_despite_wall_clock_changes() {
    let now = Instant::now();
    let mut clock = Clock::default();
    let mut s = Summary::default();
    clock.submitted(now);
    assert_eq!(
        clock
            .snapshot(&s, now + Duration::from_secs(2), 0)
            .unwrap()
            .elapsed_ms,
        Some(2_000)
    );
    let user = input(1);
    fold(&mut s, &user);
    clock.observe(&s, &user, now + Duration::from_secs(3));
    assert_eq!(
        clock
            .snapshot(&s, now + Duration::from_secs(18), i64::MIN)
            .unwrap()
            .elapsed_ms,
        Some(18_000)
    );
    let end = event(9, ce::TURN_COMPLETED, &[], json!({}));
    fold(&mut s, &end);
    clock.observe(&s, &end, now + Duration::from_secs(42));
    assert_eq!(
        clock
            .snapshot(&s, now + Duration::from_secs(300), i64::MAX)
            .unwrap()
            .elapsed_ms,
        Some(42_000)
    );
}

#[test]
fn pending_submissions_hide_old_wakes_and_old_endings_until_their_echo() {
    let now = Instant::now();
    let mut clock = Clock::default();
    let mut s = Summary::default();
    clock.submitted(now);
    clock.submitted(now + Duration::from_secs(1));
    let wake = event(1, ce::WAKE, &[], json!({"text":"background"}));
    s.observe(&wake, true, false, "synthetic", "openai");
    clock.observe(&s, &wake, now + Duration::from_secs(2));
    let end = event(2, ce::TURN_COMPLETED, &[], json!({}));
    fold(&mut s, &end);
    clock.observe(&s, &end, now + Duration::from_secs(3));
    assert!(
        clock
            .snapshot(&s, now + Duration::from_secs(4), 0)
            .unwrap()
            .running
    );
    let first = input(3);
    fold(&mut s, &first);
    clock.observe(&s, &first, now + Duration::from_secs(5));
    assert_eq!(
        clock
            .snapshot(&s, now + Duration::from_secs(6), 0)
            .unwrap()
            .elapsed_ms,
        Some(5_000)
    );
    let second = input(4);
    fold(&mut s, &second);
    clock.observe(&s, &second, now + Duration::from_secs(7));
    assert_eq!(
        clock
            .snapshot(&s, now + Duration::from_secs(8), 0)
            .unwrap()
            .elapsed_ms,
        Some(7_000)
    );
    assert!(clock.submitted.is_empty());
}

#[test]
fn restored_clock_uses_recorded_time_not_the_time_it_was_opened() {
    let mut s = Summary::default();
    fold(&mut s, &input(1));
    let end = event(9, ce::TURN_COMPLETED, &[], json!({}));
    fold(&mut s, &end);
    let mut clock = Clock::default();
    clock.observe(&s, &end, Instant::now());
    assert_eq!(
        clock
            .snapshot(&s, Instant::now(), i64::MAX)
            .unwrap()
            .elapsed_ms,
        Some(8_000)
    );
}

#[test]
fn real_domain_accepts_only_current_terminal_boundaries_and_renders_the_summary() {
    let mut ui = Ui::replayed(&[]);
    for e in [
        input(1),
        start(2, &["e1"]),
        event(3, ce::INTERRUPTED, &["e2"], json!({"by":"deadline"})),
        input(4),
        start(5, &["e4"]),
        event(6, ce::TURN_COMPLETED, &["e3"], json!({})),
    ] {
        ui.absorb(&e, 1);
    }
    assert!(
        ui.turn_summary().unwrap().running,
        "old ending does not freeze a new turn"
    );
    ui.absorb(
        &done(7, "e5", json!({"input_tokens":12,"output_tokens":4})),
        2,
    );
    ui.absorb(&event(8, ce::TURN_COMPLETED, &["e7"], json!({})), 3);
    ui.tick = 100;
    assert!(!ui.turn_summary().unwrap().running);
    let text = activity::line(&ui, 80).to_string();
    assert!(text.contains("Done") && text.contains("↑12 ↓4"), "{text}");
    assert!(activity::clock_second(&ui).is_none());
}

#[test]
fn actual_submit_path_starts_the_clock_before_the_ledger_echo() {
    use crate::terminal_host::{on_key, Hit};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().set("question");
    assert!(!on_key(
        &mut ui,
        None,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &Hit::default()
    ));
    let captured = *ui
        .turn_clock
        .submitted
        .back()
        .expect("submission captured before injection");
    let summary = ui
        .turn_clock
        .snapshot(
            &ui.domain.turn_summary,
            captured + Duration::from_secs(2),
            0,
        )
        .unwrap();
    assert_eq!(summary.elapsed_ms, Some(2_000));
    assert!(summary.running);
    assert_eq!(summary.input, TokenCount::default());
}

fn mid_call_input_events() -> Vec<EventEnvelope> {
    vec![
        input(1),
        start(2, &["e1"]),
        input(3),
        done(4, "e2", json!({"prompt_tokens":100,"completion_tokens":20})),
        event(5, ce::OUTPUT_REPLY, &["e4"], json!({"text":"old answer"})),
        event(6, ce::TURN_COMPLETED, &["e4"], json!({})),
        start(7, &["e4"]),
        done(8, "e7", json!({"prompt_tokens":12,"completion_tokens":4})),
        event(9, ce::OUTPUT_REPLY, &["e8"], json!({"text":"new answer"})),
        event(10, ce::TURN_COMPLETED, &["e8"], json!({})),
    ]
}

#[test]
fn mid_call_input_retains_its_origin_and_usage_after_the_old_boundary() {
    let events = mid_call_input_events();
    let mut ui = Ui::replayed(&[]);
    let mut clock = Clock::default();
    let base = Instant::now();
    for e in &events {
        let now = base + Duration::from_secs(e.seq);
        if e.event_type == ce::USER_MESSAGE {
            clock.submitted(now);
        }
        ui.absorb(e, e.seq as usize);
        clock.observe(&ui.domain.turn_summary, e, now);
    }
    let summary = view(&ui.domain.turn_summary);
    assert_eq!(
        (
            summary.running,
            summary.elapsed_ms,
            summary.input,
            summary.output
        ),
        (false, Some(7_000), measured(12, true), measured(4, true)),
        "the old boundary must not discard the newer input's time or usage"
    );
    assert_eq!(
        clock.snapshot(&ui.domain.turn_summary, base + Duration::from_secs(100), 0),
        Some(summary),
        "the live clock resumes from the original submission and freezes at its own ending"
    );
}

#[test]
fn mid_call_input_survives_recovery_before_and_after_the_old_boundary() {
    assert_summary_recovery(
        &mid_call_input_events(),
        measured(12, true),
        measured(4, true),
        "Done",
    );
}

#[test]
fn summary_survives_real_cold_warm_and_incremental_recovery() {
    let originals = [
        input(1),
        start(2, &["e1"]),
        start(3, &["e2"]),
        done(4, "e3", json!({"input_tokens":12,"output_tokens":3})),
        start(5, &["e4"]),
        event(6, ce::INTERRUPTED, &["e5"], json!({"by":"deadline"})),
        event(7, ce::TURN_COMPLETED, &["e6"], json!({})),
    ];
    assert_summary_recovery(
        &originals,
        measured(12, false),
        measured(3, false),
        "Timed out",
    );
}

fn assert_summary_recovery(
    originals: &[EventEnvelope],
    expected_input: TokenCount,
    expected_output: TokenCount,
    expected_status: &str,
) {
    use lattice::{EventDraft, EventLog};
    for cut in 0..=originals.len() {
        let temp = tempfile::tempdir().unwrap();
        let mut log = EventLog::open_segmented(
            ce::core_event_decls(),
            "summary-recovery",
            temp.path().join("test.ledger"),
            4096,
        )
        .unwrap();
        let mut ids = BTreeMap::new();
        let mut events = Vec::new();
        for original in originals {
            let causes: Vec<&str> = original
                .causes
                .iter()
                .map(|id| ids.get(id).map(String::as_str).unwrap())
                .collect();
            let mut draft =
                EventDraft::new(&original.event_type, &causes, original.payload.clone());
            draft.reason = Some("Synthetic summary test".into());
            let event = log.append(draft, "fixture").unwrap();
            ids.insert(original.id.clone(), event.id.clone());
            events.push(event);
        }
        let reader = log.reader();
        let reference = Ui::replayed(&events);
        let mut cold = Ui::replayed(&[]);
        cold.replay_prefix(&reader, cut as u64).unwrap();
        let mut warm = Ui::replayed(&[]);
        warm.replay_prefix(&reader, cut as u64).unwrap();
        assert_eq!(warm.recovery.as_ref().unwrap().replayed, 0);
        for event in &events[cut..] {
            warm.try_absorb_profiled(event, event.seq as usize, None)
                .unwrap();
        }
        assert_eq!(
            serde_json::to_value(&warm.domain.turn_summary).unwrap(),
            serde_json::to_value(&reference.domain.turn_summary).unwrap(),
            "cut {cut}"
        );
        let mut restored = Ui::replayed(&[]);
        restored
            .replay_prefix(&reader, events.len() as u64)
            .unwrap();
        let summary = restored.turn_summary().unwrap();
        assert_eq!(Some(summary), reference.turn_summary());
        assert_eq!(summary.input, expected_input);
        assert_eq!(summary.output, expected_output);
        assert_eq!(restored.turn_status(), expected_status);
        assert!(!summary.running);
        assert_eq!(
            reader.snapshot_end(),
            events.len() as u64,
            "display does not write audit events"
        );
    }
}

#[test]
fn serialized_summary_continues_at_every_pending_call_boundary() {
    let events = [
        input(1),
        start(2, &[]),
        start(3, &["e2"]),
        done(4, "e3", json!({"input_tokens":12})),
        start(5, &[]),
        event(6, ce::INTERRUPTED, &["e5"], json!({"by":"user"})),
        event(7, ce::TURN_COMPLETED, &["e6"], json!({})),
    ];
    let mut reference = Summary::default();
    for e in &events {
        fold(&mut reference, e);
    }
    for cut in 0..=events.len() {
        let mut s = Summary::default();
        for e in &events[..cut] {
            fold(&mut s, e);
        }
        let mut restored: Summary =
            serde_json::from_value(serde_json::to_value(s).unwrap()).unwrap();
        for e in &events[cut..] {
            fold(&mut restored, e);
        }
        assert_eq!(restored.snapshot(0), reference.snapshot(0), "cut {cut}");
        assert_eq!(
            serde_json::to_value(restored).unwrap(),
            serde_json::to_value(&reference).unwrap()
        );
    }
}
