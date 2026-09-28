//! Slash candidate generation and presentation, not editor or command execution.
use crate::terminal_host::slash_catalog::{slash_col, SLASH};
use crate::terminal_host::text::clip;
use crate::terminal_host::theme::{ACCENT, DIM, FG};
use lattice::{view::EffortView, wrap};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub(super) const MAX_VISIBLE: usize = 6;

pub(super) struct Hint {
    pub name: String,
    pub summary: String,
    /// Skills go to the core as user input; commands execute locally.
    pub skill: bool,
}

pub(super) fn slash_matches(
    input: &str,
    skills: &[(String, String)],
    _effort: &EffortView,
) -> Vec<Hint> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let token = input.split_whitespace().next().unwrap_or(input);
    let mut hints: Vec<Hint> = SLASH
        .iter()
        .filter(|c| c.name.starts_with(token) && c.name != token)
        .map(|c| Hint {
            name: c.name.to_string(),
            summary: c.summary.to_string(),
            skill: false,
        })
        .collect();
    hints.extend(
        skills
            .iter()
            .map(|(name, description)| (format!("/{name}"), description))
            .filter(|(slash, _)| slash.starts_with(token) && slash != token)
            .map(|(slash, description)| Hint {
                name: slash,
                summary: description.clone(),
                skill: true,
            }),
    );
    hints
}

/// Fit a bounded window to the actual region and keep the selection visible.
pub(super) fn lines(
    hint: &[Hint],
    selected: usize,
    room: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let room = room.min(MAX_VISIBLE).min(hint.len());
    if room == 0 {
        return Vec::new();
    }
    let selected = selected.min(hint.len() - 1);
    let start = selected.saturating_sub(room / 2).min(hint.len() - room);
    let hidden = hint.len() - room;
    let mut lines: Vec<Line> = hint
        .iter()
        .enumerate()
        .skip(start)
        .take(room)
        .map(|(i, c)| {
            let on = i == selected;
            let name_style = Style::default()
                .fg(if on { FG } else { DIM })
                .add_modifier(if on {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            let summary_style = Style::default().fg(if on { FG } else { DIM });
            Line::from(vec![
                Span::styled(if on { "▸ " } else { "  " }, Style::default().fg(ACCENT)),
                Span::styled(format!("{:<w$}", c.name, w = slash_col()), name_style),
                Span::styled(c.summary.to_string(), summary_style),
            ])
        })
        .collect();
    // The hidden count rides on the last candidate, taking space from its
    // summary: allocating a separate row would leave no candidate at height 1.
    if let (true, Some(last)) = (hidden > 0, lines.last_mut()) {
        let tag = format!("  +{hidden}");
        let (want, have) = (
            last.spans
                .iter()
                .map(|s| wrap::str_cols(&s.content))
                .sum::<usize>()
                + wrap::str_cols(&tag),
            width,
        );
        if want > have {
            if let Some(summary) = last.spans.last_mut() {
                let keep = wrap::str_cols(&summary.content).saturating_sub(want - have);
                summary.content = clip(&summary.content, keep).into();
            }
        }
        last.spans
            .push(Span::styled(tag, Style::default().fg(ACCENT)));
    }
    lines
}

#[cfg(test)]
#[path = "candidates/tests.rs"]
mod tests;
