//! Logical transcript rendering. Stable entry identities, link destinations,
//! click owners and indentation travel with text; viewport navigation wraps it.
#[cfg(test)]
#[path = "render/tests.rs"]
mod tests;
use super::work::{entry_key, folded_work, group_key, tool_count, work_group, FOLD_FROM};
use crate::terminal_host::markdown::{markdown_lines, markdown_rows, markdown_rows_uncached};
use crate::terminal_host::theme::{ACCENT, DIM, ERR, FG, WARM};
use crate::terminal_host::tool_card;
use lattice::{
    view::{self, Entry, ThinkingCard, View},
    wrap,
};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, PartialEq)]
pub(crate) struct TLine {
    pub entry: Option<usize>,
    pub line: Line<'static>,
    pub links: Vec<Option<String>>,
    pub owner: Option<String>,
    pub indent: u16,
    pub hl: bool,
}

const KIND_ANCHOR: u8 = 0;
const KIND_TOOL: u8 = 1;
const KIND_REPLY: u8 = 2;
const INDENT_AGENT: u16 = 2;
const INDENT_TOOL_OUT: u16 = 4;
const LIVE_THINKING_TAIL: usize = 5;
const THOUGHT_INLINE: usize = 1;

fn entry_kind(e: &Entry) -> u8 {
    transcript_kind(view::TranscriptKind::of(e))
}
fn transcript_kind(kind: view::TranscriptKind) -> u8 {
    match kind {
        view::TranscriptKind::Anchor => KIND_ANCHOR,
        view::TranscriptKind::Work => KIND_TOOL,
        view::TranscriptKind::Reply => KIND_REPLY,
    }
}
fn blank_line() -> TLine {
    TLine {
        entry: None,
        line: Line::default(),
        owner: None,
        indent: 0,
        hl: false,
        links: Vec::new(),
    }
}

/// Continuations line up under a list item's text, in terminal columns.
pub(crate) fn hanging_indent(line: &Line) -> usize {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    for marker in ["• ", "- ", "* "] {
        if text.starts_with(marker) {
            return wrap::str_cols(marker);
        }
    }
    let digits = text.bytes().take_while(|b| b.is_ascii_digit()).count();
    let bytes = text.as_bytes();
    if digits > 0 && bytes.get(digits) == Some(&b'.') && bytes.get(digits + 1) == Some(&b' ') {
        return digits + 2;
    }
    0
}

#[cfg(test)]
pub(crate) fn transcript_body(view: &dyn View, spinner: char, room: usize) -> Vec<TLine> {
    let mut out = Vec::new();
    for range in super::Groups::new(view.entries()) {
        out.extend(transcript_range(view, spinner, room, range, false));
    }
    let end = view.entries().len();
    out.extend(transcript_range(view, spinner, room, end..end, true));
    out
}

/// Test-only full-history adapter. Production reads complete display groups.
#[cfg(test)]
pub(super) fn transcript_range(
    view: &dyn View,
    spinner: char,
    room: usize,
    range: std::ops::Range<usize>,
    include_stream: bool,
) -> Vec<TLine> {
    let all = view.entries();
    let loaded = view::TranscriptGroup {
        first: range.start,
        total: all.len(),
        previous: range
            .start
            .checked_sub(1)
            .map(|at| view::TranscriptKind::of(&all[at])),
        entries: all[range].to_vec(),
    };
    render_transcript_group(view, spinner, room, &loaded, include_stream)
}

