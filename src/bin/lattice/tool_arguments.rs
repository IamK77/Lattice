//! Tool request presentation: one operation heading and subordinate parameters.
//! Expanded values retain their original lines; wrapping belongs to the caller.

use super::text::{clip, fit};
use super::theme::{CODE_FG, DIM};
use lattice::wrap;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

#[cfg(test)]
#[path = "tool_arguments/tests.rs"]
mod tests;

/// Keep the operation in the heading and move incidental parameters below it.
/// Expansion reveals the actual argument lines, not a flattened JSON sentence.
pub(super) fn tool_argument_layout(
    args: &Value,
    expanded: bool,
    head_room: usize,
    detail_room: usize,
) -> (String, Vec<Line<'static>>) {
    let Some(map) = args.as_object().filter(|map| !map.is_empty()) else {
        return (format_tool_args(args, head_room), Vec::new());
    };
    let primary = [
        "command",
        "pattern",
        "query",
        "q",
        "url",
        "path",
        "operation",
        "prompt",
        "glob",
        "target",
        "name",
    ]
    .into_iter()
    .find(|key| map.contains_key(*key))
    .unwrap_or_else(|| {
        map.iter()
            .find(|(_, value)| value.is_string())
            .map(|(key, _)| key.as_str())
            .unwrap_or_else(|| map.keys().next().expect("nonempty arguments"))
    });
    let value = &map[primary];
    let raw = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let first = raw.lines().next().unwrap_or_default();
    let summary = if !expanded && raw.lines().count() > 1 {
        clip(
            &format!("{first}  · {} lines", raw.lines().count()),
            head_room,
        )
    } else {
        fit(first, head_room)
    };
    let mut details = Vec::new();
    if !expanded {
        let rest: serde_json::Map<String, Value> = map
            .iter()
            .filter(|(key, _)| key.as_str() != primary)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if !rest.is_empty() {
            let text = if rest.len() == 1 {
                let (key, value) = rest.iter().next().expect("one argument");
                let label = format!("{key} ");
                clip(
                    &format!(
                        "{label}{}",
                        short_val(value, detail_room.saturating_sub(wrap::str_cols(&label)))
                    ),
                    detail_room,
                )
            } else {
                format_tool_args(&Value::Object(rest), detail_room)
            };
            details.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(DIM)),
                Span::styled(text, Style::default().fg(DIM)),
            ]));
        }
        return (summary, details);
    }
    let label_width = map
        .keys()
        .map(|key| wrap::str_cols(key))
        .max()
        .unwrap_or(0)
        .min(16);
    for (key, value) in std::iter::once((primary, value)).chain(
        map.iter()
            .filter(|(key, _)| key.as_str() != primary)
            .map(|(key, value)| (key.as_str(), value)),
    ) {
        if key == primary && raw.lines().count() <= 1 && summary == raw {
            continue;
        }
        let text = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(value).expect("JSON value"));
        for (index, line) in text.split('\n').enumerate() {
            let label = if index == 0 {
                clip(key, label_width)
            } else {
                String::new()
            };
            let padding = " ".repeat(label_width.saturating_sub(wrap::str_cols(&label)) + 2);
            details.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(DIM)),
                Span::styled(format!("{label}{padding}"), Style::default().fg(DIM)),
                Span::styled(line.to_owned(), Style::default().fg(CODE_FG)),
            ]));
        }
    }
    (summary, details)
}

/// A single argument shows its value; several share the available columns.
fn format_tool_args(args: &Value, room: usize) -> String {
    let joined = match args {
        Value::Object(map) if map.len() == 1 => short_val(map.values().next().unwrap(), room),
        Value::Object(map) => {
            let parts: Vec<(&String, String)> = map.iter().map(|(k, v)| (k, raw_val(v))).collect();
            let joins = parts.len().saturating_sub(1) * 2;
            let natural: Vec<usize> = parts
                .iter()
                .map(|(k, v)| wrap::str_cols(k) + 1 + wrap::str_cols(v))
                .collect();
            let budgets = shares(&natural, room.saturating_sub(joins));
            parts
                .iter()
                .zip(budgets)
                .map(|((k, v), budget)| {
                    // Clip the WHOLE pair: a share can be smaller than its key.
                    let label = wrap::str_cols(k) + 1;
                    let pair = format!("{k}={}", fit(v, budget.saturating_sub(label)));
                    clip(&pair, budget)
                })
                .collect::<Vec<_>>()
                .join("  ")
        }
        Value::Null => String::new(),
        other => short_val(other, room),
    };
    // The head of a card gets ONE line, whatever the sharing worked out to.
    clip(&joined, room)
}

/// Short values take only what they need, leaving space for longer ones.
/// Equal shares would cut a path while tiny numeric fields waste columns.
fn shares(natural: &[usize], room: usize) -> Vec<usize> {
    let mut out = natural.to_vec();
    if natural.iter().sum::<usize>() <= room {
        return out;
    }
    let mut left = room;
    let mut over: Vec<usize> = (0..natural.len()).collect();
    while !over.is_empty() {
        let each = left / over.len();
        let (fits, rest): (Vec<usize>, Vec<usize>) =
            over.iter().partition(|&&i| natural[i] <= each);
        if fits.is_empty() {
            for &i in &rest {
                out[i] = each;
            }
            return out;
        }
        for &i in &fits {
            out[i] = natural[i];
            left -= natural[i];
        }
        over = rest;
    }
    out
}

fn short_val(v: &Value, room: usize) -> String {
    fit(&raw_val(v), room)
}

/// One argument's value as text, newlines shown rather than breaking the line.
fn raw_val(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
    .replace('\n', "⏎")
}
