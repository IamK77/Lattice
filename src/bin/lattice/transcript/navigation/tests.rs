use super::super::render::transcript_range;
use super::*;
use crate::terminal_host::{Entry, Ui};

#[test]
fn viewport_navigation_matches_full_layout_at_both_boundaries_and_in_between() {
    let mut ui = Ui::replayed(&[]);
    let mut fixtures = vec![Vec::new()];
    fixtures.push(
        (0..50)
            .flat_map(|n| {
                [
                    Entry::User(format!("question {n}")),
                    Entry::Agent(format!(
                        "[answer {n}](https://example.com/{n})\nsecond line"
                    )),
                ]
            })
            .collect(),
    );
    for entries in fixtures {
        ui.entries = entries;
        for width in [18, 40] {
            for height in [1, 8, 24] {
                for scroll in [0, 1, 13, 100_000] {
                    ui.browsing.set_offset(scroll);
                    let mut full =
                        crate::terminal_host::brand::brand_art(&ui.domain.title, width, height)
                            .into_iter()
                            .map(|line| Row {
                                line,
                                owner: None,
                                links: Vec::new(),
                            })
                            .collect::<Vec<_>>();
                    full.extend(super::super::wrap(
                        transcript_range(&ui, 'x', width, 0..ui.entries.len(), true),
                        width,
                    ));
                    let offset = full.len().saturating_sub(height).saturating_sub(scroll);
                    let expected = &full[offset..(offset + height).min(full.len())];
                    let actual = page(&ui, 'x', width, height).unwrap();
                    if scroll == 0 {
                        assert!(
                            actual.groups_built <= 2 * height + 5,
                            "default frames must not scan invisible groups"
                        );
                    }
                    assert_eq!(
                        actual.rows.iter().map(|row| &row.row).collect::<Vec<_>>(),
                        expected.iter().collect::<Vec<_>>(),
                        "width={width} height={height} scroll={scroll}"
                    );
                    assert_eq!(actual.before, offset > 0);
                    assert_eq!(actual.after, offset + height < full.len());
                }
            }
        }
    }
}

#[test]
fn anchored_frames_do_not_walk_from_the_end_or_move_when_new_messages_arrive() {
    let mut ui = Ui::replayed(&[]);
    ui.entries = (0..10_000)
        .map(|n| Entry::Agent(format!("message {n}")))
        .collect();
    ui.browsing.set_offset(1);
    let anchor = Position {
        block: Block::Entries(5000),
        line: 0,
        byte: 0,
    };
    ui.browsing.drawn(anchor, true);
    let first = page(&ui, 'x', 40, 8).unwrap();
    assert_eq!(first.rows[0].position, anchor);
    assert!(
        first.groups_built <= 10,
        "anchored rendering must not scan old or newer groups"
    );
    ui.entries
        .extend((0..10_000).map(|n| Entry::Agent(format!("new message {n}"))));
    let second = page(&ui, 'x', 40, 8).unwrap();
    assert_eq!(second.rows[0].position, anchor);
    assert_eq!(second.groups_built, first.groups_built);
    assert_eq!(
        second.rows.iter().map(|row| &row.row).collect::<Vec<_>>(),
        first.rows.iter().map(|row| &row.row).collect::<Vec<_>>()
    );
}

#[test]
fn drawn_anchor_survives_append_resize_and_panel_then_returns_to_bottom() {
    let mut ui = Ui::replayed(&[]);
    ui.entries = (0..1000)
        .map(|n| Entry::Agent(format!("message {n}")))
        .collect();
    let mut term =
        crate::terminal_host::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
    let hit = crate::terminal_host::draw_ui(&mut term, &mut ui).unwrap();
    crate::terminal_host::scroll_by(&mut ui, 20, &hit);
    crate::terminal_host::draw_ui(&mut term, &mut ui).unwrap();
    let anchor = ui.browsing.position().unwrap();
    assert_eq!(anchor.1, 0, "the completed draw settles its movement");
    ui.entries
        .extend((0..1000).map(|n| Entry::Agent(format!("new {n}"))));
    crate::terminal_host::draw_ui(&mut term, &mut ui).unwrap();
    assert_eq!(ui.browsing.position(), Some(anchor));
    let mut term =
        crate::terminal_host::Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
    crate::terminal_host::draw_ui(&mut term, &mut ui).unwrap();
    assert_eq!(ui.browsing.position(), Some(anchor));
    ui.panel.show(crate::terminal_host::AT_BACKGROUND);
    crate::terminal_host::draw_ui(&mut term, &mut ui).unwrap();
    assert_eq!(ui.browsing.position(), Some(anchor));
    ui.panel.dismiss_preserving_details();
    let hit = crate::terminal_host::draw_ui(&mut term, &mut ui).unwrap();
    crate::terminal_host::scroll_by(&mut ui, -100_000, &hit);
    crate::terminal_host::draw_ui(&mut term, &mut ui).unwrap();
    assert_eq!(ui.browsing.offset(), 0);
    assert_eq!(ui.browsing.position(), None);
}

#[test]
fn a_later_card_stays_anchored_when_an_earlier_anonymous_tool_gains_output() {
    let mut ui = Ui::replayed(&[]);
    let tool = |call: Option<&str>| {
        Entry::Tool(crate::terminal_host::ToolCard {
            call: call.map(str::to_owned),
            name: "Read".into(),
            args: serde_json::json!({}),
            status: crate::terminal_host::ToolStatus::Ok,
            output: vec!["result".into()],
            changed: None,
            edit_diff: None,
        })
    };
    ui.entries = vec![tool(None), tool(Some("later"))];
    ui.browsing.expand("later".into());
    ui.browsing.set_offset(1);
    let anchor = Position {
        block: Block::Entries(1),
        line: 0,
        byte: 0,
    };
    ui.browsing.drawn(anchor, true);
    let before = page(&ui, 'x', 40, 1).unwrap();
    assert_eq!(before.rows[0].position, anchor);
    if let Entry::Tool(first) = &mut ui.entries[0] {
        first.output = vec!["one".into(), "two".into(), "three".into()];
    }
    let after = page(&ui, 'x', 40, 1).unwrap();
    assert_eq!(after.rows[0].position, anchor);
    assert_eq!(after.rows[0].row, before.rows[0].row);
    assert_eq!(after.rows[0].row.owner.as_deref(), Some("later"));
}

#[test]
fn hidden_transcripts_have_no_rows_or_click_targets() {
    let mut ui = Ui::replayed(&[]);
    ui.entries = vec![Entry::Agent("hidden".into()); 10_000];
    ui.panel.show(crate::terminal_host::AT_BACKGROUND);
    let actual = page(&ui, 'x', 40, 8).unwrap();
    assert!(actual.rows.is_empty());
    assert_eq!(actual.groups_built, 0);
    assert!(!actual.before && !actual.after);
}

#[test]
fn content_bytes_keep_the_reading_location_when_width_changes() {
    let mut ui = Ui::replayed(&[]);
    let text = (0..100)
        .map(|n| format!("word{n:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    ui.entries = vec![Entry::Agent(text.clone()), Entry::Agent("later".into())];
    ui.browsing.set_offset(1);
    let byte = text.find("word50").unwrap();
    ui.browsing.drawn(
        Position {
            block: Block::Entries(0),
            line: 0,
            byte,
        },
        true,
    );
    for width in [18, 40, 72] {
        let frame = page(&ui, 'x', width, 5).unwrap();
        let line: String = frame.rows[0]
            .row
            .line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(line.contains("word50"), "width={width}: {line}");
        assert!(frame.rows[0].position.byte <= byte);
    }
}
