//! The effort scale, its cursor positions and the consequences of a choice.
//! Choosing and sending the setting belong to the interaction coordinator.

use super::theme::{ACCENT, DIM, FG};
use lattice::components::model_common::Effort;
use lattice::view::EffortView;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[cfg(test)]
#[path = "effort_dial/tests.rs"]
mod tests;

/// Off, then every rung of the common ruler, including model-specific aliases.
pub(super) fn dial_positions() -> Vec<&'static str> {
    let mut all = vec!["off"];
    all.extend(Effort::ALL.iter().map(|rung| rung.name()));
    all
}

/// Open on the setting already in force.
pub(super) fn dial_home(effort: &EffortView) -> usize {
    let now = effort.now.as_deref().unwrap_or("off");
    dial_positions()
        .iter()
        .position(|word| *word == now)
        .unwrap_or(0)
}

/// Say what the highlighted position would actually send, before committing.
fn dial_readout(effort: &EffortView, cursor: usize) -> String {
    let positions = dial_positions();
    let word = positions[cursor.min(positions.len() - 1)];
    if word == "off" {
        // No parameter and explicit off are distinct. The dial offers only
        // the latter, so name the former rather than silently converting it.
        return match effort.now {
            None => "no thinking at all · currently no parameter is sent".to_string(),
            _ => "no thinking at all".to_string(),
        };
    }
    let Some(rung) = Effort::parse(word) else {
        return String::new();
    };
    let landed = lattice::components::model_common::place(rung, &effort.rungs);
    let mut says = match landed {
        Some(sent) if sent != word => format!("this model has no {word} — sends {sent}"),
        Some(sent) => format!("sends {sent}"),
        None => format!("sends {word}"),
    };
    if effort.now.as_deref() == Some(word) {
        says.push_str(" · current");
    } else {
        // Preserve the warning before the user commits to changing the prompt.
        says.push_str(" · next call starts from a cold cache");
    }
    says
}

/// One scale row and one readout row. Brightness denotes real rungs; brackets
/// denote the cursor. Reserving bracket columns prevents the scale from moving.
pub(super) fn dial_lines(effort: &EffortView, cursor: usize) -> Vec<Line<'static>> {
    let positions = dial_positions();
    let cursor = cursor.min(positions.len() - 1);
    let mut scale = vec![Span::raw("  ")];
    for (i, word) in positions.iter().enumerate() {
        if i > 0 {
            scale.push(Span::styled("·", Style::default().fg(DIM)));
        }
        let real = *word == "off"
            || Effort::parse(word)
                .and_then(|rung| lattice::components::model_common::place(rung, &effort.rungs))
                .map(|sent| sent == *word)
                // No declaration means unknown, not unsupported.
                .unwrap_or(true);
        let shade = Style::default().fg(if real { FG } else { DIM });
        let (open, close) = if i == cursor { ("[", "]") } else { (" ", " ") };
        scale.push(Span::styled(open, Style::default().fg(ACCENT)));
        scale.push(Span::styled(
            word.to_string(),
            if i == cursor {
                shade.add_modifier(Modifier::BOLD)
            } else {
                shade
            },
        ));
        scale.push(Span::styled(close, Style::default().fg(ACCENT)));
    }
    vec![
        Line::from(scale),
        Line::from(vec![
            Span::styled("  ── ", Style::default().fg(DIM)),
            Span::styled(dial_readout(effort, cursor), Style::default().fg(DIM)),
        ]),
    ]
}
