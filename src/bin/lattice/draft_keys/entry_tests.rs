use super::*;
use ratatui::crossterm::event::KeyEvent;

fn press(ui: &mut Ui, code: KeyCode, modifiers: KeyModifiers) {
    assert!(!on_key(
        ui,
        None,
        KeyEvent::new(code, modifiers),
        &Hit::default()
    ));
}

#[test]
fn draft_keys_preserve_each_edits_cursor_and_selection_rule() {
    use KeyCode::*;
    let none = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let alt = KeyModifiers::ALT;
    for (code, modifiers, text, cursor, reset) in [
        (Left, none, "one two", 2, false),
        (Right, none, "one two", 4, false),
        (Left, ctrl, "one two", 0, false),
        (Left, alt, "one two", 0, false),
        (Right, ctrl, "one two", 7, false),
        (Right, alt, "one two", 7, false),
        (Home, none, "one two", 0, false),
        (End, none, "one two", 7, false),
        (Char('a'), ctrl, "one two", 0, false),
        (Char('e'), ctrl, "one two", 7, false),
        (Delete, none, "onetwo", 3, false),
        (Backspace, none, "on two", 2, true),
        (Char('w'), ctrl, " two", 0, false),
        (Char('u'), ctrl, " two", 0, false),
        (Char('k'), ctrl, "one", 3, false),
        (Char('X'), none, "oneX two", 4, true),
        (Char('X'), alt, "oneX two", 4, true),
        (Char('X'), ctrl, "one two", 3, false),
        (Enter, KeyModifiers::SHIFT, "one\n two", 4, false),
        (Enter, alt | ctrl, "one\n two", 4, false),
    ] {
        let mut ui = Ui::replayed(&[]);
        ui.draft.edit().set("one two");
        ui.draft.edit().home();
        for _ in 0..3 {
            ui.draft.edit().right();
        }
        ui.draft.next_hint(10);
        ui.flash = Some("old".into());
        press(&mut ui, code, modifiers);
        assert_eq!(ui.draft.editor().text(), text, "{code:?} {modifiers:?}");
        assert_eq!(ui.draft.editor().cursor(), cursor, "{code:?} {modifiers:?}");
        assert_eq!(
            ui.draft.selected(),
            usize::from(!reset),
            "{code:?} {modifiers:?}"
        );
        assert!(ui.flash.is_none());
        assert!(!ui.busy());
    }
}

#[test]
fn draft_keys_candidates_precede_history_and_tab_clamps_without_submitting() {
    let mut ui = Ui::replayed(&[]);
    ui.draft.edit().set("old history");
    ui.draft.submit();
    ui.domain.skills = vec![
        ("zz-first".into(), String::new()),
        ("zz-second".into(), String::new()),
    ];
    ui.draft.edit().set("/zz");
    press(&mut ui, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(ui.draft.selected(), 1);
    assert_eq!(ui.draft.editor().text(), "/zz");
    press(&mut ui, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(ui.draft.selected(), 0);
    for _ in 0..9 {
        ui.draft.next_hint(10);
    }
    press(&mut ui, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(ui.draft.editor().text(), "/zz-second ");
    assert_eq!(ui.draft.selected(), 0);
    ui.draft.edit().clear();
    ui.draft.edit().history_prev();
    assert_eq!(ui.draft.editor().text(), "old history");
    ui.draft.edit().set("no candidate");
    ui.draft.next_hint(10);
    press(&mut ui, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(ui.draft.editor().text(), "no candidate");
    assert_eq!(ui.draft.selected(), 1);
}

#[test]
fn draft_keys_nonpress_and_modal_inputs_do_not_reach_the_draft() {
    for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
        let mut ui = Ui::replayed(&[]);
        ui.flash = Some("old".into());
        let mut key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        key.kind = kind;
        assert!(!on_key(&mut ui, None, key, &Hit::default()));
        assert!(ui.draft.editor().is_empty());
        assert_eq!(ui.flash.as_deref(), Some("old"));
    }
    for mode in 0..5 {
        let mut ui = Ui::replayed(&[]);
        match mode {
            0 => ui.controls.open_form(),
            1 => ui.panel.show(AT_CONTEXT),
            2 => ui.controls.open_dial(&EffortView::default()),
            3 => ui.controls.seed_picker(0),
            _ => ui.controls.ask_delete("fixture".into()),
        }
        press(&mut ui, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(ui.draft.editor().is_empty(), "mode {mode}");
    }
}
