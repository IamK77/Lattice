//! Model catalog presentation. The caller supplies the catalog path; this
//! module never reads the environment, writes a model or commits a selection.

use super::{key_column, table_row, thousands};
use lattice::view::{ModelForm, View};

pub(crate) fn rows(view: &dyn View, width: usize, catalog_path: &str) -> Vec<(String, String)> {
    let models = view.models();
    if let Some(form) = view.model_form() {
        let mut rows = vec![(
            String::new(),
            "  a model to add to ~/.lattice/models.json".to_string(),
        )];
        for (i, (label, hint)) in ModelForm::FIELDS.iter().enumerate() {
            let mark = if i == form.at { "▸" } else { " " };
            let value = &form.values[i];
            // The direct key must not be readable in a screenshot.
            let shown = if i == 4 && !value.is_empty() {
                "•".repeat(value.chars().count().min(24))
            } else {
                value.clone()
            };
            let trailing = if i == form.at { "_" } else { "" };
            let after = if value.is_empty() && i != form.at {
                format!("  {hint}")
            } else {
                String::new()
            };
            rows.push((
                format!("{mark} {label}"),
                format!("  {shown}{trailing}{after}"),
            ));
        }
        rows.push((String::new(), String::new()));
        if let Some(problem) = &form.problem {
            rows.push((String::new(), format!("  {problem}")));
            rows.push((String::new(), String::new()));
        }
        rows.push((
            String::new(),
            "  fill ONE of the two key fields — a key written here is a key on \
                     disk this agent can read; a variable name is not"
                .to_string(),
        ));
        rows.push((
            String::new(),
            "  ↑↓/Tab field · Enter add · Esc cancel".to_string(),
        ));
        return rows;
    }
    if models.rows.is_empty() {
        return vec![
            (
                "catalog".to_string(),
                "no models configured — a to add one".to_string(),
            ),
            (String::new(), format!("  {catalog_path}")),
        ];
    }
    let sel = view.panel_sel().min(models.rows.len() - 1);
    let keys: Vec<String> = models
        .rows
        .iter()
        .map(|row| format!("▸ {}", row.id))
        .collect();
    let col = key_column(keys.iter().map(String::as_str));
    let room = width.saturating_sub(col + 4 + 2);
    const COLS: [usize; 3] = [22, 12, 0];
    let lay = |a: &str, b: &str, c: &str| {
        format!(
            "  {}",
            table_row(&[(a, COLS[0]), (b, COLS[1]), (c, COLS[2])], room)
        )
    };
    let mut rows = vec![(String::new(), lay("model", "dialect", "endpoint"))];
    for (i, row) in models.rows.iter().enumerate() {
        let mark = match (Some(i) == models.now, i == sel, row.key_present) {
            (_, true, _) => "▸",
            (true, _, _) => "•",
            (_, _, false) => "!",
            _ => " ",
        };
        rows.push((
            format!("{mark} {}", row.id),
            lay(&row.model, &row.dialect, &row.endpoint),
        ));
        if i == sel && view.panel_open() {
            let say = |k: &str, v: String| (String::new(), format!("    {k:<16}{v}"));
            rows.push(say(
                "context window",
                match row.window {
                    Some(w) => format!("{} tokens", thousands(w)),
                    None => "not declared".to_string(),
                },
            ));
            rows.push(say(
                "reads images",
                if row.accepts_images { "yes" } else { "no" }.to_string(),
            ));
            rows.push(say(
                "thinking",
                if row.rungs.is_empty() {
                    "no rungs declared".to_string()
                } else {
                    row.rungs.join(" · ")
                },
            ));
            rows.push(say(
                "api key",
                format!(
                    "{} ({})",
                    if row.key_present { "set" } else { "NOT SET" },
                    row.key_env
                ),
            ));
        }
    }
    rows.push((String::new(), String::new()));
    if let Some(id) = view.confirm_delete() {
        rows.push((
            String::new(),
            format!("  delete {id} from the catalog? y / any other key"),
        ));
        rows.push((
            String::new(),
            "  gone for good — a key written in it goes too".to_string(),
        ));
    } else {
        rows.push((
            String::new(),
            "  ↑↓ choose · Enter details · s switch · a add · d delete".to_string(),
        ));
        rows.push((String::new(), "  • current · ! no key".to_string()));
    }
    rows
}
