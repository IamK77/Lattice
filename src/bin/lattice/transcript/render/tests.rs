use super::*;
use std::borrow::Cow;

#[derive(Default)]
struct Fixture {
    busy: bool,
}
impl View for Fixture {
    fn title(&self) -> &str {
        ""
    }
    fn entries(&self) -> &[Entry] {
        &[]
    }
    fn streaming(&self) -> &str {
        ""
    }
    fn input(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn busy(&self) -> bool {
        self.busy
    }
    fn tick(&self) -> usize {
        0
    }
}

#[test]
fn continuation_indents_follow_rendered_markers_in_terminal_columns() {
    for (text, expected) in [
        ("• first", 3),
        ("- first", 2),
        ("* first", 2),
        ("1. first", 3),
        ("12. first", 4),
        ("just a sentence", 0),
        ("-not a list", 0),
    ] {
        assert_eq!(hanging_indent(&Line::from(text)), expected, "{text}");
    }
}

#[test]
fn thought_cards_inline_zero_or_one_line_and_expand_every_line() {
    for count in 0..=3 {
        let card = ThinkingCard {
            call: "thought".into(),
            lines: (0..count).map(|i| format!("reason {i}")).collect(),
        };
        let closed = thinking_lines(&card, false);
        assert_eq!(closed.len(), 1);
        if count <= 1 {
            assert!(!closed[0].0.to_string().contains("Ctrl-O"));
            if count == 1 {
                assert!(closed[0].0.to_string().contains("reason 0"));
            }
        } else {
            assert!(closed[0]
                .0
                .to_string()
                .contains(&format!("{count} lines · Ctrl-O")));
        }
        let open = thinking_lines(&card, true);
        assert_eq!(open.len(), count + 1);
        for i in 0..count {
            assert!(open[i + 1].0.to_string().contains(&format!("reason {i}")));
            assert_eq!(
                open[i + 1].1,
                if i == 0 {
                    INDENT_AGENT
                } else {
                    INDENT_TOOL_OUT
                }
            );
        }
    }
}

fn tool(id: &str) -> Entry {
    Entry::Tool(view::ToolCard {
        call: Some(id.into()),
        name: "Read".into(),
        status: view::ToolStatus::Ok,
        args: serde_json::json!({}),
        output: Vec::new(),
        changed: None,
        edit_diff: None,
    })
}

#[test]
fn kinds_separate_question_tool_and_reply() {
    assert_ne!(entry_kind(&Entry::User("q".into())), entry_kind(&tool("c")));
    assert_ne!(
        entry_kind(&tool("c")),
        entry_kind(&Entry::Agent("r".into()))
    );
    assert_eq!(entry_kind(&tool("a")), entry_kind(&tool("b")));
}

#[test]
fn a_loaded_page_end_is_not_the_live_tail_and_fold_owner_keeps_its_ordinal() {
    let view = Fixture { busy: true };
    let mut loaded = view::TranscriptGroup {
        first: 12,
        total: 20,
        previous: Some(view::TranscriptKind::Anchor),
        entries: vec![tool("first"), tool("second")],
    };
    let folded = render_transcript_group(&view, 'x', 80, &loaded, false);
    assert_eq!(folded.len(), 2);
    assert_eq!(folded[1].entry, Some(12));
    assert_eq!(folded[1].owner.as_deref(), Some("first"));
    assert!(folded[1].line.to_string().contains("ran 2 steps"));
    loaded.total = 14;
    let live = render_transcript_group(&view, 'x', 80, &loaded, false);
    assert!(!live
        .iter()
        .any(|row| row.line.to_string().contains("ran 2 steps")));
    assert!(live
        .iter()
        .any(|row| row.entry == Some(13) && row.owner.as_deref() == Some("second")));
}
