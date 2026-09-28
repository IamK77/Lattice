//! Historical usage presentation. The caller performs the ledger reads;
//! a calendar and a totals table each consume the supplied measured days.

use super::thousands;
use crate::terminal_host::theme::{ATMO, DIM, OCEAN_DK, OCEAN_LT, OCEAN_MD};
use lattice::ledgers::Day;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::collections::BTreeMap;

#[cfg(test)]
#[path = "usage/tests.rs"]
mod tests;

pub(crate) fn rows(days: &BTreeMap<String, Day>) -> Vec<(String, String)> {
    if days.is_empty() {
        return vec![(
            "so far".to_string(),
            "no conversations on disk to count".to_string(),
        )];
    }
    // Preserve the existing meaning: today is the last recorded day, not now.
    let today = days.keys().next_back().cloned().unwrap_or_default();
    let month = &today[..7.min(today.len())];
    let sum = |keep: &dyn Fn(&str) -> bool| {
        days.iter()
            .filter(|(d, _)| keep(d))
            .fold(Day::default(), |mut a, (_, d)| {
                a.calls += d.calls;
                a.prompt += d.prompt;
                a.cached += d.cached;
                a.output += d.output;
                a.reasoning += d.reasoning;
                a.conversations += d.conversations;
                a
            })
    };
    let (t, m, all) = (
        sum(&|d: &str| d == today),
        sum(&|d: &str| d.starts_with(month)),
        sum(&|_: &str| true),
    );
    let pct = |part: u64, whole: u64| match whole {
        0 => "—".to_string(),
        w => format!("{}%", (part as f64 / w as f64 * 100.0).round() as u64),
    };
    let three = |a: String, b: String, c: String| format!("  {a:<14}{b:<14}{c}");
    let mut rows = vec![
        (
            String::new(),
            format!("  {:<14}{:<14}{}", "today", "this month", "all time"),
        ),
        (
            "conversations".to_string(),
            three(
                thousands(t.conversations),
                thousands(m.conversations),
                thousands(all.conversations),
            ),
        ),
        (
            "calls".to_string(),
            three(thousands(t.calls), thousands(m.calls), thousands(all.calls)),
        ),
        (
            "tokens in".to_string(),
            three(
                thousands(t.prompt),
                thousands(m.prompt),
                thousands(all.prompt),
            ),
        ),
        (
            "tokens out".to_string(),
            three(
                thousands(t.output),
                thousands(m.output),
                thousands(all.output),
            ),
        ),
        (
            "cache hits".to_string(),
            three(
                pct(t.cached, t.prompt),
                pct(m.cached, m.prompt),
                pct(all.cached, all.prompt),
            ),
        ),
        (
            "thinking".to_string(),
            three(
                pct(t.reasoning, t.output),
                pct(m.reasoning, m.output),
                pct(all.reasoning, all.output),
            ),
        ),
    ];
    if let Some((busiest, _)) = days.iter().max_by_key(|(_, d)| d.prompt) {
        rows.push((String::new(), String::new()));
        rows.push(("busiest day".to_string(), format!("  {busiest}")));
    }
    rows.push((String::new(), String::new()));
    rows.push((
        String::new(),
        "  counted from the ledgers themselves — compacting or deleting".to_string(),
    ));
    rows.push((
        String::new(),
        "  old conversations takes their numbers with them".to_string(),
    ));
    rows
}

/// Weekdays run down, weeks across. Missing days keep their cells.
pub(crate) fn calendar(days: &BTreeMap<String, Day>, width: usize) -> Vec<Line<'static>> {
    use chrono::{Datelike, NaiveDate};
    if days.is_empty() {
        return Vec::new();
    }
    let parse = |d: &str| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok();
    let Some(last) = days.keys().next_back().and_then(|d| parse(d)) else {
        return Vec::new();
    };
    let weeks = ((width.saturating_sub(8)) / 2).clamp(8, 53);
    let end = last + chrono::Duration::days(6 - last.weekday().num_days_from_monday() as i64);
    let start = end - chrono::Duration::days((weeks * 7 - 1) as i64);
    // Intensity is relative to the full history, not just the visible window.
    let peak = days.values().map(|d| d.prompt).max().unwrap_or(1).max(1);
    let shade = |tokens: u64| -> (char, Color) {
        if tokens == 0 {
            return ('·', Color::Rgb(52, 58, 74));
        }
        match (tokens as f64 / peak as f64 * 4.0).ceil() as u64 {
            0 | 1 => ('▪', OCEAN_DK),
            2 => ('▪', OCEAN_MD),
            3 => ('█', OCEAN_LT),
            _ => ('█', ATMO),
        }
    };
    let mut out = vec![Line::default()];
    let mut header = String::from("       ");
    let mut seen = 0u32;
    for w in 0..weeks {
        let day = start + chrono::Duration::days((w * 7) as i64);
        if day.month() != seen {
            seen = day.month();
            let name = [
                "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                "Dec",
            ][seen as usize];
            if header.len() <= 7 + w * 2 {
                header.push_str(&" ".repeat(7 + w * 2 - header.len()));
                header.push_str(name);
            }
        }
    }
    out.push(Line::from(Span::styled(header, Style::default().fg(DIM))));
    for weekday in 0..7 {
        let label = ["Mon", "", "Wed", "", "Fri", "", ""][weekday];
        let mut spans = vec![Span::styled(
            format!("   {label:<4}"),
            Style::default().fg(DIM),
        )];
        for w in 0..weeks {
            let day = start + chrono::Duration::days((w * 7 + weekday) as i64);
            let tokens = if day > last {
                0
            } else {
                days.get(&day.format("%Y-%m-%d").to_string())
                    .map(|d| d.prompt)
                    .unwrap_or(0)
            };
            let (ch, colour) = shade(tokens);
            spans.push(Span::styled(format!("{ch} "), Style::default().fg(colour)));
        }
        out.push(Line::from(spans));
    }
    out.push(Line::default());
    let busiest = days.iter().max_by_key(|(_, d)| d.prompt);
    let note = match busiest {
        Some((day, d)) => format!(
            "   less ·▪▪██ more            busiest {day} · {} tokens",
            thousands(d.prompt)
        ),
        None => "   less ·▪▪██ more".to_string(),
    };
    out.push(Line::from(Span::styled(note, Style::default().fg(DIM))));
    out.push(Line::default());
    out
}
