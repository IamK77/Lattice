use super::*;
use crate::richtext::render_width;

fn plain(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn review_table_wrap_consumes_separator_after_a_full_width_word() {
    for (text, first) in [
        ("abcdefgh ijklmnop", "abcdefgh"),
        ("ab cdefg ijklmnop", "ab cdefg"),
    ] {
        let md = format!("| h |\n|---|\n| {text} |");
        let lines = render_width(&md, 12);
        assert_eq!(plain(&lines[2..]), format!("│ {first} │\n│ ijklmnop │"));
    }
}

#[test]
fn table_wrap_separator_preserves_source_bytes_and_styled_fragments() {
    let first = Span::new("éééé", Style::Bold);
    let last = Span {
        text: "wxyz".into(),
        style: Style::Code,
        link: Some("https://example.com/target".into()),
    };
    let pieces = cell::wrap(
        &[first.clone(), Span::new(" ", Style::Plain), last.clone()],
        4,
        |s| s.chars().count(),
    );
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].line, vec![first]);
    assert_eq!(pieces[0].bytes, 0..8);
    assert_eq!(pieces[1].line, vec![last]);
    assert_eq!(
        pieces[1].bytes,
        9..13,
        "consuming a separator must not renumber source bytes"
    );
}

#[test]
fn table_width_boundary_uses_natural_short_columns_and_a_readable_minimum() {
    let md = "| a | b |\n|---|---|\n| 0123456789012345 | abcdefghijklmnop |";
    // Two long columns: 8 + 8 content columns and seven frame columns.
    assert!(plain(&render_width(md, 23)).contains('│'));
    assert!(plain(&render_width(md, 22)).starts_with("Row 1"));
    let short = "| a | b |\n|---|---|\n| x | y |";
    assert!(plain(&render_width(short, 9)).contains("│ x │ y │"));
    assert!(plain(&render_width(short, 8)).starts_with("Row 1"));
    for width in [8, 9, 12, 22, 23, 40, 80] {
        for md in [md, short] {
            assert!(render_width(md, width)
                .iter()
                .all(|line| cell_width(line) <= width));
        }
    }
}

#[test]
fn table_layout_retains_styles_links_and_code_without_linking_padding() {
    let md = "| **Title** | Values |\n|---|---|\n| *斜体中文* **boldtext** | [`path/to/a/long/file.json`](https://example.com/file) |";
    for width in [12, 40, 80] {
        let lines = render_width(md, width);
        let spans: Vec<_> = lines.iter().flatten().collect();
        let text_for = |style| {
            spans
                .iter()
                .filter(|span| span.style == style)
                .map(|span| span.text.as_str())
                .collect::<String>()
        };
        assert!(text_for(Style::Italic).contains("斜体中文"));
        assert!(text_for(Style::Bold).contains("boldtext"));
        let linked: Vec<_> = spans.iter().filter(|span| span.link.is_some()).collect();
        assert_eq!(
            linked
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>(),
            "path/to/a/long/file.json"
        );
        assert!(linked.iter().all(|span| span.style == Style::Code
            && span.link.as_deref() == Some("https://example.com/file")));
        assert!(lines.iter().all(|line| cell_width(line) <= width));
    }
}

#[test]
fn table_layout_keeps_blank_missing_extra_cells_and_header_only_streams() {
    let md = "| | Header |\n|---|---|\n| first | | extra |\n| second |\n\nAfter.";
    let narrow = plain(&render_width(md, 12));
    for text in [
        "Row 1",
        "Row 2",
        "Column 1:",
        "Column 3:",
        "first",
        "second",
        "extra",
        "After.",
    ] {
        assert!(narrow.contains(text), "missing {text}: {narrow}");
    }
    let wide = plain(&render_width(md, 80));
    assert!(wide.contains("│ first  │        │ extra │"));
    assert!(wide.ends_with("\n\nAfter."));
    let header_only = plain(&render_width(
        "| Very long header | Another long header |\n|---|---|",
        12,
    ));
    assert!(header_only.contains("Column 1:"));
    assert!(header_only.contains("Column 2:"));
    assert!(!header_only.contains("Row 1"));
}

#[test]
fn table_layout_keeps_room_for_wide_glyphs_in_the_stacked_indent() {
    for width in 2..=12 {
        let lines = render_width("| 键 | 长字段 |\n|---|---|\n| 值 | 中文 |", width);
        assert!(
            lines.iter().all(|line| cell_width(line) <= width),
            "overflow at {width}: {}",
            plain(&lines)
        );
        let text: String = plain(&lines)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(text.contains("中文"));
    }
}

#[test]
fn table_layout_normalizes_tabs_before_allocating_column_widths() {
    let lines = render_width("| a | b |\n|---|---|\n| a\tb | 中文宽度 |", 23);
    assert!(lines.iter().all(|line| cell_width(line) <= 23));
    assert!(
        plain(&lines).contains("a   b"),
        "tab advances to the next four-column stop"
    );
}
