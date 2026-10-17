//! Cell wrapping uses the supplied renderer metric, not the conservative
//! CJK paragraph budget. Keep graphemes and source byte intervals together.
use super::{Line, Span};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct Piece {
    pub line: Line,
    pub bytes: Range<usize>,
}

pub(super) fn width(cell: &[Span], measure: fn(&str) -> usize) -> usize {
    cell.iter()
        .flat_map(|span| span.text.graphemes(true))
        .map(measure)
        .sum()
}

pub(super) fn len(cell: &[Span]) -> usize {
    cell.iter().map(|span| span.text.len()).sum()
}

pub(super) fn normalize(cell: &[Span]) -> Line {
    let runs: Vec<_> = cell
        .iter()
        .enumerate()
        .map(|(id, span)| crate::wrap::Run::new(&span.text, id))
        .collect();
    crate::wrap::wrap(&runs, usize::MAX)
        .pop()
        .unwrap_or_default()
        .into_iter()
        .map(|run| {
            let mut span = cell[run.style].clone();
            span.text = run.text;
            span
        })
        .collect()
}

pub(super) fn wrap(cell: &[Span], width: usize, measure: fn(&str) -> usize) -> Vec<Piece> {
    let mut tokens = Vec::new();
    let mut base = 0;
    for (id, span) in cell.iter().enumerate() {
        for (at, text) in span.text.grapheme_indices(true) {
            tokens.push((id, text, base + at));
        }
        base += span.text.len();
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < tokens.len() {
        let mut end = start;
        let mut used = 0;
        let mut space = None;
        while end < tokens.len() {
            let columns = measure(tokens[end].1);
            if end > start && used + columns > width {
                break;
            }
            used += columns;
            if tokens[end].1 == " " && end > start {
                space = Some(end);
            }
            end += 1;
        }
        let next = if end < tokens.len() {
            if let Some(at) = space {
                end = at;
                at + 1
            } else {
                end
            }
        } else {
            end
        };
        let mut line: Line = Vec::new();
        let mut previous = None;
        for &(id, text, _) in &tokens[start..end] {
            if previous == Some(id) {
                line.last_mut().unwrap().text.push_str(text);
            } else {
                let mut span = cell[id].clone();
                span.text = text.into();
                line.push(span);
                previous = Some(id);
            }
        }
        let last = tokens[end - 1];
        out.push(Piece {
            line,
            bytes: tokens[start].2..last.2 + last.1.len(),
        });
        start = next;
    }
    if out.is_empty() {
        out.push(Piece {
            line: Vec::new(),
            bytes: 0..0,
        });
    }
    out
}
