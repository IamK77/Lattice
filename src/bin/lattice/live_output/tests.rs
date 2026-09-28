use super::*;
use serde_json::json;

fn populated() -> LiveOutput {
    let mut live = LiveOutput::default();
    assert_eq!(
        live.notice(&json!({"phase":"reasoning", "chunk":"thought"})),
        None
    );
    assert_eq!(live.notice(&json!({"chunk":"reply"})), None);
    live
}

#[test]
fn notices_route_only_foreground_chunks_and_preserve_receipt_precedence() {
    let mut live = populated();
    for payload in [
        json!({"purpose":"condense","chunk":"hidden"}),
        json!({"purpose":null,"chunk":"hidden","note":"hidden"}),
        json!({"phase":null,"chunk":"hidden","note":"hidden"}),
        json!({"phase":"other","chunk":"hidden","note":"hidden"}),
        json!({"chunk":"","note":"hidden"}),
    ] {
        assert_eq!(live.notice(&payload), None);
    }
    assert_eq!(live.reply(), "reply");
    assert_eq!(live.thinking(), "thought");
    assert_eq!(
        live.notice(&json!({"chunk":7,"note":"receipt"})),
        Some("receipt")
    );
    assert_eq!(
        live.notice(&json!({"phase":"reasoning","note":"receipt"})),
        Some("receipt")
    );
    live.notice(&json!({"chunk":" tail"}));
    live.notice(&json!({"phase":"reasoning","chunk":" tail"}));
    assert_eq!(live.reply(), "reply tail");
    assert_eq!(live.thinking(), "thought tail");
}

#[test]
fn cards_replace_only_their_channel_and_submission_only_the_reply() {
    let mut live = populated();
    live.card_landed(true);
    assert_eq!(live.reply(), "reply");
    assert!(live.thinking().is_empty());
    let mut live = populated();
    live.card_landed(false);
    assert!(live.reply().is_empty());
    assert_eq!(live.thinking(), "thought");
    let mut live = populated();
    live.submitted();
    assert!(live.reply().is_empty());
    assert_eq!(live.thinking(), "thought");
}

#[test]
fn only_round_end_interruption_and_waiting_clear_both_channels() {
    for event in [
        ce::MODEL_CALL_STARTED,
        ce::MODEL_CALL_COMPLETED,
        ce::USER_MESSAGE,
        ce::WAKE,
    ] {
        let mut live = populated();
        live.round_event(event);
        assert_eq!(live.reply(), "reply", "{event}");
        assert_eq!(live.thinking(), "thought", "{event}");
    }
    for event in [
        ce::TURN_COMPLETED,
        ce::INTERRUPTED,
        lattice::components::minimal_loop::WAITING,
    ] {
        let mut live = populated();
        live.round_event(event);
        assert!(live.reply().is_empty(), "{event}");
        assert!(live.thinking().is_empty(), "{event}");
    }
}
