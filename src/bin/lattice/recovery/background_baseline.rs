use super::*;

#[test]
fn pre_background_v4_rows_preserve_history_and_reset_live_progress() {
    let old = include_bytes!("../../../../tests/fixtures/terminal-background-state-v4/state.json");
    let mut ui = Ui::replayed(&[]);
    serde_json::from_slice::<State>(old)
        .unwrap()
        .apply(&mut ui.domain, SETTLED_TICK)
        .unwrap();
    assert_eq!(ui.domain.background.rows().len(), 4);
    for (at, kind) in ["expert", "command", "timer", "watch"]
        .into_iter()
        .enumerate()
    {
        let row = &ui.domain.background.rows()[at];
        assert_eq!(row.kind, kind);
        assert_eq!(row.key, format!("{kind}:fixture"));
        assert_eq!(row.label, format!("fixture {kind}"));
        assert_eq!(row.standing, at >= 2);
        assert_eq!(row.fires, at + 3);
        assert_eq!(
            row.ledger.as_deref(),
            (at == 0).then_some("/fixture/background/expert.ledger")
        );
        assert_eq!(row.since, crate::terminal_host::SETTLED_TICK);
        assert_eq!((row.tools, row.tokens, row.read_len), (0, 0, 0));
    }
    // Capturing cannot add local progress fields to the historical shape.
    for at in 0..ui.domain.background.rows().len() {
        ui.domain.background.fixture_edit(at, |row| {
            row.since = 17;
            row.tools = 42;
            row.tokens = 88;
            row.read_len = 256;
        });
    }
    let old: serde_json::Value = serde_json::from_slice(old).unwrap();
    let captured = serde_json::to_value(State::reference_capture(&ui.domain)).unwrap();
    assert_eq!(captured["background"], old["background"]);
    assert_eq!(VERSION, 5);
}
