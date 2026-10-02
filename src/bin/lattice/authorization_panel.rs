//! Transient authorization choices and scrollable details, never transcript text.
use super::theme::{ACCENT, DIM, FG, WARM};
use lattice::view::{AuthorizationChoice as Choice, AuthorizationPrompt};
use lattice::wrap::{self, Run};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub(super) const HEIGHT: u16 = 10;
pub(super) const KEYS: &str = "Up/Down choose · Enter confirm · PgUp/PgDn details · Esc refuse";

fn label(choice: Choice) -> &'static str {
    match choice {
        Choice::Once => "Allow once",
        Choice::Flow => "Allow for this conversation (survives reopening)",
        Choice::Permanent => "Trust permanently (across conversations)",
        Choice::Refuse => "Refuse request",
    }
}

fn detail_rows(prompt: &AuthorizationPrompt, width: usize) -> Vec<String> {
    prompt
        .description
        .lines()
        .flat_map(|line| {
            wrap::wrap(&[Run::new(line, 0)], width.max(1))
                .into_iter()
                .map(|runs| runs.into_iter().map(|run| run.text).collect())
        })
        .collect()
}

fn detail_room(prompt: &AuthorizationPrompt, height: usize) -> usize {
    let heading = usize::from(height > prompt.choices.len() + 1);
    height.saturating_sub(prompt.choices.len() + heading + 1)
}

pub(super) fn scroll(
    prompt: &AuthorizationPrompt,
    width: usize,
    height: usize,
    down: bool,
) -> usize {
    let room = detail_room(prompt, height).max(1);
    let maximum = detail_rows(prompt, width).len().saturating_sub(room);
    let current = prompt.detail_line.min(maximum);
    if down {
        current.saturating_add(room).min(maximum)
    } else {
        current.saturating_sub(room)
    }
}

pub(super) fn lines(
    prompt: &AuthorizationPrompt,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let option = |choice: Choice| {
        let selected = prompt.selected == choice;
        Line::from(Span::styled(
            format!("{} {}", if selected { ">" } else { " " }, label(choice)),
            if selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        ))
    };
    // A tiny panel follows the selected choice rather than hiding it below
    // the viewport. Growing the terminal restores all choices and details.
    if height <= prompt.choices.len() {
        let at = prompt
            .choices
            .iter()
            .position(|c| *c == prompt.selected)
            .unwrap_or(0);
        let start = at.saturating_sub(height - 1);
        return prompt
            .choices
            .iter()
            .skip(start)
            .take(height)
            .copied()
            .map(option)
            .collect();
    }
    let mut rows = Vec::new();
    if height > prompt.choices.len() + 1 {
        let id: String = prompt.request.chars().filter(|c| !c.is_control()).collect();
        rows.push(Line::from(Span::styled(
            format!("Authorization: {id}"),
            Style::default().fg(WARM),
        )));
    }
    rows.extend(prompt.choices.iter().copied().map(option));
    let room = detail_room(prompt, height);
    let details = detail_rows(prompt, width);
    let start = prompt
        .detail_line
        .min(details.len().saturating_sub(room.max(1)));
    let end = (start + room).min(details.len());
    rows.extend(
        details[start..end]
            .iter()
            .map(|text| Line::from(Span::styled(text.clone(), Style::default().fg(FG)))),
    );
    rows.push(Line::from(Span::styled(
        if room == 0 {
            "Enlarge terminal to inspect request details".to_owned()
        } else {
            format!(
                "Details {}-{}/{} · PgUp/PgDn",
                if details.is_empty() { 0 } else { start + 1 },
                end,
                details.len()
            )
        },
        Style::default().fg(DIM),
    )));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> AuthorizationPrompt {
        AuthorizationPrompt {
            request: "request\x1b\t".into(),
            description: "Browser: click\n\x1b[2Jmore details".into(),
            choices: vec![Choice::Once, Choice::Flow, Choice::Refuse],
            selected: Choice::Refuse,
            detail_line: 0,
        }
    }

    fn text(prompt: &AuthorizationPrompt, width: usize, height: usize) -> String {
        lines(prompt, width, height)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn small_panels_keep_choices_and_never_emit_terminal_controls() {
        let prompt = prompt();
        for height in 1..12 {
            let rows = lines(&prompt, 24, height);
            assert!(rows.len() <= height);
            let text = text(&prompt, 24, height);
            assert!(text.contains("> Refuse request"));
            assert!(!text.contains('\x1b'));
            assert!(!text.contains('\t'));
        }
        let shown = text(&prompt, 80, HEIGHT as usize);
        assert!(shown.contains("Allow once"));
        assert!(shown.contains("Allow for this conversation (survives reopening)"));
        assert!(!shown.contains("Trust permanently"));
    }

    #[test]
    fn every_wrapped_detail_is_reachable_without_transcript_or_overscroll() {
        let mut prompt = prompt();
        prompt.description = (0..35)
            .map(|i| format!("long-command-{i:02}: {}", "argument ".repeat(5)))
            .collect::<Vec<_>>()
            .join("\n");
        let width = 24;
        let height = HEIGHT as usize;
        let expected = detail_rows(&prompt, width);
        let mut seen = Vec::new();
        loop {
            let room = detail_room(&prompt, height);
            seen.extend(expected.iter().skip(prompt.detail_line).take(room).cloned());
            let shown = text(&prompt, width, height);
            for row in expected.iter().skip(prompt.detail_line).take(room) {
                assert!(shown.contains(row));
            }
            assert!(!shown.contains("transcript"));
            let next = scroll(&prompt, width, height, true);
            if next == prompt.detail_line {
                break;
            }
            prompt.detail_line = next;
        }
        for row in expected {
            assert!(seen.contains(&row), "missing {row}");
        }
        let last = prompt.detail_line;
        assert!(last > 0);
        prompt.detail_line = scroll(&prompt, width, height, false);
        assert!(prompt.detail_line < last);
        prompt.detail_line = usize::MAX;
        assert!(text(&prompt, width, height).contains("long-command-34"));
    }
}
