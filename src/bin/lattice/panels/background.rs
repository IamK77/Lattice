//! Background work is either running now or armed to act later.

use crate::terminal_host::text::fit;
use lattice::view::View;

pub(crate) fn rows(view: &dyn View, width: usize) -> Vec<(String, String)> {
    let live = view.background();
    if live.is_empty() {
        return vec![(String::new(), "nothing running, nothing armed".to_string())];
    }
    let mut rows = Vec::new();
    let owners: std::collections::BTreeSet<_> = live
        .iter()
        .filter_map(|row| row.reference.as_ref().map(|r| r.identity.stream.as_str()))
        .collect();
    for owner in owners {
        rows.push((String::new(), format!("stream: {owner}")));
    }
    let (working, waiting): (Vec<_>, Vec<_>) = live.iter().partition(|l| !l.standing);
    for (heading, group) in [("running", working), ("armed", waiting)] {
        if group.is_empty() {
            continue;
        }
        if !rows.is_empty() {
            rows.push((String::new(), String::new()));
        }
        rows.push((String::new(), heading.to_string()));
        for l in group {
            let note = if l.standing {
                format!("{} · fired {}", l.label, l.fires)
            } else if l.ledger.is_some() {
                format!(
                    "{} · {} tools · {} tokens",
                    l.label,
                    l.tools,
                    compact(l.tokens)
                )
            } else {
                l.label.clone()
            };
            if let Some(reference) = &l.reference {
                let kind = match reference.identity.kind {
                    lattice::background_aliases::Kind::Expert => "Expert",
                    lattice::background_aliases::Kind::Run => "Run",
                    lattice::background_aliases::Kind::Timer => "Timer",
                    lattice::background_aliases::Kind::Watch => "Watch",
                };
                rows.push((reference.alias.clone(), format!("{kind:<7} {note}")));
            } else {
                rows.push((l.key.clone(), fit(&note, width.saturating_sub(24))));
            }
        }
    }
    rows
}

/// Wrap complete identifiers instead of clipping them into unusable prefixes.
pub(crate) fn lines(view: &dyn View, width: usize) -> Vec<ratatui::text::Line<'static>> {
    use crate::terminal_host::theme::{ACCENT, FG};
    use lattice::wrap::{self, Run};
    use ratatui::{
        style::Style,
        text::{Line, Span},
    };
    let mut lines = super::compose::lines(super::AT_BACKGROUND, width, Vec::new(), Vec::new());
    let available = width.saturating_sub(2).max(1);
    for (alias, note) in rows(view, width) {
        let runs = if alias.is_empty() {
            vec![Run::new(note, 0)]
        } else {
            vec![Run::new(format!("{alias}  "), 1), Run::new(note, 0)]
        };
        for row in wrap::wrap(&runs, available) {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(row.into_iter().map(|run| {
                Span::styled(
                    run.text,
                    Style::default().fg(if run.style == 1 { ACCENT } else { FG }),
                )
            }));
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}
