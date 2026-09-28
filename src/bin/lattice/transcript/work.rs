//! Work-run identity, folding threshold, and summary. No UI mutation or reads.
use crate::terminal_host::theme::{DIM, ERR, FG, WARM};
use lattice::view::{Entry, ToolCard, ToolStatus};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub(super) const FOLD_FROM: usize = 2;
const FOLD_CHANGED: usize = 3;

pub(super) fn entry_key(e: &Entry) -> Option<String> {
    match e {
        Entry::Tool(c) => c.call.clone(),
        Entry::Thinking(c) => Some(c.call.clone()),
        _ => None,
    }
}

pub(super) fn work_group(entries: &[Entry], from: usize) -> usize {
    let mut end = from;
    while end < entries.len() && matches!(entries[end], Entry::Tool(_) | Entry::Thinking(_)) {
        end += 1;
    }
    end
}

pub(super) fn tool_count(group: &[Entry]) -> usize {
    group.iter().filter(|e| matches!(e, Entry::Tool(_))).count()
}

/// The same identity is used for rendering and for keyboard folding.
pub(crate) fn group_key(group: &[Entry]) -> Option<String> {
    group.iter().find_map(entry_key)
}

pub(crate) fn folded_work(group: &[Entry], folded: bool) -> Line<'static> {
    let dim = Style::default().fg(DIM);
    let cards: Vec<&ToolCard> = group
        .iter()
        .filter_map(|e| match e {
            Entry::Tool(card) => Some(card),
            _ => None,
        })
        .collect();
    let steps = cards.len();
    let mut changed: Vec<&str> = cards
        .iter()
        .filter(|c| c.status != ToolStatus::Failed)
        .filter_map(|c| c.changed.as_deref())
        .collect();
    // Preserve adjacent-only deduplication and the existing unknown-change text.
    changed.dedup();
    let failed = cards
        .iter()
        .filter(|c| c.status == ToolStatus::Failed)
        .count();
    let plural = if steps == 1 { "step" } else { "steps" };
    let tail = match changed.len() {
        0 => "changed nothing".to_string(),
        n if n <= FOLD_CHANGED => format!("changed {}", join_and(&changed)),
        n => format!("changed {n} files"),
    };
    let mut spans = vec![
        Span::styled(if folded { "" } else { "▾ " }.to_string(), dim),
        Span::styled(
            format!("ran {steps} {plural} and {tail}"),
            if changed.is_empty() {
                dim.add_modifier(Modifier::ITALIC)
            } else {
                Style::default().fg(FG).add_modifier(Modifier::ITALIC)
            },
        ),
    ];
    if failed > 0 {
        spans.push(Span::styled(
            format!(" · {failed} failed"),
            Style::default().fg(ERR),
        ));
    }
    for (status, label) in [
        (ToolStatus::Background, "running in background"),
        (ToolStatus::Unknown, "outcome unknown"),
    ] {
        let count = cards.iter().filter(|card| card.status == status).count();
        if count > 0 {
            spans.push(Span::styled(
                format!(" · {count} {label}"),
                Style::default().fg(WARM),
            ));
        }
    }
    spans.push(Span::styled(
        if folded {
            "  (Ctrl-O)"
        } else {
            "  (Ctrl-O to fold)"
        }
        .to_string(),
        dim,
    ));
    Line::from(spans)
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [only] => only.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(call: Option<&str>, status: ToolStatus, changed: Option<&str>) -> Entry {
        Entry::Tool(ToolCard {
            call: call.map(str::to_string),
            name: "tool".into(),
            args: serde_json::json!({}),
            status,
            output: vec![],
            changed: changed.map(str::to_string),
            edit_diff: None,
        })
    }

    #[test]
    fn work_identity_is_the_first_available_key_and_stops_at_a_reply() {
        let entries = vec![
            card(None, ToolStatus::Ok, None),
            Entry::Thinking(lattice::view::ThinkingCard {
                call: "thought".into(),
                lines: vec![],
            }),
            card(Some("tool"), ToolStatus::Ok, None),
            Entry::Agent("reply".into()),
        ];
        assert_eq!(work_group(&entries, 0), 3);
        assert_eq!(work_group(&entries, 3), 3);
        assert_eq!(tool_count(&entries[..3]), 2);
        assert_eq!(group_key(&entries[..3]).as_deref(), Some("thought"));
        assert_eq!(group_key(&entries[..1]), None);
    }

    #[test]
    fn summary_preserves_failed_background_and_unknown_outcomes() {
        let group = vec![
            card(Some("a"), ToolStatus::Failed, Some("not-written")),
            card(Some("b"), ToolStatus::Background, None),
            card(Some("c"), ToolStatus::Unknown, None),
        ];
        let folded = folded_work(&group, true).to_string();
        assert!(folded.contains("ran 3 steps and changed nothing"));
        assert!(!folded.contains("not-written"));
        assert!(folded.contains("1 failed"));
        assert!(folded.contains("1 running in background"));
        assert!(folded.contains("1 outcome unknown"));
        assert!(folded.ends_with("(Ctrl-O)"));
        assert!(folded_work(&group, false)
            .to_string()
            .ends_with("(Ctrl-O to fold)"));
    }
}
