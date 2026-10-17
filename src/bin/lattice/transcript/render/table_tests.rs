use super::*;
use std::borrow::Cow;

const MD: &str = "| 任务类型与适用场景 | Validation conditions | Expected outcome |\n|---|---|---|\n| 1. first 中文混排 **bold** text | `tests/fixtures/a_very_long_unbroken_identifier.json` | [linked-label-that-must-remain-clickable](https://example.com/table) |\n| B2end | second | last |\n\nOutside paragraph.";

#[derive(Default)]
struct Fixture {
    entries: Vec<Entry>,
    streaming: String,
}
impl View for Fixture {
    fn title(&self) -> &str {
        ""
    }
    fn entries(&self) -> &[Entry] {
        &self.entries
    }
    fn streaming(&self) -> &str {
        &self.streaming
    }
    fn input(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn busy(&self) -> bool {
        !self.streaming.is_empty()
    }
    fn tick(&self) -> usize {
        0
    }
}

fn rows(view: &Fixture, width: usize) -> Vec<super::super::Row> {
    super::super::wrap(transcript_body(view, 'x', width), width)
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn table_viewport_preserves_cell_boundaries_and_finishes_each_logical_row() {
    let view = Fixture {
        entries: vec![Entry::Agent(MD.into())],
        ..Default::default()
    };
    for width in [40, 80, 120] {
        let screen = rows(&view, width);
        let lines: Vec<_> = screen.iter().map(|row| row.line.to_string()).collect();
        eprintln!("--- table viewport {width} ---\n{}", lines.join("\n"));
        assert!(lines.iter().all(|line| wrap::str_cols(line) <= width));
        let rule = lines
            .iter()
            .position(|line| line.contains('├'))
            .expect("table header must have a bounded column rule");
        let body: Vec<Vec<&str>> = lines[rule + 1..]
            .iter()
            .take_while(|line| line.contains('│'))
            .map(|line| {
                let cells: Vec<_> = line.split('│').collect();
                assert_eq!(
                    cells.len(),
                    5,
                    "three complete columns on every physical row: {line}"
                );
                cells[1..4].to_vec()
            })
            .collect();
        let next = body
            .iter()
            .position(|cells| cells[0].trim() == "B2end")
            .expect("second logical row");
        let expected = [
            "1. first 中文混排 bold text",
            "tests/fixtures/a_very_long_unbroken_identifier.json",
            "linked-label-that-must-remain-clickable",
        ];
        for (column, expected) in expected.iter().enumerate() {
            let actual: String = body[..next].iter().map(|cells| cells[column]).collect();
            assert_eq!(
                compact(&actual),
                compact(expected),
                "column {column} at width {width}"
            );
        }
        assert_eq!(
            body[next]
                .iter()
                .map(|cell| cell.trim())
                .collect::<Vec<_>>(),
            ["B2end", "second", "last"]
        );
        assert!(lines.last().unwrap().trim() == "Outside paragraph.");
        assert!(screen
            .iter()
            .flat_map(|row| &row.links)
            .any(|(_, _, url)| url == "https://example.com/table"));
    }
}

#[test]
fn table_viewport_resize_and_stream_completion_keep_content_and_owner_labels() {
    use crate::terminal_host::markdown::{clear_render_cache, render_cache_len};
    clear_render_cache();
    let live = Fixture {
        streaming: MD.into(),
        ..Default::default()
    };
    let committed = Fixture {
        entries: vec![Entry::Agent(MD.into())],
        ..Default::default()
    };
    let text = |screen: Vec<super::super::Row>| {
        screen
            .iter()
            .map(|row| row.line.to_string())
            .filter(|line| line.trim() != "▌")
            .map(|line| line.replace('▌', ""))
            .collect::<Vec<_>>()
    };
    let streamed = text(rows(&live, 80));
    assert_eq!(render_cache_len(), 0, "growing prefixes bypass the cache");
    let wide = text(rows(&committed, 80));
    assert_eq!(streamed, wide);
    eprintln!(
        "--- stacked viewport 24 ---\n{}",
        text(rows(&committed, 24)).join("\n")
    );
    for width in [24, 12, 40, 24, 80] {
        let layout = text(rows(&committed, width));
        assert_eq!(
            layout,
            text(rows(&live, width)),
            "live/final parity at {width}"
        );
        assert!(layout.iter().all(|line| wrap::str_cols(line) <= width));
        if width <= 24 {
            let flat = compact(&layout.join(""));
            assert!(flat.contains("Row1"));
            assert!(flat.contains("Row2"));
            for header in [
                "任务类型与适用场景",
                "Validationconditions",
                "Expectedoutcome",
            ] {
                assert_eq!(
                    flat.matches(header).count(),
                    2,
                    "each row retains header {header}"
                );
            }
            for content in [
                "1.first中文混排boldtext",
                "tests/fixtures/a_very_long_unbroken_identifier.json",
                "linked-label-that-must-remain-clickable",
                "B2end",
                "Outsideparagraph.",
            ] {
                assert!(flat.contains(content), "lost {content} at {width}");
            }
        }
        if width == 80 {
            assert_eq!(layout, wide, "wide cache survives a narrow/wide round trip");
        }
    }
}

#[test]
fn table_viewport_stream_cursor_does_not_rewrap_a_full_table_row() {
    let md = MD.split("\n\n").next().unwrap();
    let live = Fixture {
        streaming: md.into(),
        ..Default::default()
    };
    let committed = Fixture {
        entries: vec![Entry::Agent(md.into())],
        ..Default::default()
    };
    for width in [12, 24, 40, 80, 120] {
        let screen = rows(&live, width);
        let live_text: Vec<_> = screen.iter().map(|row| row.line.to_string()).collect();
        assert!(live_text.iter().all(|line| wrap::str_cols(line) <= width));
        let without_cursor: Vec<_> = live_text
            .iter()
            .filter(|line| line.trim() != "▌")
            .map(|line| line.replace('▌', ""))
            .collect();
        let final_text: Vec<_> = rows(&committed, width)
            .iter()
            .map(|row| row.line.to_string())
            .collect();
        assert_eq!(
            without_cursor, final_text,
            "cursor disturbed table layout at {width}"
        );
    }
}
