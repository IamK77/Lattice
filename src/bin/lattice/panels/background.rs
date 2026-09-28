//! Background work is either running now or armed to act later.

use crate::terminal_host::text::fit;
use lattice::view::View;

pub(crate) fn rows(view: &dyn View, width: usize) -> Vec<(String, String)> {
    let live = view.background();
    if live.is_empty() {
        return vec![(String::new(), "nothing running, nothing armed".to_string())];
    }
    let mut rows = Vec::new();
    let (working, waiting): (Vec<_>, Vec<_>) = live.iter().partition(|l| !l.standing);
    for (heading, group) in [("running", working), ("armed", waiting)] {
        if group.is_empty() {
            continue;
        }
        if !rows.is_empty() {
            rows.push((String::new(), String::new()));
        }
        rows.push((String::new(), heading.to_string()));
        for l in group {
            let note = if l.standing {
                format!("{} · fired {}", l.label, l.fires)
            } else if l.ledger.is_some() {
                format!(
                    "{} · {} tools · {} tokens",
                    l.label,
                    l.tools,
                    compact(l.tokens)
                )
            } else {
                l.label.clone()
            };
            rows.push((l.key.clone(), fit(&note, width.saturating_sub(24))));
        }
    }
    rows
}

fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}
