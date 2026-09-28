//! Static help and the current session's identifying facts.
use crate::terminal_host::slash_catalog::SLASH;
use lattice::view::View;

pub(crate) fn commands() -> Vec<(String, String)> {
    SLASH
        .iter()
        .map(|c| (c.name.to_string(), c.summary.to_string()))
        .collect()
}

pub(crate) fn keys() -> Vec<(String, String)> {
    // One binding per row: a second key hidden in a description is not a key
    // the reader can find by scanning the key column.
    [
        ("Enter", "send"),
        ("Alt-Enter", "new line"),
        ("↑ ↓", "input history · pick a /command"),
        ("← →", "move the cursor"),
        ("Ctrl/Alt-← →", "move it by word"),
        ("Ctrl-A / Ctrl-E", "jump to line start / end"),
        ("Ctrl-W / U / K", "delete word / to start / to end"),
        ("Ctrl-V", "attach the picture on the clipboard"),
        ("Tab", "complete a /command"),
        ("wheel / PgUp/PgDn", "scroll the transcript"),
        ("Ctrl-O / click", "fold or unfold a tool's output"),
        ("Esc", "interrupt · back to the bottom"),
        ("Ctrl-C", "clear the input line (interrupt when busy)"),
        ("Ctrl-L", "clear the screen (the log keeps it)"),
        ("F1", "this panel"),
        ("Ctrl-D", "quit"),
    ]
    .iter()
    .map(|(k, d)| (k.to_string(), d.to_string()))
    .collect()
}

pub(crate) fn session(view: &dyn View) -> Vec<(String, String)> {
    let mut rows = vec![("version".to_string(), lattice::VERSION.to_string())];
    if let Some(now) = view.models().current() {
        rows.push(("model".to_string(), now.model.clone()));
        rows.push(("endpoint".to_string(), now.endpoint.clone()));
        rows.push(("dialect".to_string(), now.dialect.clone()));
        rows.push((
            "reads images".to_string(),
            if now.accepts_images { "yes" } else { "no" }.to_string(),
        ));
        if let Some(w) = now.window {
            rows.push(("context".to_string(), format!("{w} tokens")));
        }
    }
    if let Some(now) = view.effort().now.clone() {
        rows.push(("thinking".to_string(), now));
    }
    rows.push(("working in".to_string(), view.title().to_string()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_row_names_exactly_one_binding() {
        let rows = keys();
        for (key, what) in &rows {
            assert!(
                !what.contains("Ctrl-") && !what.contains("Alt-") && !what.contains("F1"),
                "{key:?} hides another binding in its description: {what:?}"
            );
        }
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        for own in ["Alt-Enter", "Ctrl-D", "Enter", "F1"] {
            assert!(keys.contains(&own), "{own} has a row of its own: {keys:?}");
        }
    }

    #[test]
    fn the_panel_carries_the_commands_and_the_keyboard() {
        let text = |rows: Vec<(String, String)>| {
            rows.iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let commands = text(commands());
        assert!(commands.contains("/help") && commands.contains("/clear"));
        let keys = text(keys());
        assert!(
            keys.contains("Ctrl-O") && keys.contains("Alt-Enter") && keys.contains("Esc"),
            "{keys}"
        );
    }
}
