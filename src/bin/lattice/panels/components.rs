//! The assembled components and the selected component's actual wiring.

use super::{key_column, table_row};
use lattice::view::View;

pub(crate) fn rows(view: &dyn View, width: usize) -> Vec<(String, String)> {
    let parts = view.components();
    if parts.is_empty() {
        return vec![("assembly".to_string(), "not reported".to_string())];
    }
    let keys: Vec<String> = parts
        .iter()
        .map(|(instance, ..)| format!("▸ {instance}"))
        .collect();
    let col = key_column(keys.iter().map(String::as_str));
    let room = width.saturating_sub(col + 4 + 2);
    const COLS: [usize; 3] = [16, 12, 0];
    let lay = |a: &str, b: &str, c: &str| {
        format!(
            "  {}",
            table_row(&[(a, COLS[0]), (b, COLS[1]), (c, COLS[2])], room)
        )
    };
    let mut rows = vec![(String::new(), lay("component", "runs", "provides"))];
    let sel = view.panel_sel().min(parts.len().saturating_sub(1));
    let open = view.panel_open();
    for (i, (instance, component, runtime, tools, removable, wires)) in
        parts.into_iter().enumerate()
    {
        let mark = match (i == sel, removable) {
            (true, _) => "▸",
            (false, true) => "+",
            (false, false) => " ",
        };
        rows.push((
            format!("{mark} {instance}"),
            lay(&component, runtime, &tools),
        ));
        if i == sel && open {
            if wires.is_empty() {
                rows.push((
                    String::new(),
                    "    on no wires — it is reached some other way".to_string(),
                ));
            }
            for wire in wires {
                rows.push((String::new(), format!("    {wire}")));
            }
        }
    }
    rows.push((String::new(), String::new()));
    rows.push((
        String::new(),
        "  ↑↓ choose · Enter shows its wiring · u removes it · + = installed".to_string(),
    ));
    rows
}
