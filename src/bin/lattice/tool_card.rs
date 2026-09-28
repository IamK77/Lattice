//! One tool card rendered from recorded facts. Never reopen a file to display
//! a past write. Transcript layout supplies indentation; this module owns no UI.

use super::theme::{ACCENT, CODE_FG, DIM, ERR, FG};
use super::tool_arguments::tool_argument_layout;
use lattice::view::{ToolCard, ToolStatus};
use lattice::wrap;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[cfg(test)]
#[path = "tool_card/tests.rs"]
mod tests;

/// Tool outputs up to this many lines always show in full.
const TOOL_SHORT: usize = 6;

pub(super) fn render(
    card: &ToolCard,
    spinner: char,
    expanded: bool,
    room: usize,
    indent_agent: u16,
    indent_tool_out: u16,
) -> Vec<(Line<'static>, u16)> {
    let (glyph, glyph_style) = match card.status {
        ToolStatus::Running => (spinner.to_string(), Style::default().fg(ACCENT)),
        ToolStatus::Background => ("·".to_string(), Style::default().fg(ACCENT)),
        ToolStatus::Unknown => ("?".to_string(), Style::default().fg(DIM)),
        ToolStatus::Ok => ("✓".to_string(), Style::default().fg(FG)),
        ToolStatus::Failed => ("✗".to_string(), Style::default().fg(ERR)),
        ToolStatus::Cancelled => ("⊘".to_string(), Style::default().fg(DIM)),
    };
    let name = capitalize(&card.name);
    let mut head = vec![
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(
            name.clone(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ];
    // Measure the actual prefix with the wrapper's ruler, including ambiguous
    // glyph widths. An arithmetic approximation previously wrapped the head.
    const GAP: usize = 2;
    let prefix: usize = head.iter().map(|s| wrap::str_cols(&s.content)).sum();
    // Edit's text arguments appear as a confirmed change, not twice as fields.
    // Scope flags and path remain visible even while pending or after failure.
    let edit_sides = (card.name == "Edit")
        .then(|| Some((card.args["old"].as_str()?, card.args["new"].as_str()?)))
        .flatten();
    let confirmed_write =
        (card.name == "Write" && card.status == ToolStatus::Ok && card.changed.is_some())
            .then(|| card.args["content"].as_str())
            .flatten();
    let mut display_args = card.args.clone();
    if confirmed_write.is_some() {
        if let Some(map) = display_args.as_object_mut() {
            map.remove("content");
        }
    }
    if edit_sides.is_some() {
        if let Some(map) = display_args.as_object_mut() {
            map.remove("old");
            map.remove("new");
        }
    }
    let (args, details) = tool_argument_layout(
        &display_args,
        expanded,
        room.saturating_sub(indent_agent as usize + prefix + GAP),
        room.saturating_sub(indent_agent as usize + wrap::str_cols("│ ")),
    );
    if !args.is_empty() {
        head.push(Span::styled(
            format!("  {args}"),
            Style::default().fg(CODE_FG),
        ));
    }
    let mut lines = vec![(Line::from(head), indent_agent)];
    lines.extend(details.into_iter().map(|line| (line, indent_agent)));

    let confirmed_edit =
        edit_sides.filter(|_| card.status == ToolStatus::Ok && card.changed.is_some());
    if expanded && confirmed_edit.is_some() {
        if let Some(diff) = &card.edit_diff {
            lines.extend(edit_diff_lines(diff, indent_tool_out));
            return lines;
        }
    }
    // Legacy results only have bounded previews. Expand the original request
    // after success is confirmed, without pretending it is a file snapshot.
    let full_diff: Vec<String> = if expanded {
        confirmed_edit
            .into_iter()
            .flat_map(|(old, new)| {
                old.lines()
                    .map(|line| format!("- {line}"))
                    .chain(new.lines().map(|line| format!("+ {line}")))
            })
            .collect()
    } else {
        Vec::new()
    };
    // The request contains the complete written text. Show it once, only after
    // success, under the normal fold control. Never reopen the current file.
    let written: Vec<String> = confirmed_write
        .map(|text| {
            if text.is_empty() {
                vec!["Wrote empty file".to_string()]
            } else {
                text.lines().map(str::to_owned).collect()
            }
        })
        .unwrap_or_default();
    let out = if confirmed_write.is_some() {
        &written
    } else if expanded && confirmed_edit.is_some() {
        &full_diff
    } else {
        &card.output
    };
    if out.is_empty() {
        return lines;
    }
    let body = if card.status == ToolStatus::Failed {
        Style::default().fg(ERR)
    } else {
        Style::default().fg(FG)
    };
    // Never infer a diff from arbitrary command output beginning with +/-.
    let output_style = |line: &str| {
        if confirmed_edit.is_some() {
            if line.starts_with("- ") {
                return Style::default().fg(Color::Rgb(225, 146, 151));
            }
            if line.starts_with("+ ") {
                return Style::default().fg(Color::Rgb(139, 202, 177));
            }
        }
        body
    };
    let conn = Style::default().fg(DIM);
    // Changes, failures and a command's output earn room. Run is a display
    // preference, not a security exception; the general rule uses changed.
    let worth_showing = expanded
        || card.status == ToolStatus::Failed
        || card.changed.is_some()
        || card.name == "Run";
    if !worth_showing && out.len() > 1 {
        lines.push((
            Line::from(vec![
                Span::styled("⎿ ", conn),
                Span::styled(format!("{} lines", out.len()), body),
                Span::styled("  (Ctrl-O)".to_string(), Style::default().fg(DIM)),
            ]),
            indent_agent,
        ));
        return lines;
    }
    if expanded || out.len() <= TOOL_SHORT {
        for (i, line) in out.iter().enumerate() {
            if i == 0 {
                lines.push((
                    Line::from(vec![
                        Span::styled("⎿ ", conn),
                        Span::styled(line.clone(), output_style(line)),
                    ]),
                    indent_agent,
                ));
            } else {
                lines.push((
                    Line::from(Span::styled(line.clone(), output_style(line))),
                    indent_tool_out,
                ));
            }
        }
    } else {
        lines.push((
            Line::from(vec![
                Span::styled("⎿ ", conn),
                Span::styled(out[0].clone(), output_style(&out[0])),
                Span::styled(
                    format!("  (+{} lines · Ctrl-O)", out.len() - 1),
                    Style::default().fg(DIM),
                ),
            ]),
            indent_agent,
        ));
    }
    lines
}

/// Render only the captured snapshot, never the file currently on disk.
fn edit_diff_lines(diff: &lattice::edit_diff::EditDiff, indent: u16) -> Vec<(Line<'static>, u16)> {
    use lattice::edit_diff::DiffKind;
    let dim = Style::default().fg(DIM);
    let width = diff
        .hunks
        .iter()
        .map(|h| {
            h.old_start
                .max(h.new_start)
                .saturating_add(h.lines.len())
                .to_string()
                .len()
        })
        .max()
        .unwrap_or(1);
    let mut rows = Vec::new();
    for hunk in &diff.hunks {
        rows.push((
            Line::from(Span::styled(
                format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start),
                dim,
            )),
            indent,
        ));
        let (mut old, mut new) = (hunk.old_start, hunk.new_start);
        for line in &hunk.lines {
            let (left, right, marker, color) = match line.kind {
                DiffKind::Equal => (old.to_string(), new.to_string(), ' ', DIM),
                DiffKind::Delete => (
                    old.to_string(),
                    String::new(),
                    '-',
                    Color::Rgb(225, 146, 151),
                ),
                DiffKind::Insert => (
                    String::new(),
                    new.to_string(),
                    '+',
                    Color::Rgb(139, 202, 177),
                ),
            };
            rows.push((
                Line::from(vec![
                    Span::styled(format!("{left:>width$} {right:>width$} "), dim),
                    Span::styled(
                        format!("{marker} {}", line.text),
                        Style::default().fg(color),
                    ),
                ]),
                indent,
            ));
            if !line.newline {
                rows.push((
                    Line::from(Span::styled("\\ No newline at end of file", dim)),
                    indent,
                ));
            }
            if line.kind != DiffKind::Insert {
                old = old.saturating_add(1);
            }
            if line.kind != DiffKind::Delete {
                new = new.saturating_add(1);
            }
        }
    }
    if diff.truncated {
        rows.push((
            Line::from(Span::styled("… diff snapshot truncated", dim)),
            indent,
        ));
    } else if diff.hunks.is_empty() {
        rows.push((Line::from(Span::styled("No textual changes", dim)), indent));
    }
    rows
}

/// Cosmetic label only: never change the tool name recorded in the ledger.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
