//! Width budgeting for one activity row. Never truncate a numeric field into
//! another number; discard whole fields only after shortening the quotation.
use lattice::view::activity::{TokenCount, TurnSummary};
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

pub(super) fn fit(
    word: &str,
    status: &str,
    summary: TurnSummary,
    state: Option<&str>,
    width: usize,
) -> (String, String) {
    let time = elapsed(summary.elapsed_ms);
    let tokens = format!("↑{} ↓{}", count(summary.input), count(summary.output));
    let phase = state.map(|s| format!(" · {s}")).unwrap_or_default();
    let suffix = format!("({time} · {tokens}{phase})");
    let minimum = if summary.running {
        0
    } else {
        cells(status) + 1
    };
    if cells(&suffix) + minimum <= width {
        let room = width.saturating_sub(cells(&suffix) + 1);
        let word = if !summary.running && cells(word) > room {
            status
        } else {
            word
        };
        return (clip(word, room), suffix);
    }

    // The quotation has gone; the observed state becomes the compact label.
    let label = if summary.running {
        state.unwrap_or("working")
    } else {
        status
    };
    let suffix = format!("({time} {tokens})");
    if cells(label) + 1 + cells(&suffix) <= width {
        return (label.to_owned(), suffix);
    }
    if cells(label) + 1 + cells(&time) <= width {
        return (label.to_owned(), time);
    }
    (clip(label, width), String::new())
}

pub(super) fn cells(text: &str) -> usize {
    Span::raw(text).width()
}

pub(super) fn clip(text: &str, width: usize) -> String {
    if cells(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    for glyph in text.graphemes(true) {
        if cells(&result) + cells(glyph) + 1 > width {
            break;
        }
        result.push_str(glyph);
    }
    result.push('…');
    result
}

fn count(count: TokenCount) -> String {
    match count.known {
        None => "—".into(),
        Some(value) if count.complete => compact(value),
        Some(value) => format!("≥{}", compact(value)),
    }
}

fn compact(value: u64) -> String {
    for (scale, suffix) in [
        (1_000_000_000_000_000_000, "E"),
        (1_000_000_000_000_000, "P"),
        (1_000_000_000_000, "T"),
        (1_000_000_000, "G"),
        (1_000_000, "M"),
        (1_000, "k"),
    ] {
        if value >= scale {
            let whole = value / scale;
            let tenth = (value % scale) / (scale / 10);
            return if tenth == 0 {
                format!("{whole}{suffix}")
            } else {
                format!("{whole}.{tenth}{suffix}")
            };
        }
    }
    value.to_string()
}

fn elapsed(ms: Option<u64>) -> String {
    let Some(ms) = ms else {
        return "—".into();
    };
    let seconds = ms / 1_000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        3600..86400 => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d{:02}h", seconds / 86400, seconds % 86400 / 3600),
    }
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;
