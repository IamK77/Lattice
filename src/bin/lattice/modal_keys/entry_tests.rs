use super::*;
use ratatui::crossterm::event::KeyEvent;

fn press(ui: &mut Ui, code: KeyCode, modifiers: KeyModifiers) -> bool {
    on_key(ui, None, KeyEvent::new(code, modifiers), &Hit::default())
}

#[test]
fn modal_priority_retains_coexisting_modes_and_form_fallthrough() {
    let mut ui = Ui::replayed(&[]);
    ui.controls.open_form();
    ui.controls.ask_delete("kept".into());
    ui.panel.show(AT_COMPONENTS);
    ui.controls.open_dial(&EffortView::default());
    ui.controls.seed_picker(0);
    assert!(!press(&mut ui, KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(ui.controls.form().unwrap().values[0], "y");
    assert_eq!(ui.controls.deletion(), Some("kept"));
    assert!(!press(&mut ui, KeyCode::PageDown, KeyModifiers::CONTROL));
    assert!(matches!(ui.navigation, Some(tabs::Navigation::Next)));
    assert_eq!(ui.controls.deletion(), Some("kept"));
    // The form does not take Right; deletion cancels and consumes it.
    press(&mut ui, KeyCode::Right, KeyModifiers::NONE);
    assert!(ui.controls.deletion().is_none());
    assert_eq!(ui.panel.active(), Some(AT_COMPONENTS));
    let dial = ui.controls.dial();
    press(&mut ui, KeyCode::Right, KeyModifiers::NONE);
    assert_ne!(ui.panel.active(), Some(AT_COMPONENTS));
    assert_eq!(ui.controls.dial(), dial);
    assert_eq!(ui.controls.picker(), Some(0));
    // Modified Enter still submits the form; it must not edit the draft.
    press(&mut ui, KeyCode::Enter, KeyModifiers::SHIFT);
    assert!(ui.controls.form().unwrap().problem.is_some());
    assert!(ui.draft.editor().text().is_empty());
}

#[test]
fn component_uninstall_guards_and_local_receipts_survive_without_session() {
    for removable in [false, true] {
        let mut ui = Ui::replayed(&[]);
        ui.parts = vec![(
            "fixture".into(),
            "fixture".into(),
            "in-process",
            String::new(),
            removable,
            vec![],
        )];
        ui.panel.show(AT_COMPONENTS);
        press(&mut ui, KeyCode::Enter, KeyModifiers::NONE);
        assert!(ui.panel.details_expanded());
        press(&mut ui, KeyCode::Char('u'), KeyModifiers::ALT);
        if removable {
            assert!(!ui.panel.is_visible());
            assert!(ui.panel.details_expanded());
            assert!(ui.busy());
            assert_eq!(ui.flash.as_deref(), Some("removing fixture"));
        } else {
            assert!(ui.panel.is_visible());
            assert!(!ui.busy());
            assert!(ui.flash.as_deref().unwrap().contains("cannot be removed"));
        }
        assert!(ui.draft.editor().text().is_empty());
    }
    let mut empty = Ui::replayed(&[]);
    empty.panel.show(AT_COMPONENTS);
    press(&mut empty, KeyCode::Char('u'), KeyModifiers::NONE);
    assert!(empty.panel.is_visible());
    assert!(empty.flash.is_none());
    assert!(!empty.busy());
}
