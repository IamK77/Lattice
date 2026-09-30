//! Reference-panel identities and shared table layout. These rules are shared
//! by panel contents and the outer layout, not inferred independently.

use super::text::clip;

#[cfg(test)]
#[path = "panels/tests.rs"]
mod tests;

#[path = "panels/background.rs"]
pub(super) mod background;
#[path = "panels/components.rs"]
pub(super) mod components;
#[path = "panels/compose.rs"]
pub(super) mod compose;
#[path = "panels/config.rs"]
pub(super) mod config;
#[path = "panels/context.rs"]
pub(super) mod context;
#[path = "panels/experts.rs"]
pub(super) mod experts;
#[path = "panels/models.rs"]
pub(super) mod models;
#[path = "panels/navigation.rs"]
pub(super) mod navigation;
#[path = "panels/reference.rs"]
pub(super) mod reference;
#[path = "panels/usage.rs"]
pub(super) mod usage;

/// Separate panels answer separate questions; navigation stays local to each.
pub(super) const PANELS: &[(&str, &[&str])] = &[
    ("help", &["Commands", "Keys"]),
    ("cost", &["Context", "Usage"]),
    ("setup", &["Session", "Config", "Models", "Components"]),
    ("work", &["Background", "Experts"]),
];
pub(super) const AT_COMMANDS: (usize, usize) = (0, 0);
pub(super) const AT_CONTEXT: (usize, usize) = (1, 0);
pub(super) const AT_USAGE: (usize, usize) = (1, 1);
#[allow(dead_code)] // Reached by arrow navigation from Config.
pub(super) const AT_SESSION: (usize, usize) = (2, 0);
pub(super) const AT_CONFIG: (usize, usize) = (2, 1);
pub(super) const AT_MODELS: (usize, usize) = (2, 2);
pub(super) const AT_COMPONENTS: (usize, usize) = (2, 3);
pub(super) const AT_BACKGROUND: (usize, usize) = (3, 0);
pub(super) const AT_EXPERTS: (usize, usize) = (3, 1);

pub(super) fn panel_tabs(panel: usize) -> &'static [&'static str] {
    PANELS.get(panel).map(|(_, t)| *t).unwrap_or(&[])
}

pub(super) fn thousands(n: u64) -> String {
    thousands_wide(u128::from(n))
}

pub(super) fn thousands_wide(n: u128) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Deliberately CHARACTER count: the current panel key column uses the
/// terminal's one-column arrows rather than the wrapper's ambiguous widths.
pub(super) fn key_column<'a>(keys: impl Iterator<Item = &'a str>) -> usize {
    keys.map(|k| k.chars().count()).max().unwrap_or(0) + 2
}

/// Drop whole columns from the right, rather than gutting every cell. The
/// final column takes the remaining room only if enough remains to read it.
pub(super) fn table_row(cells: &[(&str, usize)], room: usize) -> String {
    const LAST_MIN: usize = 6;
    let mut out = String::new();
    let mut used = 0usize;
    for (i, (text, want)) in cells.iter().enumerate() {
        if i + 1 == cells.len() {
            let left = room.saturating_sub(used);
            if left >= LAST_MIN {
                out.push_str(&clip(text, left));
            }
            break;
        }
        if used + want > room {
            break;
        }
        out.push_str(&format!("{text:<want$}"));
        used += want;
    }
    out.trim_end().to_string()
}
