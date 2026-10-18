//! Viewport-aware table layout with width-independent content coordinates.
use super::{LayoutLine, Line, LineLayout, Span, Style};
use std::ops::Range;
mod cell;
#[cfg(test)]
fn cell_width(cell: &[Span]) -> usize {
    cell::width(cell, unicode_width::UnicodeWidthStr::width)
}

#[cfg(test)]
mod tests;

const MIN_COLUMN: usize = 8;

fn header(cell: &[Span]) -> Line {
    cell.iter()
        .map(|span| {
            let mut span = span.clone();
            if span.style == Style::Plain {
                span.style = Style::Bold;
            }
            span
        })
        .collect()
}

fn positioned(line: Line, source_line: usize, ranges: Vec<Range<usize>>) -> LayoutLine {
    let byte = ranges.first().map_or(0, |range| range.start);
    LayoutLine {
        line,
        layout: LineLayout {
            source_line,
            byte,
            ranges,
            prewrapped: true,
        },
    }
}

struct Field {
    label: Line,
    label_byte: usize,
    value_byte: usize,
}

// Reserve a blank separator and the complete row marker, then address each
// label and value. Every generated fragment remains individually seekable.
fn fields(headings: &[Line], row: &[Line], row_number: usize) -> Vec<Field> {
    let mut byte = format!("Row {row_number}").len() + 2;
    row.iter()
        .enumerate()
        .map(|(col, value)| {
            let label = if row_number == 0 {
                vec![Span::new(format!("Column {}:", col + 1), Style::Marker)]
            } else {
                let mut label = if cell::len(&headings[col]) == 0 {
                    vec![Span::new(format!("Column {}", col + 1), Style::Bold)]
                } else {
                    headings[col].clone()
                };
                label.push(Span::new(":", Style::Plain));
                label
            };
            let label_byte = byte;
            let value_byte = label_byte + cell::len(&label) + 1;
            byte = value_byte + cell::len(value) + 1;
            Field {
                label,
                label_byte,
                value_byte,
            }
        })
        .collect()
}

pub(super) fn layout(
    rows: &[Vec<Line>],
    width: usize,
    source: usize,
    measure: fn(&str) -> usize,
) -> Vec<LayoutLine> {
    let cell_width = |cell: &[Span]| cell::width(cell, measure);
    let cols = rows.first().map_or(0, Vec::len);
    if cols == 0 {
        return Vec::new();
    }
    let rows: Vec<Vec<Line>> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| {
            row.iter()
                .map(|cell| {
                    let normalized = cell::normalize(cell);
                    if at == 0 {
                        header(&normalized)
                    } else {
                        normalized
                    }
                })
                .collect()
        })
        .collect();
    let natural: Vec<_> = (0..cols)
        .map(|col| {
            rows.iter()
                .map(|row| cell_width(&row[col]))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    let mut widths: Vec<_> = natural.iter().map(|&n| n.min(MIN_COLUMN)).collect();
    let frame = cols.saturating_mul(3).saturating_add(1);
    let minimum = widths.iter().sum::<usize>().saturating_add(frame);
    if minimum > width {
        return stacked(&rows, width, source, measure);
    }
    let mut spare = width - minimum;
    while spare > 0 {
        let mut grew = false;
        for (current, wanted) in widths.iter_mut().zip(&natural) {
            if *current < *wanted && spare > 0 {
                *current += 1;
                spare -= 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut out = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let source_line = source + if at == 0 { 0 } else { at + 1 };
        let fields = fields(&rows[0], row, at);
        let wrapped: Vec<_> = row
            .iter()
            .zip(&widths)
            .map(|(cell, &width)| cell::wrap(cell, width, measure))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line in 0..height {
            let mut physical = vec![Span::new("│", Style::TableRule)];
            let mut ranges = Vec::new();
            for ((cell, &width), field) in wrapped.iter().zip(&widths).zip(&fields) {
                let piece = cell.get(line);
                let text = piece.map(|piece| piece.line.as_slice()).unwrap_or(&[]);
                if let Some(piece) = piece {
                    ranges.push(
                        field.value_byte + piece.bytes.start..field.value_byte + piece.bytes.end,
                    );
                }
                if line == 0 {
                    // A label hidden in grid mode belongs to this cell's first
                    // row, never to the end of the preceding column's value.
                    ranges.push(field.label_byte..field.value_byte);
                }
                physical.push(Span::new(" ", Style::Plain));
                physical.extend_from_slice(text);
                physical.push(Span::new(
                    " ".repeat(width.saturating_sub(cell_width(text)) + 1),
                    Style::Plain,
                ));
                physical.push(Span::new("│", Style::TableRule));
            }
            if line == 0 {
                ranges.push(0..format!("Row {at}").len() + 2);
            }
            out.push(positioned(physical, source_line, ranges));
        }
        if at == 0 {
            let rule = widths
                .iter()
                .map(|width| "─".repeat(width + 2))
                .collect::<Vec<_>>()
                .join("┼");
            out.push(positioned(
                vec![Span::new(format!("├{rule}┤"), Style::TableRule)],
                source + 1,
                std::iter::once(0..1).collect(),
            ));
        }
    }
    out
}

fn stacked(
    rows: &[Vec<Line>],
    width: usize,
    source: usize,
    measure: fn(&str) -> usize,
) -> Vec<LayoutLine> {
    let mut out = Vec::new();
    let headings = &rows[0];
    if rows.len() == 1 {
        for (heading, field) in headings.iter().zip(fields(headings, headings, 0)) {
            for (text, base) in [
                (&field.label, field.label_byte),
                (heading, field.value_byte),
            ] {
                for piece in cell::wrap(text, width, measure) {
                    out.push(positioned(
                        piece.line,
                        source,
                        std::iter::once(base + piece.bytes.start..base + piece.bytes.end).collect(),
                    ));
                }
            }
        }
        return out;
    }
    for (at, row) in rows.iter().enumerate().skip(1) {
        let source_line = source + at + 1;
        if at > 1 {
            out.push(positioned(
                Vec::new(),
                source_line,
                std::iter::once(0..1).collect(),
            ));
        }
        // The first row marker also stands in for a header that is no longer
        // separate in stacked mode, so an old header anchor stays at this table.
        for piece in cell::wrap(
            &[Span::new(format!("Row {at}"), Style::Marker)],
            width,
            measure,
        ) {
            out.push(positioned(
                piece.line,
                if at == 1 { source } else { source_line },
                std::iter::once(1 + piece.bytes.start..1 + piece.bytes.end).collect(),
            ));
        }
        for (value, field) in row.iter().zip(fields(headings, row, at)) {
            let indent = if width >= 4 { 2 } else { 0 };
            for (text, base) in [(&field.label, field.label_byte), (value, field.value_byte)] {
                for mut piece in cell::wrap(text, width.saturating_sub(indent).max(1), measure) {
                    piece
                        .line
                        .insert(0, Span::new(" ".repeat(indent), Style::Plain));
                    out.push(positioned(
                        piece.line,
                        source_line,
                        std::iter::once(base + piece.bytes.start..base + piece.bytes.end).collect(),
                    ));
                }
            }
        }
    }
    out
}
