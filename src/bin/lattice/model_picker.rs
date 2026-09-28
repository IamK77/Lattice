//! Read-only model comparison. Catalog loading and committing a selection are
//! separate from the display of the facts that make that selection meaningful.

use super::theme::{ACCENT, DIM, FG};
use lattice::view::ModelView;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[cfg(test)]
#[path = "model_picker/tests.rs"]
mod tests;

/// Names share one horizontal row; below it, spell out what choosing the
/// highlighted model would change. Availability and cursor are separate cues.
pub(super) fn picker_lines(models: &ModelView, cursor: usize) -> Vec<Line<'static>> {
    if models.rows.is_empty() {
        return vec![Line::from(Span::styled(
            "  no models configured — see ~/.lattice/models.json".to_string(),
            Style::default().fg(DIM),
        ))];
    }
    let cursor = cursor.min(models.rows.len() - 1);
    let mut names = vec![Span::raw("  ")];
    for (i, row) in models.rows.iter().enumerate() {
        if i > 0 {
            // Models are distinct things, not rungs on one joined scale.
            names.push(Span::raw("  "));
        }
        let shade = Style::default().fg(if row.key_present { FG } else { DIM });
        let (open, close) = if i == cursor { ("[", "]") } else { (" ", " ") };
        names.push(Span::styled(open, Style::default().fg(ACCENT)));
        names.push(Span::styled(
            row.id.clone(),
            if i == cursor {
                shade.add_modifier(Modifier::BOLD)
            } else {
                shade
            },
        ));
        names.push(Span::styled(close, Style::default().fg(ACCENT)));
    }

    let row = &models.rows[cursor];
    let current = models.current();
    let heading = match current {
        Some(now) if now.id == row.id => format!("── {} · running now ", row.model),
        _ => format!("── changing to {} ", row.model),
    };
    // Rule to a fixed width so the comparison reads as a section.
    const RULE_TO: usize = 52;
    let ruled = format!(
        "  {heading}{}",
        "─".repeat(RULE_TO.saturating_sub(heading.chars().count()))
    );
    let mut lines = vec![
        Line::from(names),
        Line::from(""),
        Line::from(Span::styled(ruled, Style::default().fg(DIM))),
    ];
    let dim = Style::default().fg(DIM);
    let mut fact = |label: &str, text: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("     {label:9}"), dim),
            Span::styled(text, Style::default().fg(FG)),
        ]));
    };
    // An absent endpoint or key must say so rather than leave an empty value.
    let named = |text: &str| {
        if text.is_empty() {
            "none".to_string()
        } else {
            text.to_string()
        }
    };
    let changed = |was: &str, now: &str| {
        if was == now {
            named(now)
        } else {
            format!("{} → {}", named(was), named(now))
        }
    };
    match current {
        Some(now) if now.id != row.id => {
            if now.endpoint != row.endpoint {
                fact("endpoint", changed(&now.endpoint, &row.endpoint));
            }
            if now.dialect != row.dialect {
                fact("dialect", changed(&now.dialect, &row.dialect));
            }
            if now.window != row.window {
                fact(
                    "window",
                    changed(&window_text(now.window), &window_text(row.window)),
                );
            }
        }
        _ => {
            fact("endpoint", named(&row.endpoint));
            fact("window", window_text(row.window));
        }
    }
    fact(
        "key",
        match (row.key_env.is_empty(), row.key_present) {
            (true, _) => "none needed".to_string(),
            (false, true) => format!("{} is set", row.key_env),
            (false, false) => format!("{} is NOT set — this one cannot answer", row.key_env),
        },
    );
    lines
}

/// Missing knowledge of a context window is not a zero-size window.
fn window_text(window: Option<u64>) -> String {
    match window {
        Some(n) if n >= 1_000_000 => format!("{}M", n / 1_000_000),
        Some(n) if n >= 1_000 => format!("{}k", n / 1_000),
        Some(n) => n.to_string(),
        None => "unknown".to_string(),
    }
}
