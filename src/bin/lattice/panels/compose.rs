//! Common panel chrome and row layout, independent of content acquisition.
use super::{key_column, panel_tabs};
use crate::terminal_host::text::clip;
use crate::terminal_host::theme::{ACCENT, DIM, FG};
use lattice::wrap;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub(crate) fn lines(
    at: (usize, usize),
    width: usize,
    picture: Vec<Line<'static>>,
    rows: Vec<(String, String)>,
) -> Vec<Line<'static>> {
    let (panel, tab) = at;
    let mut out = vec![Line::default()];
    let tabs: Vec<Span> = panel_tabs(panel)
        .iter()
        .enumerate()
        .flat_map(|(i, name)| {
            let on = i == tab;
            [
                Span::styled(
                    format!(" {name} "),
                    if on {
                        Style::default()
                            .fg(FG)
                            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                    } else {
                        Style::default().fg(DIM)
                    },
                ),
                Span::styled("  ", Style::default()),
            ]
        })
        .collect();
    let mut head = vec![Span::styled("  ", Style::default())];
    // A narrow panel must name the active tab and indicate its neighbours,
    // rather than clip every name into a strip that cannot be navigated.
    let full: usize = 2 + tabs
        .iter()
        .map(|s| wrap::str_cols(&s.content))
        .sum::<usize>();
    if full > width {
        let names = panel_tabs(panel);
        let here = names.get(tab).copied().unwrap_or_default();
        head.push(Span::styled(
            if tab > 0 { "‹ " } else { "  " },
            Style::default().fg(DIM),
        ));
        head.push(Span::styled(
            format!(" {here} "),
            Style::default()
                .fg(FG)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ));
        head.push(Span::styled(
            if tab + 1 < names.len() { " ›" } else { "" },
            Style::default().fg(DIM),
        ));
    } else {
        head.extend(tabs);
    }
    out.push(Line::from(head));
    out.push(Line::default());
    out.extend(picture);
    // An empty label is either a spacer or a continuation, not a missing row.
    let rows: Vec<(String, String)> = rows
        .into_iter()
        .filter(|(k, v)| k.trim().is_empty() || !v.trim().is_empty())
        .collect();
    let col = key_column(rows.iter().map(|(k, _)| k.as_str()));
    for (k, v) in rows {
        out.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(format!("{k:<col$}"), Style::default().fg(ACCENT)),
            Span::styled(
                clip(&v, width.saturating_sub(col + 4)),
                Style::default().fg(FG),
            ),
        ]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_rows_and_spacers_survive_but_empty_labelled_rows_do_not() {
        let rows = vec![
            ("heading".into(), "value".into()),
            ("".into(), "cache 50%".into()),
            ("".into(), "".into()),
            ("empty".into(), "  ".into()),
        ];
        let lines = lines((1, 0), 80, vec![Line::from("picture")], rows);
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[3].to_string(), "picture");
        assert!(lines[4].to_string().contains("heading"));
        assert!(lines[5].to_string().contains("cache 50%"));
        assert!(lines[6].to_string().trim().is_empty());
    }

    #[test]
    fn narrow_tabs_keep_the_current_name_and_only_existing_neighbours() {
        for (tab, expected) in [
            (0, "     Session  ›"),
            (1, "  ‹  Config  ›"),
            (3, "  ‹  Components "),
        ] {
            let lines = lines((2, tab), 12, vec![], vec![]);
            assert_eq!(lines[1].to_string(), expected);
        }
        let wide = lines((2, 1), 100, vec![], vec![]);
        for name in panel_tabs(2) {
            assert!(wide[1].to_string().contains(name));
        }
    }
}
