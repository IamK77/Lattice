//! Display-group navigation. A physical history page is not a display group:
//! work and consecutive user messages keep their original folding identities.

use super::theme::USER_BG;
#[cfg(test)]
use lattice::view::{Entry, View};
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use render::{hanging_indent, TLine};

#[path = "transcript/render.rs"]
mod render;
#[path = "transcript/work.rs"]
mod work;
#[cfg(test)]
pub(super) use render::{entry_lines, transcript_body};
#[cfg(test)]
use std::ops::Range;
#[cfg(test)]
pub(super) use work::folded_work;
pub(super) use work::group_key;

#[path = "transcript/navigation.rs"]
mod navigation;
pub(super) use navigation::page;
#[path = "transcript/cache.rs"]
mod cache;
pub(super) use cache::Cache;
#[cfg(test)]
#[path = "transcript/cache_tests.rs"]
mod cache_tests;

/// A screen row keeps its click targets beside the exact text they describe.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub line: Line<'static>,
    pub owner: Option<String>,
    pub links: Vec<(usize, usize, String)>,
}

#[cfg(test)]
pub(super) fn wrap(lines: Vec<TLine>, width: usize) -> Vec<Row> {
    wrap_positioned(lines, width)
        .into_iter()
        .map(|positioned| positioned.row)
        .collect()
}

struct Positioned {
    row: Row,
    entry: Option<usize>,
    line: usize,
    byte: usize,
}

fn wrap_positioned(lines: Vec<TLine>, width: usize) -> Vec<Positioned> {
    let mut rows = Vec::new();
    let mut entry = lines.iter().find_map(|line| line.entry);
    let mut logical = 0;
    for tl in lines {
        let next = tl.entry.or(entry);
        if next != entry {
            entry = next;
            logical = 0;
        }
        let text: String = tl
            .line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let mut consumed = 0;
        let indent = tl.indent as usize;
        let hang = hanging_indent(&tl.line);
        let content_width = width.saturating_sub(indent + hang).max(1);
        for (at, wrapped) in
            crate::terminal_host::linked_line::wrap_linked_line(&tl.line, &tl.links, content_width)
                .into_iter()
                .enumerate()
        {
            let rendered: String = wrapped
                .line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            let begin = text
                .get(consumed..)
                .and_then(|tail| tail.find(&rendered))
                .map_or(consumed, |offset| consumed + offset);
            consumed = (begin + rendered.len()).min(text.len());
            let lead = indent + if at > 0 { hang } else { 0 };
            let mut x = lead;
            let mut links = Vec::new();
            for (i, span) in wrapped.line.spans.iter().enumerate() {
                let columns = span.width().min(width.saturating_sub(x));
                if columns > 0 {
                    if let Some(url) = wrapped.links.get(i).and_then(Option::as_ref) {
                        links.push((x, columns, url.clone()));
                    }
                }
                x += span.width();
            }
            let mut spans = Vec::with_capacity(wrapped.line.spans.len() + 2);
            if lead > 0 {
                spans.push(Span::raw(" ".repeat(lead)));
            }
            spans.extend(wrapped.line.spans);
            if tl.hl {
                let used: usize = spans
                    .iter()
                    .map(|s| lattice::wrap::str_cols(&s.content))
                    .sum();
                for span in &mut spans {
                    span.style = span.style.bg(USER_BG);
                }
                if used < width {
                    spans.push(Span::styled(
                        " ".repeat(width - used),
                        Style::default().bg(USER_BG),
                    ));
                }
            }
            rows.push(Positioned {
                row: Row {
                    line: Line::from(spans),
                    owner: tl.owner.clone(),
                    links,
                },
                entry,
                line: logical,
                byte: begin,
            });
        }
        logical += 1;
    }
    rows
}

#[cfg(test)]
pub(super) struct Tail {
    pub rows: Vec<Row>,
    pub more_above: bool,
    #[cfg(test)]
    pub groups_built: usize,
}

/// Fill the bottom viewport backwards without laying out invisible history.
/// A single message still has its own parsing cost; the retained rows are bounded.
#[cfg(test)]
pub(super) fn tail(view: &dyn View, spinner: char, width: usize, height: usize) -> Tail {
    let mut rows = std::collections::VecDeque::new();
    let end = view.entries().len();
    let live = render::transcript_range(view, spinner, width, end..end, true);
    let mut more_above = false;
    #[cfg(test)]
    let mut groups_built = 0;
    let mut prepend = |chunk: Vec<Row>| {
        for row in chunk.into_iter().rev() {
            if rows.len() == height {
                return true;
            }
            rows.push_front(row);
        }
        false
    };
    if prepend(wrap(live, width)) {
        more_above = true;
    } else {
        let mut groups = Groups::new(view.entries()).rev();
        loop {
            // At least the welcome scene is still above a full body viewport.
            if rows.len() == height {
                more_above = true;
                break;
            }
            let Some(range) = groups.next() else {
                break;
            };
            #[cfg(test)]
            {
                groups_built += 1;
            }
            let chunk = wrap(
                render::transcript_range(view, spinner, width, range, false),
                width,
            );
            // Reborrow for each chunk so no whole-history row collection exists.
            for row in chunk.into_iter().rev() {
                if rows.len() == height {
                    more_above = true;
                    break;
                }
                rows.push_front(row);
            }
            if more_above {
                break;
            }
        }
        if !more_above {
            for line in crate::terminal_host::brand::brand_art(view.title(), width, height)
                .into_iter()
                .rev()
            {
                if rows.len() == height {
                    more_above = true;
                    break;
                }
                rows.push_front(Row {
                    line,
                    owner: None,
                    links: Vec::new(),
                });
            }
        }
    }
    Tail {
        rows: rows.into_iter().collect(),
        more_above,
        #[cfg(test)]
        groups_built,
    }
}

