//! One visible page plus a byte-bounded cache of laid-out display groups.
//! Streaming invalidates the page, not committed groups. No raw events retained.
use super::navigation::{self, Located, Page};
use lattice::view::{TranscriptPosition, View};
use std::io;

#[derive(PartialEq, Eq)]
struct Key {
    width: usize,
    height: usize,
    scroll: usize,
    position: Option<(TranscriptPosition, isize)>,
    folds: u64,
    busy: bool,
    hidden: bool,
    title: String,
}

/// Layout is independent of the animation glyph. Only marked tool headings
/// receive the current glyph when a viewport is copied out.
type GroupRows = std::sync::Arc<Vec<Located>>;
struct CachedGroup {
    first: usize,
    end: usize,
    rows: GroupRows,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct Groups {
    layout: Option<(usize, bool, u64)>,
    rows: std::collections::VecDeque<CachedGroup>,
    bytes: usize,
    #[cfg(test)]
    pub builds: usize,
}
const GROUP_BUDGET: usize = 16 * 1024 * 1024;
const GROUP_LIMIT: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_host::transcript::Row;
    use lattice::view::TranscriptBlock;

    fn rows(bytes: usize) -> GroupRows {
        vec![Located {
            row: Row {
                line: ratatui::text::Line::from("x".repeat(bytes)),
                owner: None,
                links: vec![],
            },
            position: TranscriptPosition {
                block: TranscriptBlock::Entries(0),
                line: 0,
                byte: 0,
            },
            spinner_span: None,
        }]
        .into()
    }
    #[test]
    fn group_cache_bounds_bytes_and_empty_group_count() {
        let mut cache = Groups::default();
        let big = rows(GROUP_BUDGET / 2);
        cache.insert(0, 1, big.clone());
        cache.insert(1, 2, big);
        assert!(
            cache.get(0).is_none(),
            "old rows must be evicted to make room"
        );
        assert!(cache.get(1).is_some());
        assert!(cache.bytes <= GROUP_BUDGET);
        cache.insert(2, 3, rows(GROUP_BUDGET));
        assert!(
            cache.get(2).is_none(),
            "oversized groups must not be retained"
        );
        cache.clear();
        for n in 0..GROUP_LIMIT * 2 {
            cache.insert(n, n + 1, Default::default());
        }
        assert_eq!(
            cache.rows.len(),
            GROUP_LIMIT,
            "zero-byte groups still need a count bound"
        );
        cache.prepare(100, true, 1);
        assert!(
            cache.rows.is_empty(),
            "a layout change invalidates all groups"
        );
    }
}

impl Groups {
    fn prepare(&mut self, width: usize, busy: bool, folds: u64) {
        if self.layout != Some((width, busy, folds)) {
            self.clear();
            self.layout = Some((width, busy, folds));
        }
    }
    fn clear(&mut self) {
        self.rows.clear();
        self.bytes = 0;
    }
    pub fn get(&self, index: usize) -> Option<(usize, usize, GroupRows)> {
        self.rows
            .iter()
            .find(|group| group.first <= index && index < group.end)
            .map(|group| (group.first, group.end, group.rows.clone()))
    }
    pub fn insert(&mut self, first: usize, end: usize, rows: GroupRows) {
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let bytes = rows.capacity() * std::mem::size_of::<Located>()
            + rows
                .iter()
                .map(|located| {
                    let row = &located.row;
                    row.line.spans.capacity() * std::mem::size_of::<ratatui::text::Span<'static>>()
                        + row
                            .line
                            .spans
                            .iter()
                            .map(|s| match &s.content {
                                std::borrow::Cow::Owned(text) => text.capacity(),
                                std::borrow::Cow::Borrowed(_) => 0,
                            })
                            .sum::<usize>()
                        + row.owner.as_ref().map_or(0, String::capacity)
                        + row.links.capacity() * std::mem::size_of::<(usize, usize, String)>()
                        + row
                            .links
                            .iter()
                            .map(|(_, _, url)| url.capacity())
                            .sum::<usize>()
                })
                .sum::<usize>();
        if bytes > GROUP_BUDGET {
            return;
        }
        while self.bytes + bytes > GROUP_BUDGET || self.rows.len() >= GROUP_LIMIT {
            self.bytes -= self
                .rows
                .pop_front()
                .expect("retained bytes have a group")
                .bytes;
        }
        self.bytes += bytes;
        self.rows.push_back(CachedGroup {
            first,
            end,
            rows,
            bytes,
        });
    }
}

#[derive(Default)]
pub(crate) struct Cache {
    saved: Option<(Key, Page)>,
    pub(super) groups: Groups,
    #[cfg(test)]
    pub builds: usize,
}

impl Cache {
    #[cfg(test)]
    pub fn group_builds(&self) -> usize {
        self.groups.builds
    }

    pub fn invalidate(&mut self) {
        self.invalidate_page();
        self.groups.clear();
    }

    pub fn invalidate_page(&mut self) {
        self.saved = None;
    }

    pub fn page(
        &mut self,
        view: &dyn View,
        spinner: char,
        width: usize,
        height: usize,
        folds: u64,
    ) -> io::Result<Page> {
        let key = Key {
            width,
            height,
            scroll: view.scroll(),
            position: view.transcript_position(),
            folds,
            busy: view.busy(),
            hidden: view.panel().is_some() && view.pending_auth().is_none(),
            title: view.title().to_owned(),
        };
        if self
            .saved
            .as_ref()
            .is_none_or(|(previous, _)| previous != &key)
        {
            // Do not retain a stale page if rebuilding it fails.
            self.saved = None;
            self.groups.prepare(width, view.busy(), folds);
            let page =
                navigation::page_cached(view, spinner, width, height, Some(&mut self.groups))?;
            self.saved = Some((key, page));
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        let mut page = self.saved.as_ref().expect("page was built").1.clone();
        for located in &mut page.rows {
            if let Some(at) = located.spinner_span {
                let span = &mut located.row.line.spans[at];
                let rest = &span.content[span
                    .content
                    .chars()
                    .next()
                    .expect("spinner glyph")
                    .len_utf8()..];
                span.content = format!("{spinner}{rest}").into();
            }
        }
        Ok(page)
    }
}
