//! Local modal priority and edits. Effects remain with the terminal host.
use super::model_actions::Action;
use super::model_controls::ModelControls;
use super::panels::{navigation::PanelNavigation, AT_COMPONENTS, AT_MODELS};
use ratatui::crossterm::event::KeyCode;

pub(super) enum Outcome {
    Unhandled,
    Consumed,
    Model(Action<'static>),
    Uninstall(usize),
}

pub(super) fn handle(
    controls: &mut ModelControls,
    panel: &mut PanelNavigation,
    key: KeyCode,
    models: usize,
    components: usize,
) -> Outcome {
    use Outcome::*;
    match key {
        KeyCode::Esc if controls.form().is_some() => controls.close_form(),
        KeyCode::Enter if controls.form().is_some() => return Model(Action::SubmitForm),
        KeyCode::Tab | KeyCode::Down if controls.form().is_some() => controls.next_field(),
        KeyCode::BackTab | KeyCode::Up if controls.form().is_some() => controls.previous_field(),
        KeyCode::Backspace if controls.form().is_some() => controls.backspace(),
        KeyCode::Char(c) if controls.form().is_some() => controls.type_character(c),
        // An unrecognized form key intentionally continues into the other modes.
        KeyCode::Char('y') if controls.deletion().is_some() => return Model(Action::ConfirmDelete),
        _ if controls.deletion().is_some() => return Model(Action::CancelDelete),
        KeyCode::Char('a') if panel.active() == Some(AT_MODELS) => return Model(Action::OpenForm),
        KeyCode::Char('d') if panel.active() == Some(AT_MODELS) => return Model(Action::AskDelete),
        KeyCode::Enter
            if panel.active() == Some(AT_COMPONENTS) || panel.active() == Some(AT_MODELS) =>
        {
            panel.toggle_details()
        }
        KeyCode::Esc | KeyCode::Enter if panel.is_visible() => panel.close(),
        KeyCode::Left if panel.is_visible() => panel.previous_tab(),
        KeyCode::Right | KeyCode::Tab if panel.is_visible() => panel.next_tab(),
        KeyCode::Up
            if panel.active() == Some(AT_COMPONENTS) || panel.active() == Some(AT_MODELS) =>
        {
            panel.previous_row()
        }
        KeyCode::Down
            if panel.active() == Some(AT_COMPONENTS) || panel.active() == Some(AT_MODELS) =>
        {
            panel.next_row(if panel.active() == Some(AT_MODELS) {
                models
            } else {
                components
            })
        }
        KeyCode::Char('s') if panel.active() == Some(AT_MODELS) => {
            return Model(Action::SwitchSelected)
        }
        KeyCode::Up if panel.is_visible() => panel.scroll_up(1),
        KeyCode::Down if panel.is_visible() => panel.scroll_down(1),
        KeyCode::PageUp if panel.is_visible() => panel.scroll_up(10),
        KeyCode::PageDown if panel.is_visible() => panel.scroll_down(10),
        KeyCode::Char('u') if panel.active() == Some(AT_COMPONENTS) => {
            return Uninstall(panel.selected_row())
        }
        _ if panel.is_visible() => {}
        KeyCode::Left if controls.dial().is_some() => controls.previous_dial(),
        KeyCode::Right if controls.dial().is_some() => controls.next_dial(),
        KeyCode::Enter if controls.dial().is_some() => return Model(Action::CommitDial),
        KeyCode::Esc if controls.dial().is_some() => controls.close_dial(),
        _ if controls.dial().is_some() => {}
        KeyCode::Left if controls.picker().is_some() => controls.previous_picker(),
        KeyCode::Right if controls.picker().is_some() => controls.next_picker(models),
        KeyCode::Enter if controls.picker().is_some() => return Model(Action::CommitPicker),
        KeyCode::Esc if controls.picker().is_some() => controls.close_picker(),
        _ if controls.picker().is_some() => {}
        _ => return Unhandled,
    }
    Consumed
}
