//! Bottom authorization choices. Never inspect or modify the suspended draft.
use super::theme::{ACCENT, DIM, FG, WARM};
use lattice::view::AuthorizationPrompt;
use lattice::wrap::{self, Run};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub(super) const HEIGHT: u16 = 7;
pub(super) const KEYS: &str = "Up/Down choose · Enter confirm · Esc refuse · PgUp/PgDn details";

pub(super) fn lines(
    prompt: &AuthorizationPrompt,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let option = |allow: bool| {
        let selected = prompt.allow_selected == allow;
        let label = if allow {
            "Allow request"
        } else {
            "Refuse request"
        };
        Line::from(Span::styled(
            format!("{} {label}", if selected { ">" } else { " " }),
            if selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        ))
    };
    // Even a tiny terminal keeps the selected answer visible.
    if height == 1 {
        return vec![option(prompt.allow_selected)];
    }
    let mut rows = Vec::new();
    if height >= 3 {
        let id: String = prompt.request.chars().filter(|c| !c.is_control()).collect();
        rows.push(Line::from(Span::styled(
            format!("Authorization: {id}"),
            Style::default().fg(WARM),
        )));
    }
    rows.extend([option(true), option(false)]);
    let room = height.saturating_sub(rows.len());
    if room > 0 {
        let details: Vec<String> = prompt
            .description
            .lines()
            .flat_map(|line| {
                wrap::wrap(&[Run::new(line.trim(), 0)], width)
                    .into_iter()
                    .map(|runs| runs.into_iter().map(|run| run.text).collect())
            })
            .collect();
        let truncated = details.len() > room;
        rows.extend(
            details
                .into_iter()
                .take(room - usize::from(truncated))
                .map(|text| Line::from(Span::styled(text, Style::default().fg(FG)))),
        );
        if truncated {
            rows.push(Line::from(Span::styled(
                "More details in transcript (PgUp)",
                Style::default().fg(DIM),
            )));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_panels_keep_choices_and_never_emit_terminal_controls() {
        let prompt = AuthorizationPrompt {
            request: "request\x1b\t".into(),
            description: "Browser: click\n\x1b[2Jmore details".into(),
            allow_selected: false,
        };
        for height in 1..10 {
            let rows = lines(&prompt, 24, height);
            assert!(rows.len() <= height);
            let text: String = rows
                .iter()
                .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
                .collect();
            assert!(text.contains("> Refuse request"));
            assert!(!text.contains('\x1b'));
            assert!(!text.contains('\t'));
        }
    }
}