pub(super) fn render_transcript_group(
    view: &dyn View,
    spinner: char,
    room: usize,
    loaded: &view::TranscriptGroup,
    include_stream: bool,
) -> Vec<TLine> {
    let mut out: Vec<TLine> = Vec::new();
    let base = loaded.first;
    let mut prev_kind = loaded.previous.map(transcript_kind);
    let entries = &loaded.entries;
    let mut i = 0;
    while i < entries.len() {
        let group = work_group(entries, i);
        if group > i {
            let foldable = tool_count(&entries[i..group]) >= FOLD_FROM;
            // The end of a loaded page is not necessarily the end of history.
            let live = (base + group == loaded.total && view.busy()) || !foldable;
            let key = group_key(&entries[i..group]);
            let open = live || key.as_deref().is_some_and(|k| view.tool_expanded(Some(k)));
            if prev_kind.is_some_and(|p| p != KIND_TOOL) {
                out.push(blank_line());
            }
            prev_kind = Some(KIND_TOOL);
            if open {
                // A finished, unfolded run retains the same toggle target.
                if !live && foldable {
                    out.push(TLine {
                        entry: Some(base + i),
                        line: folded_work(&entries[i..group], false),
                        owner: key.clone(),
                        indent: INDENT_AGENT,
                        hl: false,
                        links: Vec::new(),
                    });
                }
                for (relative, e) in entries[i..group].iter().enumerate() {
                    let owner = entry_key(e);
                    let expanded = owner
                        .as_deref()
                        .is_some_and(|k| view.tool_expanded(Some(k)));
                    for (line, indent) in entry_lines(e, spinner, expanded, room) {
                        out.push(TLine {
                            entry: Some(base + i + relative),
                            line,
                            owner: owner.clone(),
                            indent,
                            hl: false,
                            links: Vec::new(),
                        });
                    }
                }
            } else {
                out.push(TLine {
                    entry: Some(base + i),
                    line: folded_work(&entries[i..group], true),
                    owner: key,
                    indent: INDENT_AGENT,
                    hl: false,
                    links: Vec::new(),
                });
            }
            i = group;
            continue;
        }
        // Consecutive input messages are presented as one thing said.
        if matches!(entries[i], Entry::User(_)) {
            let mut group = i;
            while matches!(entries.get(group), Some(Entry::User(_))) {
                group += 1;
            }
            if group - i > 1 {
                let joined: Vec<&str> = entries[i..group]
                    .iter()
                    .map(|e| match e {
                        Entry::User(text) => text.as_str(),
                        _ => unreachable!("filtered above"),
                    })
                    .collect();
                let merged = Entry::User(joined.join("\n"));
                if prev_kind.is_some_and(|p| p != KIND_ANCHOR) {
                    out.push(blank_line());
                }
                prev_kind = Some(KIND_ANCHOR);
                for (line, indent) in entry_lines(&merged, spinner, false, room) {
                    out.push(TLine {
                        entry: Some(base + i),
                        line,
                        owner: None,
                        indent,
                        hl: true,
                        links: Vec::new(),
                    });
                }
                i = group;
                continue;
            }
        }
        let e = &entries[i];
        i += 1;
        let kind = entry_kind(e);
        if prev_kind.is_some_and(|p| p != kind) {
            out.push(blank_line());
        }
        prev_kind = Some(kind);
        let hl = matches!(e, Entry::User(_));
        // Replies must retain their link destinations, not just styled text.
        if let Entry::Agent(text) = e {
            out.extend(markdown_rows(text).into_iter().map(|row| TLine {
                entry: Some(base + i - 1),
                line: row.line,
                links: row.links,
                owner: None,
                indent: INDENT_AGENT,
                hl: false,
            }));
            continue;
        }
        for (line, indent) in entry_lines(e, spinner, false, room) {
            out.push(TLine {
                entry: Some(base + i - 1),
                line,
                owner: None,
                indent,
                hl,
                links: Vec::new(),
            });
        }
    }
    if !include_stream {
        return out;
    }
    // Show live thinking before the reply, with a bounded tail; the full
    // thought becomes accessible through its card when committed.
    if !view.thinking().is_empty() {
        if prev_kind.is_some_and(|p| p != KIND_TOOL) {
            out.push(blank_line());
        }
        let style = Style::default().fg(DIM).add_modifier(Modifier::ITALIC);
        out.push(TLine {
            entry: None,
            line: Line::from(vec![
                Span::styled("✻ ", Style::default().fg(ACCENT)),
                Span::styled("Thinking", style.add_modifier(Modifier::BOLD)),
            ]),
            owner: None,
            indent: INDENT_AGENT,
            hl: false,
            links: Vec::new(),
        });
        let all: Vec<&str> = view.thinking().lines().collect();
        let tail = all.len().saturating_sub(LIVE_THINKING_TAIL);
        for line in &all[tail..] {
            out.push(TLine {
                entry: None,
                line: Line::from(Span::styled((*line).to_string(), style)),
                owner: None,
                indent: INDENT_TOOL_OUT,
                hl: false,
                links: Vec::new(),
            });
        }
        prev_kind = Some(KIND_TOOL);
    }
    let reply_blank = prev_kind.is_some_and(|p| p != KIND_REPLY);
    if !view.streaming().is_empty() {
        if reply_blank {
            out.push(blank_line());
        }
        // Never cache growing stream prefixes: they cannot hit on the next
        // frame and would retain quadratically many copies of a long reply.
        let mut streamed = markdown_rows_uncached(view.streaming());
        if let Some(last) = streamed.last_mut() {
            last.line
                .spans
                .push(Span::styled("▌", Style::default().fg(ACCENT)));
        }
        out.extend(streamed.into_iter().map(|row| TLine {
            entry: None,
            line: row.line,
            links: row.links,
            owner: None,
            indent: INDENT_AGENT,
            hl: false,
        }));
    }
    out
}

