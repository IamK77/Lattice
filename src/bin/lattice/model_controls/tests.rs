use super::*;
use crate::terminal_host::{on_key, Hit, Ui, AT_MODELS};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn form_edits_preserve_error_and_unicode_boundaries() {
    let mut controls = ModelControls::default();
    assert!(!controls.paste_form("not consumed"));
    controls.open_form();
    controls.previous_field();
    assert_eq!(controls.form().unwrap().at, ModelForm::FIELDS.len() - 1);
    controls.next_field();
    assert_eq!(controls.form().unwrap().at, 0);
    controls.paste_form("a界");
    controls.form_problem("keep until typing".into());
    controls.backspace();
    assert_eq!(controls.form().unwrap().values[0], "a");
    controls.next_field();
    controls.previous_field();
    assert_eq!(
        controls.form().unwrap().problem.as_deref(),
        Some("keep until typing")
    );
    for text in [
        "bad\tvalue",
        "bad\nvalue",
        "bad\rvalue",
        "bad\u{1b}value",
        "bad\0value",
    ] {
        assert!(controls.paste_form(text));
        assert_eq!(controls.form().unwrap().values[0], "a");
        assert!(controls.form().unwrap().problem.is_some());
    }
    controls.type_character('界');
    assert_eq!(controls.form().unwrap().values[0], "a界");
    assert!(controls.form().unwrap().problem.is_none());
    controls.form_problem("clear on valid paste".into());
    assert!(controls.paste_form(""));
    assert!(controls.form().unwrap().problem.is_none());
    controls.close_form();
    controls.open_form();
    assert!(controls.form().unwrap().values.iter().all(String::is_empty));
}

#[test]
fn cursors_clamp_and_consuming_one_control_leaves_the_others() {
    let mut controls = ModelControls::default();
    controls.previous_dial();
    controls.next_picker(0);
    assert_eq!(controls.dial(), None);
    assert_eq!(controls.picker(), None);
    controls.open_dial(&EffortView::default());
    controls.previous_dial();
    assert_eq!(controls.dial(), Some(0));
    for _ in 0..dial_positions().len() + 2 {
        controls.next_dial();
    }
    assert_eq!(controls.dial(), Some(dial_positions().len() - 1));
    controls.seed_picker(8);
    controls.next_picker(0);
    assert_eq!(controls.picker(), Some(0));
    controls.previous_picker();
    controls.ask_delete("kept".into());
    controls.open_form();
    assert_eq!(controls.take_dial_word(), *dial_positions().last().unwrap());
    assert_eq!(controls.dial(), None);
    assert!(controls.form().is_some());
    assert_eq!(controls.deletion(), Some("kept"));
    assert_eq!(controls.take_picker(), Some(0));
    assert_eq!(controls.take_picker(), None);
    assert_eq!(controls.take_deletion().as_deref(), Some("kept"));
    assert!(!controls.blocks_paste());
}

fn key(ui: &mut Ui, code: KeyCode) {
    assert!(!on_key(
        ui,
        None,
        KeyEvent::new(code, KeyModifiers::NONE),
        &Hit::default()
    ));
}

#[test]
fn actual_keys_keep_overlapping_controls_and_non_press_events_separate() {
    let mut ui = Ui::replayed(&[]);
    ui.panel.show(AT_MODELS);
    ui.controls.open_dial(&EffortView::default());
    key(&mut ui, KeyCode::Right);
    assert_ne!(ui.panel.active(), Some(AT_MODELS));
    assert_eq!(ui.controls.dial(), Some(0), "panel owns the arrow first");
    ui.controls.open_form();
    ui.controls.ask_delete("keep".into());
    key(&mut ui, KeyCode::Char('y'));
    assert_eq!(ui.controls.form().unwrap().values[0], "y");
    assert_eq!(ui.controls.deletion(), Some("keep"));
    for kind in [
        crate::terminal_host::KeyEventKind::Repeat,
        crate::terminal_host::KeyEventKind::Release,
    ] {
        let mut event = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        event.kind = kind;
        on_key(&mut ui, None, event, &Hit::default());
        assert!(ui.controls.form().is_some());
    }
    on_key(
        &mut ui,
        None,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        &Hit::default(),
    );
    assert_eq!(ui.controls.form().unwrap().values[0], "yx");
    key(&mut ui, KeyCode::Esc);
    assert!(ui.controls.form().is_none());
    assert_eq!(ui.controls.deletion(), Some("keep"));
    key(&mut ui, KeyCode::Esc);
    assert!(ui.controls.deletion().is_none());
    assert_eq!(ui.controls.dial(), Some(0));
}

#[test]
fn catalog_keys_close_only_after_success_and_delete_confirmation_is_consumed() {
    const CHILD: &str = "LATTICE_MODEL_CONTROLS_TEST_CHILD";
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let directory = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "terminal_host::model_controls::tests::catalog_keys_close_only_after_success_and_delete_confirmation_is_consumed", "--nocapture"])
            .env(CHILD, "1")
            .env("LATTICE_MODELS", directory.path().join("models.json"))
            .env("HOME", directory.path())
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            String::from_utf8_lossy(&result.stdout).contains("1 passed; 0 failed"),
            "the isolated catalog test must actually execute"
        );
        return;
    }
    // Only this isolated child may call the real catalog-writing entry points.
    let path = lattice::models::path().unwrap();
    let mut ui = Ui::replayed(&[]);
    ui.panel.show(AT_MODELS);
    let fill = |ui: &mut Ui| {
        key(ui, KeyCode::Char('a'));
        for field in [
            "fixture",
            "openai",
            "fixture-model",
            "https://fixture.invalid/v1",
            "fake-test-key",
            "",
        ] {
            crate::terminal_host::absorb_paste(ui, field);
            key(ui, KeyCode::Tab);
        }
    };
    fill(&mut ui);
    key(&mut ui, KeyCode::Enter);
    assert!(ui.controls.form().is_none());
    assert_eq!(
        ui.domain.model.catalog().rows[ui.panel.selected_row()].id,
        "fixture"
    );
    let written = std::fs::read(&path).unwrap();
    fill(&mut ui);
    let fields = ui.controls.form().unwrap().values.clone();
    key(&mut ui, KeyCode::Enter);
    assert_eq!(ui.controls.form().unwrap().values, fields);
    assert!(ui.controls.form().unwrap().problem.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), written);
    key(&mut ui, KeyCode::Esc);
    key(&mut ui, KeyCode::Char('d'));
    assert_eq!(ui.controls.deletion(), Some("fixture"));
    key(&mut ui, KeyCode::Char('y'));
    assert!(ui.controls.deletion().is_none());
    assert!(ui
        .domain
        .model
        .catalog()
        .rows
        .iter()
        .all(|row| row.id != "fixture"));
    assert!(ui.flash.as_deref().unwrap().starts_with("deleted fixture"));

    // A damaged catalog is not rewritten. The valid form stays intact.
    std::fs::write(&path, "not-json").unwrap();
    fill(&mut ui);
    key(&mut ui, KeyCode::Enter);
    assert_eq!(ui.controls.form().unwrap().values, fields);
    assert!(ui.controls.form().unwrap().problem.is_some());
    key(&mut ui, KeyCode::Esc);
    ui.controls.ask_delete("fixture".into());
    key(&mut ui, KeyCode::Char('y'));
    assert!(
        ui.controls.deletion().is_none(),
        "failure still consumes the confirmation"
    );
    assert!(!ui.flash.as_deref().unwrap().starts_with("deleted"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not-json");
}
