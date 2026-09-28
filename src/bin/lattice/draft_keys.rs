//! Terminal editing rules after the coordinator has handled modal and session keys.
use crate::terminal_host::{candidates::slash_matches, draft::Draft};
use lattice::view::EffortView;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(super) fn handle(
    draft: &mut Draft,
    key: KeyEvent,
    input_width: usize,
    skills: &[(String, String)],
    effort: &EffortView,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let newline = key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
    // Headless callers without a drawn frame still have logical lines.
    let input_width = if input_width == 0 {
        usize::MAX
    } else {
        input_width
    };
    match key.code {
        KeyCode::Up => {
            if slash_matches(draft.editor().text(), skills, effort).is_empty() {
                if !draft.edit().vertical(input_width, false) {
                    draft.edit().history_prev();
                }
            } else {
                draft.previous_hint();
            }
        }
        KeyCode::Down => {
            let n = slash_matches(draft.editor().text(), skills, effort).len();
            if n == 0 {
                if !draft.edit().vertical(input_width, true) {
                    draft.edit().history_next();
                }
            } else {
                draft.next_hint(n);
            }
        }
        KeyCode::Left if ctrl || alt => draft.edit().word_left(),
        KeyCode::Right if ctrl || alt => draft.edit().word_right(),
        KeyCode::Left => draft.edit().left(),
        KeyCode::Right => draft.edit().right(),
        KeyCode::Home => draft.edit().home(),
        KeyCode::End => draft.edit().end(),
        KeyCode::Char('a') if ctrl => draft.edit().home(),
        KeyCode::Char('e') if ctrl => draft.edit().end(),
        KeyCode::Delete => draft.edit().delete(),
        KeyCode::Char('w') if ctrl => draft.edit().delete_word(),
        KeyCode::Char('u') if ctrl => draft.edit().kill_to_start(),
        KeyCode::Char('k') if ctrl => draft.edit().kill_to_end(),
        KeyCode::Backspace => {
            draft.edit().backspace();
            draft.reset_selection();
        }
        KeyCode::Tab => {
            let candidates = slash_matches(draft.editor().text(), skills, effort);
            if !candidates.is_empty() {
                let selected = draft.selected().min(candidates.len() - 1);
                draft.edit().set(format!("{} ", candidates[selected].name));
                draft.reset_selection();
            }
        }
        KeyCode::Enter if newline => draft.edit().insert('\n'),
        KeyCode::Char(c) if !ctrl => {
            draft.edit().insert(c);
            draft.reset_selection();
        }
        _ => {}
    }
}
