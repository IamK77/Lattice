//! Context occupancy and growth presentation from measured view facts.
//! Tokens come from the provider; attribution uses byte shares, not a tokenizer.

use super::{thousands, thousands_wide};
use crate::terminal_host::material::accumulates;
use crate::terminal_host::theme::{
    ACCENT, ATMO, DIM, FG, LAND_DK, LAND_LT, LAND_MD, OCEAN_LT, OCEAN_MD,
};
use lattice::view::View;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

#[cfg(test)]
#[path = "context/tests.rs"]
mod tests;

pub(crate) fn rows(view: &dyn View) -> Vec<(String, String)> {
    let mut rows = compaction_rows(view);
    let window = view.models().current().and_then(|m| m.window);
    let Some(report) = view.usage() else {
        // No measurement is not a measurement of zero.
        rows.push((
            "so far".to_string(),
            "nothing sent yet — the reading appears after the first reply".to_string(),
        ));
        if let Some(w) = window {
            rows.push(("window".to_string(), format!("{} tokens", thousands(w))));
        }
        return rows;
    };
    match window {
        Some(w) if w > 0 && !view.composition().is_empty() => {
            let _ = w;
        }
        Some(w) if w > 0 => {
            let fraction = report.call.prompt as f64 / w as f64;
            rows.push((
                "context".to_string(),
                format!(
                    "{}  {}%   {} of {}",
                    bar(fraction, 20),
                    (fraction * 100.0).round() as u64,
                    thousands(report.call.prompt),
                    thousands(w)
                ),
            ));
            rows.push((
                "room left".to_string(),
                format!("{} tokens", thousands(w.saturating_sub(report.call.prompt))),
            ));
        }
        _ => rows.push((
            "prompt".to_string(),
            format!(
                "{} tokens (this model declares no window)",
                thousands(report.call.prompt)
            ),
        )),
    }
    rows.push((String::new(), String::new()));
    rows.push((
        "cache hits".to_string(),
        "  last call        this turn        session".to_string(),
    ));
    let rate = |u: &lattice::Usage| match u.hit_rate() {
        Some(r) => format!("{}%", (r * 100.0).round() as u64),
        None => "—".to_string(),
    };
    rows.push((
        String::new(),
        format!(
            "  {:<16}  {:<15}  {}",
            rate(&report.call),
            rate(&report.turn),
            rate(&report.session)
        ),
    ));
    rows.push((
        "cached".to_string(),
        format!(
            "  {:<16}  {:<15}  {}",
            thousands(report.call.cached),
            thousands(report.turn.cached),
            thousands(report.session.cached)
        ),
    ));
    rows.push((
        "prompt".to_string(),
        format!(
            "  {:<16}  {:<15}  {}",
            thousands(report.call.prompt),
            thousands(report.turn.prompt),
            thousands(report.session.prompt)
        ),
    ));
    rows.push((
        "written".to_string(),
        format!(
            "  {:<16}  {:<15}  {}",
            thousands(report.call.written),
            thousands(report.turn.written),
            thousands(report.session.written)
        ),
    ));
    rows.push((
        "calls".to_string(),
        format!(
            "  {:<16}  {:<15}  {}",
            report.call.calls, report.turn.calls, report.session.calls
        ),
    ));
    rows.push((String::new(), String::new()));
    rows.push((
        "written out".to_string(),
        match report.call.reasoning {
            0 => format!("{} tokens", thousands(report.call.output)),
            r => format!(
                "{} tokens, {} of it thinking",
                thousands(report.call.output),
                thousands(r)
            ),
        },
    ));
    // Source timestamps, not frontend observation time.
    if report.call.millis > 0 {
        let secs = report.call.millis as f64 / 1000.0;
        let rate = if secs > 0.0 {
            format!(" · {:.0} tok/s", report.call.output as f64 / secs)
        } else {
            String::new()
        };
        rows.push(("took".to_string(), format!("{secs:.2}s{rate}")));
    }
    if report.session.millis > 0 && report.session.calls > 0 {
        rows.push((
            "spent waiting".to_string(),
            format!(
                "{:.1}s over {} calls",
                report.session.millis as f64 / 1000.0,
                report.session.calls
            ),
        ));
    }
    rows
}

fn compaction_rows(view: &dyn View) -> Vec<(String, String)> {
    let Some(status) = view.compaction_status() else {
        return Vec::new();
    };
    let state = match (status.in_flight, status.failure.is_some()) {
        (true, true) => "One attempt in progress; automatic compaction remains paused until a summary is applied",
        (true, false) => "Compaction in progress",
        (false, true) => "Automatic compaction is paused; it will not retry on its own",
        (false, false) => "No recorded compaction pause",
    };
    let mut rows = vec![("compaction".into(), state.into())];
    if let Some(failure) = &status.failure {
        rows.push((
            "failure".into(),
            format!("{}: {}", failure.code, failure.message),
        ));
        rows.push(("recorded at".into(), failure.event.clone()));
    }
    rows.push((
        "manual".into(),
        "/compact requests one attempt; /context shows the recorded status".into(),
    ));
    rows.push((String::new(), String::new()));
    rows
}