#[cfg(test)]
pub(super) struct Groups<'a> {
    entries: &'a [Entry],
    remaining: Range<usize>,
}

#[cfg(test)]
impl<'a> Groups<'a> {
    pub fn new(entries: &'a [Entry]) -> Self {
        Self {
            entries,
            remaining: 0..entries.len(),
        }
    }
}

#[cfg(test)]
fn joins(left: &Entry, right: &Entry) -> bool {
    matches!((left, right), (Entry::User(_), Entry::User(_)))
        || (matches!(left, Entry::Tool(_) | Entry::Thinking(_))
            && matches!(right, Entry::Tool(_) | Entry::Thinking(_)))
}

#[cfg(test)]
impl Iterator for Groups<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }
        let start = self.remaining.start;
        let mut end = start + 1;
        while end < self.remaining.end && joins(&self.entries[end - 1], &self.entries[end]) {
            end += 1;
        }
        self.remaining.start = end;
        Some(start..end)
    }
}

#[cfg(test)]
impl DoubleEndedIterator for Groups<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }
        let end = self.remaining.end;
        let mut start = end - 1;
        while start > self.remaining.start && joins(&self.entries[start - 1], &self.entries[start])
        {
            start -= 1;
        }
        self.remaining.end = start;
        Some(start..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_host::{ThinkingCard, ToolCard, ToolStatus};

    fn entries() -> Vec<Entry> {
        vec![
            Entry::User("first".into()),
            Entry::User("second".into()),
            Entry::Attachment("picture".into()),
            Entry::Thinking(ThinkingCard {
                call: "thought".into(),
                lines: vec!["thinking".into()],
            }),
            Entry::Tool(ToolCard {
                call: Some("call".into()),
                name: "Read".into(),
                args: serde_json::json!({"path":"fixture"}),
                status: ToolStatus::Ok,
                output: vec!["result".into()],
                changed: None,
                edit_diff: None,
            }),
            Entry::Agent("answer".into()),
            Entry::User("next".into()),
        ]
    }

    #[test]
    fn forward_backward_and_interleaved_navigation_keep_complete_groups() {
        let entries = entries();
        let expected = vec![0..2, 2..3, 3..5, 5..6, 6..7];
        assert_eq!(Groups::new(&entries).collect::<Vec<_>>(), expected);
        assert_eq!(
            Groups::new(&entries).rev().collect::<Vec<_>>(),
            expected.iter().rev().cloned().collect::<Vec<_>>()
        );
        let mut groups = Groups::new(&entries);
        assert_eq!(groups.next(), Some(0..2));
        assert_eq!(groups.next_back(), Some(6..7));
        assert_eq!(groups.next_back(), Some(5..6));
        assert_eq!(groups.next(), Some(2..3));
        assert_eq!(groups.next_back(), Some(3..5));
        assert_eq!(groups.next(), None);
        assert_eq!(groups.next_back(), None);
    }

    #[test]
    fn bottom_viewport_matches_full_layout_without_building_invisible_groups() {
        let mut ui = crate::terminal_host::Ui::replayed(&[]);
        for entries in [
            entries(),
            vec![Entry::Agent("[link](https://example.com/)".into()); 10_000],
        ] {
            ui.entries = entries;
            for width in [18, 40] {
                for height in [1, 8, 24] {
                    let actual = tail(&ui, 'x', width, height);
                    let mut expected =
                        crate::terminal_host::brand::brand_art(&ui.domain.title, width, height)
                            .into_iter()
                            .map(|line| Row {
                                line,
                                owner: None,
                                links: Vec::new(),
                            })
                            .collect::<Vec<_>>();
                    expected.extend(wrap(
                        render::transcript_range(&ui, 'x', width, 0..ui.entries.len(), true),
                        width,
                    ));
                    let more = expected.len() > height;
                    let from = expected.len().saturating_sub(height);
                    assert_eq!(actual.rows, expected.split_off(from));
                    assert_eq!(actual.more_above, more);
                    assert!(actual.groups_built <= height + 1);
                    assert!(actual.rows.len() <= height);
                }
            }
        }
    }

    #[test]
    fn grouped_rendering_preserves_spacing_keys_links_and_the_real_live_tail() {
        let mut ui = crate::terminal_host::Ui::replayed(&[]);
        ui.entries = entries();
        let tool = ui.entries[4].clone();
        ui.entries.splice(5..5, vec![tool; 4]);
        ui.live_output
            .seed_reply("[live](https://example.com/)".into());
        ui.live_output.seed_thinking("live thought".into());
        ui.domain.turns.seed_busy(true);
        for expanded in [false, true] {
            if expanded {
                ui.browsing.expand("thought".into());
            }
            let expected = render::transcript_range(&ui, 'x', 40, 0..ui.entries.len(), true);
            let actual = render::transcript_body(&ui, 'x', 40);
            assert_eq!(actual, expected);
        }
    }
}