pub(crate) fn entry_lines(
    entry: &Entry,
    spinner: char,
    expanded: bool,
    room: usize,
) -> Vec<(Line<'static>, u16)> {
    match entry {
        Entry::User(text) => text
            .split('\n')
            .enumerate()
            .map(|(i, line)| {
                let marker = if i == 0 { "❯ " } else { "  " };
                (
                    Line::from(vec![
                        Span::styled(
                            marker,
                            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            line.to_string(),
                            Style::default().fg(FG).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    0,
                )
            })
            .collect(),
        Entry::Wake(text) => vec![(
            Line::from(vec![
                Span::styled("↯ ", Style::default().fg(WARM)),
                Span::styled(
                    text.clone(),
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                ),
            ]),
            0,
        )],
        Entry::Agent(text) => markdown_lines(text)
            .into_iter()
            .map(|l| (l, INDENT_AGENT))
            .collect(),
        Entry::Error(text) => vec![(
            Line::from(Span::styled(format!("✗ {text}"), Style::default().fg(ERR))),
            INDENT_AGENT,
        )],
        Entry::Approval(text) => text
            .split('\n')
            .enumerate()
            .map(|(i, line)| {
                let style = if i == 0 {
                    Style::default().fg(ERR).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(ERR)
                };
                let text = if i == 0 {
                    format!("⚠ {line}")
                } else {
                    line.to_string()
                };
                (Line::from(Span::styled(text, style)), INDENT_AGENT)
            })
            .collect(),
        Entry::Tool(card) => {
            tool_card::render(card, spinner, expanded, room, INDENT_AGENT, INDENT_TOOL_OUT)
        }
        Entry::Thinking(card) => thinking_lines(card, expanded),
        Entry::Attachment(name) => vec![(
            Line::from(vec![
                Span::styled("⎿ ", Style::default().fg(DIM)),
                Span::styled(name.clone(), Style::default().fg(ACCENT)),
            ]),
            INDENT_AGENT,
        )],
        Entry::Notice(text) => text
            .split('\n')
            .map(|l| {
                (
                    Line::from(Span::styled(l.to_string(), Style::default().fg(DIM))),
                    INDENT_AGENT,
                )
            })
            .collect(),
    }
}

fn thinking_lines(card: &ThinkingCard, expanded: bool) -> Vec<(Line<'static>, u16)> {
    let style = Style::default().fg(DIM).add_modifier(Modifier::ITALIC);
    let conn = Style::default().fg(DIM);
    let count = card.lines.len();
    let mut head = vec![
        Span::styled("✻ ", Style::default().fg(ACCENT)),
        Span::styled("Thought", style.add_modifier(Modifier::BOLD)),
    ];
    if !expanded && count <= THOUGHT_INLINE {
        head.push(Span::styled(format!("  {}", card.lines.join(" ")), style));
        return vec![(Line::from(head), INDENT_AGENT)];
    }
    if !expanded {
        head.push(Span::styled(
            format!("  ({count} lines · Ctrl-O)"),
            Style::default().fg(DIM),
        ));
    }
    let mut lines = vec![(Line::from(head), INDENT_AGENT)];
    if !expanded {
        return lines;
    }
    for (i, line) in card.lines.iter().enumerate() {
        if i == 0 {
            lines.push((
                Line::from(vec![
                    Span::styled("⎿ ", conn),
                    Span::styled(line.clone(), style),
                ]),
                INDENT_AGENT,
            ));
        } else {
            lines.push((
                Line::from(Span::styled(line.clone(), style)),
                INDENT_TOOL_OUT,
            ));
        }
    }
    lines
}
