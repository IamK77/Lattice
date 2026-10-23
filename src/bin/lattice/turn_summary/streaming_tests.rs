use super::*;
use crate::terminal_host::{fold_render, RenderEvent};

fn notice(ui: &mut Ui, payload: Value) {
    fold_render(
        ui,
        RenderEvent::Notice {
            source: "model".into(),
            payload,
        },
    )
    .unwrap();
}
fn line(ui: &Ui) -> String {
    activity::line(ui, 150).to_string()
}

#[test]
fn streaming_summary_changes_before_completion_and_replaces_estimates_once() {
    let mut ui = Ui::replayed(&[input(1), start(2, &["e1"])]);
    let persisted = serde_json::to_value(&ui.domain.turn_summary).unwrap();
    notice(&mut ui, json!({"call":"e2", "chunk":"abcdefgh"}));
    assert!(line(&ui).contains("↑— ↓~2"), "{}", line(&ui));
    notice(
        &mut ui,
        json!({"call":"e2", "chunk":"ijklmnop", "phase":"reasoning"}),
    );
    assert!(line(&ui).contains("↑— ↓~4"), "{}", line(&ui));
    assert!(!line(&ui).contains(" · model"));
    assert_eq!(
        serde_json::to_value(&ui.domain.turn_summary).unwrap(),
        persisted
    );
    let restored: Summary = serde_json::from_value(persisted).unwrap();
    assert_eq!(view(&restored).output, TokenCount::default());
    ui.absorb(
        &done(3, "e2", json!({"prompt_tokens":10,"completion_tokens":3})),
        3,
    );
    assert!(line(&ui).contains("↑10 ↓3"), "{}", line(&ui));
    notice(
        &mut ui,
        json!({"call":"e2", "chunk":"late text must not count"}),
    );
    ui.absorb(&start(4, &["e3"]), 4);
    notice(&mut ui, json!({"call":"e4", "chunk":"abcdefgh"}));
    assert!(line(&ui).contains("↑≥10 ↓~5"), "{}", line(&ui));
    ui.absorb(
        &done(5, "e4", json!({"prompt_tokens":20,"completion_tokens":6})),
        5,
    );
    ui.absorb(&event(6, ce::TURN_COMPLETED, &["e5"], json!({})), 6);
    assert_eq!(view(&ui.domain.turn_summary).output, measured(9, true));
    assert!(!line(&ui).contains('~'));
}

#[test]
fn streaming_summary_prefers_measured_snapshots_and_estimates_only_subsequent_text() {
    let mut ui = Ui::replayed(&[input(1), start(2, &["e1"])]);
    notice(
        &mut ui,
        json!({"call":"e2", "usage":{"prompt_tokens":12,"completion_tokens":1}}),
    );
    assert!(line(&ui).contains("↑≥12 ↓≥1"), "{}", line(&ui));
    notice(&mut ui, json!({"call":"e2", "chunk":"abcdefgh"}));
    assert!(line(&ui).contains("↑≥12 ↓~3"), "{}", line(&ui));
    notice(
        &mut ui,
        json!({"call":"e2", "usage":{"prompt_tokens":12,"completion_tokens":2}}),
    );
    assert!(line(&ui).contains("↑≥12 ↓≥2"), "{}", line(&ui));
    ui.absorb(
        &done(3, "e2", json!({"prompt_tokens":12,"completion_tokens":2})),
        3,
    );
    assert!(line(&ui).contains("↑12 ↓2"), "{}", line(&ui));
}

#[test]
fn streaming_summary_is_call_scoped_and_drops_provisional_counts_on_interruption() {
    let mut ui = Ui::replayed(&[
        input(1),
        start(2, &["e1"]),
        input(3),
        start(4, &["e3"]),
        start(5, &["e4"]),
    ]);
    for payload in [
        json!({"call":"e2","chunk":"old output"}),
        json!({"chunk":"unattributed output"}),
        json!({"call":"missing","usage":{"completion_tokens":999}}),
        json!({"call":"e5","chunk":"auxiliary","purpose":"compact"}),
        json!({"call":"e5","chunk":"unknown phase","phase":"other"}),
    ] {
        notice(&mut ui, payload);
    }
    assert!(line(&ui).contains("↑— ↓—"), "{}", line(&ui));
    notice(&mut ui, json!({"call":"e5", "chunk":"abcdefgh"}));
    assert!(line(&ui).contains("↓~2"), "{}", line(&ui));
    ui.absorb(&event(6, ce::INTERRUPTED, &["e5"], json!({"by":"user"})), 6);
    notice(&mut ui, json!({"call":"e5", "chunk":"late text"}));
    assert!(line(&ui).contains("↓—"), "{}", line(&ui));
    assert!(!line(&ui).contains('~'));
}

#[test]
fn streaming_summary_estimation_is_independent_of_chunk_boundaries() {
    let mut whole = Ui::replayed(&[input(1), start(2, &["e1"])]);
    let mut split = Ui::replayed(&[input(1), start(2, &["e1"])]);
    let text = "abcdef你好";
    notice(&mut whole, json!({"call":"e2","chunk":text}));
    for ch in text.chars() {
        notice(&mut split, json!({"call":"e2","chunk":ch.to_string()}));
    }
    assert!(line(&whole).contains("↓~4"), "{}", line(&whole));
    assert!(line(&split).contains("↓~4"), "{}", line(&split));
}

#[test]
fn streaming_summary_input_only_usage_does_not_stop_output_estimation() {
    let mut ui = Ui::replayed(&[input(1), start(2, &["e1"])]);
    notice(&mut ui, json!({"call":"e2","usage":{"prompt_tokens":0}}));
    assert!(line(&ui).contains("↑≥0 ↓—"), "{}", line(&ui));
    notice(&mut ui, json!({"call":"e2","chunk":"abcdefgh"}));
    notice(&mut ui, json!({"call":"e2","usage":{"prompt_tokens":12}}));
    assert!(line(&ui).contains("↑≥12 ↓~2"), "{}", line(&ui));
    ui.absorb(&input(3), 3);
    notice(
        &mut ui,
        json!({"call":"e2","chunk":"old tail","usage":{"completion_tokens":500}}),
    );
    assert!(line(&ui).contains("↑— ↓—"), "{}", line(&ui));
}

#[test]
fn streaming_summary_estimate_marker_survives_narrow_layout_or_is_omitted_whole() {
    let mut ui = Ui::replayed(&[input(1), start(2, &["e1"])]);
    notice(&mut ui, json!({"call":"e2","chunk":"x".repeat(4_800)}));
    for width in 0..100 {
        let row = activity::line(&ui, width);
        assert!(row.width() <= width);
        let text = row.to_string();
        if text.contains('↓') {
            assert!(text.contains("↓~1.2k"), "{width}: {text}");
        }
    }
}