pub(crate) fn picture(view: &dyn View, width: usize) -> Vec<Line<'static>> {
    let parts = view.composition();
    let Some(report) = view.usage() else {
        return Vec::new();
    };
    let (Some(window), true) = (
        view.models().current().and_then(|m| m.window),
        !parts.is_empty(),
    ) else {
        return Vec::new();
    };
    let measured: u64 = parts.iter().map(|(_, b)| b).sum();
    if window == 0 || measured == 0 {
        return Vec::new();
    }
    const ROWS: usize = 5;
    let cols = width.saturating_sub(6).clamp(20, 96);
    let per_cell = window.div_ceil((ROWS * cols) as u64).max(1);
    let mut cells: Vec<(char, Color)> = Vec::new();
    let mut legend: Vec<(&'static str, u64)> = Vec::new();
    for (kind, bytes) in &parts {
        let tokens = report.call.prompt * bytes / measured;
        legend.push((kind, tokens));
        let n = (tokens / per_cell) as usize;
        cells.extend(std::iter::repeat_n(('█', material_colour(kind)), n));
    }
    let total_cells = ROWS * cols;
    cells.truncate(total_cells);
    let filled = cells.len();
    cells.resize(total_cells, ('·', Color::Rgb(52, 58, 74)));
    let mut out = vec![
        Line::from(Span::styled(
            format!(
                "  what is in the window — one cell is {} tokens",
                thousands(per_cell)
            ),
            Style::default().fg(DIM),
        )),
        Line::default(),
    ];
    for row in cells.chunks(cols) {
        let mut spans = vec![Span::styled("   ", Style::default())];
        for (ch, colour) in row {
            spans.push(Span::styled(ch.to_string(), Style::default().fg(*colour)));
        }
        out.push(Line::from(spans));
    }
    out.push(Line::default());
    let used = report.call.prompt;
    out.push(Line::from(Span::styled(
        format!(
            "   {}% full · {} of {} · {} free · {} cells",
            (used as f64 / window as f64 * 100.0).round() as u64,
            thousands(used),
            thousands(window),
            thousands(window.saturating_sub(used)),
            filled
        ),
        Style::default().fg(FG),
    )));
    out.push(Line::default());
    // One combined legend and growth table instead of repeating the names.
    let (turn, session) = view.growth();
    let scale = |kind: &str, list: &[(&'static str, u64)]| -> u64 {
        list.iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, b)| *b * report.call.prompt / measured.max(1))
            .unwrap_or(0)
    };
    let widest = legend.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    out.push(Line::from(Span::styled(
        format!(
            "   {:<w$}  {:<16}{:>9}  {:>10}  {}",
            "",
            "",
            "in window",
            "this turn",
            "ever",
            w = widest + 3
        ),
        Style::default().fg(DIM),
    )));
    for (kind, tokens) in &legend {
        let share = *tokens as f64 / used.max(1) as f64;
        let (added, ever) = if accumulates(kind) {
            (
                thousands(scale(kind, &turn)),
                thousands(scale(kind, &session)),
            )
        } else {
            ("—".to_string(), "re-sent".to_string())
        };
        out.push(Line::from(vec![
            Span::styled("   █  ", Style::default().fg(material_colour(kind))),
            Span::styled(format!("{kind:<widest$}  "), Style::default().fg(FG)),
            Span::styled(
                format!(
                    "{:<16}",
                    "█".repeat((share * 14.0).round().max(1.0) as usize)
                ),
                Style::default().fg(material_colour(kind)),
            ),
            Span::styled(
                format!("{:>9}  {:>10}  {}", thousands(*tokens), added, ever),
                Style::default().fg(DIM),
            ),
        ]));
    }
    out.push(Line::from(Span::styled(
        "   ≈ split by size · ever = all that entered the window, condensed or not",
        Style::default().fg(DIM),
    )));
    // Show what each turn added, largest first. Keep the smaller-turn total.
    let history = view.history_growth();
    if history.records > 1 && !history.leaders.is_empty() {
        let biggest = history.leaders[0].1.max(1);
        let bar_w = width.saturating_sub(34).clamp(10, 40);
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            "   what each turn added to the window",
            Style::default().fg(DIM),
        )));
        for (turn, grew) in &history.leaders {
            let fill = ((*grew as f64 / biggest as f64) * bar_w as f64).round() as usize;
            out.push(Line::from(vec![
                Span::styled(format!("   turn {turn:<4}"), Style::default().fg(DIM)),
                Span::styled(
                    format!("{:<bar_w$}", "█".repeat(fill.max(1))),
                    Style::default().fg(ACCENT),
                ),
                Span::styled(format!(" +{}", thousands(*grew)), Style::default().fg(FG)),
            ]));
        }
        if history.smaller_count > 0 {
            out.push(Line::from(Span::styled(
                format!(
                    "   {} smaller turns{:>w$} +{}",
                    history.smaller_count,
                    "",
                    thousands_wide(history.smaller_sum),
                    w = bar_w.saturating_sub(6)
                ),
                Style::default().fg(DIM),
            )));
        }
    }
    out.push(Line::default());
    out
}

fn material_colour(kind: &str) -> Color {
    match kind {
        "tool results" => LAND_MD,
        "model replies" => ACCENT,
        "tool declarations" => OCEAN_LT,
        "system prompt" => OCEAN_MD,
        "what you said" => ATMO,
        "thinking" => DIM,
        "condensed summaries" => LAND_LT,
        "tool calls" => LAND_DK,
        _ => DIM,
    }
}

fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0)) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}
