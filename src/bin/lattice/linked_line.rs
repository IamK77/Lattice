//! A styled terminal line and its link destinations travel together through
//! wrapping. This layer knows neither Markdown nor transcript hit geometry.

use lattice::wrap;
use ratatui::text::{Line, Span};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct LinkedLine {
    pub(super) line: Line<'static>,
    /// Destination per span; absent trailing entries mean plain text.
    pub(super) links: Vec<Option<String>>,
}

/// Wrap via opaque ids so both styles and link destinations survive.
pub(super) fn wrap_linked_line(
    line: &Line,
    links: &[Option<String>],
    width: usize,
) -> Vec<LinkedLine> {
    // The opaque wrapping id names BOTH presentation and destination. Two
    // adjacent links can look identical without pointing to the same place.
    let mut styles = Vec::new();
    let runs: Vec<wrap::Run> = line
        .spans
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let pair = (s.style, links.get(i).cloned().flatten());
            let id = styles.iter().position(|st| *st == pair).unwrap_or_else(|| {
                styles.push(pair);
                styles.len() - 1
            });
            wrap::Run::new(s.content.to_string(), id)
        })
        .collect();
    wrap::wrap(&runs, width)
        .into_iter()
        .map(|row| {
            let links = row.iter().map(|r| styles[r.style].1.clone()).collect();
            let line = Line::from(
                row.into_iter()
                    .map(|r| Span::styled(r.text, styles[r.style].0))
                    .collect::<Vec<_>>(),
            );
            LinkedLine { line, links }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    #[test]
    fn identical_styles_keep_distinct_targets_and_plain_trailing_spans() {
        let style = Style::default().fg(Color::Blue);
        let line = Line::from(vec![
            Span::styled("first", style),
            Span::styled("second", style),
            Span::styled("plain", style),
        ]);
        let links = vec![
            Some("https://one.example/".into()),
            Some("https://two.example/".into()),
        ];
        for width in [3, 7, 30] {
            let mut texts = [String::new(), String::new(), String::new()];
            for row in wrap_linked_line(&line, &links, width) {
                assert_eq!(row.line.spans.len(), row.links.len());
                for (span, link) in row.line.spans.iter().zip(row.links) {
                    assert_eq!(span.style, style);
                    let index = match link.as_deref() {
                        Some("https://one.example/") => 0,
                        Some("https://two.example/") => 1,
                        None => 2,
                        other => panic!("unexpected target: {other:?}"),
                    };
                    texts[index].push_str(&span.content);
                }
            }
            assert_eq!(texts, ["first", "second", "plain"], "width={width}");
        }
    }
}
