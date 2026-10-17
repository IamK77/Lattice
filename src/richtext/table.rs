//! Viewport-aware table layout. Cells remain separate until every logical row
//! has been wrapped and its tallest cell determines the row height.
use super::{Line, Span, Style};
use crate::wrap;

#[cfg(test)]
mod tests;

// Short columns keep their natural width. Longer columns need at least this
// much room before a grid is preferable to labeled, vertically stacked fields.
const MIN_COLUMN: usize = 8;

fn cell_width(cell: &[Span]) -> usize {
    cell.iter().map(|span| wrap::str_cols(&span.text)).sum()
}

fn wrap_cell(cell: &[Span], width: usize) -> Vec<Line> {
    let runs: Vec<_> = cell
        .iter()
        .enumerate()
        .map(|(id, span)| wrap::Run::new(&span.text, id))
        .collect();
    wrap::wrap(&runs, width)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|run| {
                    let mut span = cell[run.style].clone();
                    span.text = run.text;
                    span
                })
                .collect()
        })
        .collect()
}

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

pub(super) fn layout(rows: &[Vec<Line>], width: usize) -> Vec<Line> {
    let cols = rows.first().map_or(0, Vec::len);
    if cols == 0 {
        return Vec::new();
    }
    // Normalize tabs/control characters before measuring, using the same
    // width policy as final transcript wrapping rather than byte counts.
    let rows: Vec<Vec<Line>> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| {
            row.iter()
                .map(|cell| {
                    let normalized = wrap_cell(cell, usize::MAX).pop().unwrap_or_default();
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
    // One cell has a left/right space; one separator is shared with its neighbor.
    let frame = cols.saturating_mul(3).saturating_add(1);
    let minimum = widths.iter().sum::<usize>().saturating_add(frame);
    if minimum > width {
        return stacked(&rows, width);
    }
    let mut spare = width - minimum;
    // Fairly share remaining room without wasting it on already-short columns.
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
        let wrapped: Vec<_> = row
            .iter()
            .zip(&widths)
            .map(|(cell, &width)| wrap_cell(cell, width))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line in 0..height {
            let mut physical = vec![Span::new("│", Style::TableRule)];
            for (cell, &width) in wrapped.iter().zip(&widths) {
                let piece = cell.get(line).map(Vec::as_slice).unwrap_or(&[]);
                physical.push(Span::new(" ", Style::Plain));
                physical.extend_from_slice(piece);
                physical.push(Span::new(
                    " ".repeat(width.saturating_sub(cell_width(piece)) + 1),
                    Style::Plain,
                ));
                physical.push(Span::new("│", Style::TableRule));
            }
            out.push(physical);
        }
        if at == 0 {
            let rule = widths
                .iter()
                .map(|width| "─".repeat(width + 2))
                .collect::<Vec<_>>()
                .join("┼");
            out.push(vec![Span::new(format!("├{rule}┤"), Style::TableRule)]);
        }
    }
    out
}

fn stacked(rows: &[Vec<Line>], width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    let headings = &rows[0];
    if rows.len() == 1 {
        // A streamed table may have only its header and separator so far.
        for (col, heading) in headings.iter().enumerate() {
            let mut label = vec![Span::new(format!("Column {}: ", col + 1), Style::Marker)];
            label.extend_from_slice(heading);
            out.extend(wrap_cell(&label, width));
        }
        return out;
    }
    for (at, row) in rows.iter().enumerate().skip(1) {
        if at > 1 {
            out.push(Vec::new());
        }
        out.extend(wrap_cell(
            &[Span::new(format!("Row {at}"), Style::Marker)],
            width,
        ));
        for (col, cell) in row.iter().enumerate() {
            let mut label = if cell_width(&headings[col]) == 0 {
                vec![Span::new(format!("Column {}", col + 1), Style::Bold)]
            } else {
                headings[col].clone()
            };
            label.push(Span::new(":", Style::Plain));
            // Leading spaces also prevent a value beginning with a list marker
            // from acquiring a second hanging indent in the transcript layer.
            // Retain at least two columns for a wide glyph after indentation.
            let indent = if width >= 4 { 2 } else { 0 };
            for value in [&label, cell] {
                for mut line in wrap_cell(value, width.saturating_sub(indent).max(1)) {
                    line.insert(0, Span::new(" ".repeat(indent), Style::Plain));
                    out.push(line);
                }
            }
        }
    }
    out
}
