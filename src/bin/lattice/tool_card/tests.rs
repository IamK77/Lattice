use super::*;
use serde_json::json;

fn tool(output: Vec<String>) -> ToolCard {
    ToolCard {
        call: Some("test".into()),
        name: "Run".into(),
        args: json!({}),
        status: ToolStatus::Ok,
        output,
        changed: None,
        edit_diff: None,
    }
}

fn tool_lines(card: &ToolCard, expanded: bool, room: usize) -> Vec<(Line<'static>, u16)> {
    render(card, '*', expanded, room, 2, 4)
}

fn text(rows: &[(Line<'static>, u16)]) -> String {
    rows.iter()
        .map(|(line, _)| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn tool_name_capitalizes() {
    assert_eq!(capitalize("run"), "Run");
    assert_eq!(capitalize("read_file"), "Read_file");
    assert_eq!(capitalize(""), "");
}

#[test]
fn unconfirmed_writes_keep_requested_content_and_the_actual_outcome() {
    let mut card = tool(vec!["permission denied".into()]);
    card.name = "Write".into();
    card.args = json!({"path":"file.txt", "content":"requested text"});
    for status in [
        ToolStatus::Running,
        ToolStatus::Failed,
        ToolStatus::Cancelled,
        ToolStatus::Ok,
    ] {
        card.status = status;
        card.changed = None;
        let rows = tool_lines(&card, true, 60);
        let text = text(&rows);
        assert!(
            text.contains("requested text") && text.contains("permission denied"),
            "{text}"
        );
        assert!(!text.contains("Wrote empty file"));
    }
    card.status = ToolStatus::Failed;
    card.changed = Some("file.txt".into());
    assert!(text(&tool_lines(&card, true, 60)).contains("permission denied"));
}

#[test]
fn edit_cards_show_one_complete_colored_diff_only_after_success() {
    let mut card = tool(vec!["- old".into(), "+ preview".into()]);
    card.name = "Edit".into();
    card.changed = Some("example.txt".into());
    card.args = json!({"path":"example.txt", "old":"old", "new":"first\n\n中文\nlast", "all":true});
    let rows = tool_lines(&card, true, 40);
    let content = text(&rows);
    assert!(
        !content.contains("│ old") && !content.contains("│ new"),
        "{content}"
    );
    for (value, color) in [
        ("- old", Color::Rgb(225, 146, 151)),
        ("+ last", Color::Rgb(139, 202, 177)),
        ("+ 中文", Color::Rgb(139, 202, 177)),
    ] {
        let matches: Vec<_> = rows
            .iter()
            .flat_map(|(line, _)| &line.spans)
            .filter(|span| span.content == value)
            .collect();
        assert_eq!(matches.len(), 1, "{content}");
        assert_eq!(matches[0].style.fg, Some(color));
    }
    assert!(
        content.contains("all"),
        "replacement scope must remain visible"
    );
    card.status = ToolStatus::Failed;
    card.output = vec!["text not found".into()];
    let content = text(&tool_lines(&card, true, 40));
    assert!(
        content.contains("text not found") && !content.contains("+ last"),
        "{content}"
    );
    card.name = "Run".into();
    card.status = ToolStatus::Ok;
    card.output = vec!["+ not a diff".into()];
    let rows = tool_lines(&card, true, 40);
    let span = rows
        .iter()
        .flat_map(|(line, _)| &line.spans)
        .find(|span| span.content == "+ not a diff")
        .unwrap();
    assert_eq!(span.style.fg, Some(FG));
}

#[test]
fn tool_cards_separate_the_operation_from_secondary_arguments() {
    let mut card = tool(vec!["Build finished".into()]);
    card.args = json!({"background":true,"command":"cargo test --lib","cwd":"/workspace/Lattice"});
    let rows = tool_lines(&card, false, 90);
    let head = rows[0].0.to_string();
    assert!(head.contains("cargo test --lib"), "{head}");
    assert!(
        !head.contains("background") && !head.contains("cwd"),
        "metadata must not compete with the command: {head}"
    );
    assert!(rows
        .iter()
        .skip(1)
        .any(|(line, _)| line.to_string().contains("cwd")));
    let name = rows[0].0.spans.iter().find(|s| s.content == "Run").unwrap();
    assert_eq!(name.style.fg, Some(ACCENT));
    let output = rows
        .iter()
        .flat_map(|(line, _)| &line.spans)
        .find(|s| s.content == "Build finished")
        .unwrap();
    assert_eq!(output.style.fg, Some(FG), "output needs readable contrast");
}

#[test]
fn expanded_tool_arguments_keep_multiline_code_and_full_values() {
    let mut card = tool(vec![]);
    card.args = json!({"command":"python3 - <<'PY'\nprint('中文')\nPY","cwd":"/a/very/long/workspace/path"});
    let content = text(&tool_lines(&card, true, 36));
    assert!(content.contains("print('中文')"), "{content}");
    assert!(
        content.contains("/a/very/long/workspace/path"),
        "expanded arguments must not be clipped: {content}"
    );
    assert!(
        !content.contains('⏎'),
        "multiline code must have real line breaks: {content}"
    );
}

#[test]
fn compact_tool_argument_rows_fit_narrow_and_wide_terminals() {
    let mut card = tool(vec![]);
    card.args = json!({"command":"printf '中文中文中文中文中文中文'","cwd":"/very/long/工作目录/项目","background":true});
    for width in [24, 36, 80, 160] {
        for (line, indent) in tool_lines(&card, false, width) {
            assert!(
                wrap::str_cols(&line.to_string()) + usize::from(indent) <= width,
                "width={width}: {line}"
            );
        }
    }
}

fn reading(path: &str) -> ToolCard {
    ToolCard {
        call: None,
        name: "read".into(),
        args: json!({"from":80, "limit":120, "path":path}),
        status: ToolStatus::Ok,
        output: vec![],
        changed: None,
        edit_diff: None,
    }
}

fn head_of(card: &ToolCard, inner: usize) -> String {
    tool_lines(card, false, inner)[0].0.to_string()
}

#[test]
fn a_path_gives_up_its_front_and_never_its_end() {
    let path = "/Users/example/dev/Lattice/src/kernel/inspect.rs";
    let card = reading(path);
    for inner in [40usize, 46, 60, 80, 126] {
        let head = head_of(&card, inner);
        let shown = head.split("Read  ").nth(1).expect(&head);
        let tail = shown.trim_start_matches(crate::terminal_host::text::ELLIPSIS);
        assert!(
            path.ends_with(tail) && !tail.is_empty(),
            "at {inner} what is shown is not the path's end: {head}"
        );
    }
    assert!(
        head_of(&card, 126).contains(path),
        "a wide window has room for all of it"
    );
    assert!(
        head_of(&card, 60).contains("inspect.rs"),
        "an ordinary one still names the file"
    );
}

#[test]
fn a_card_head_stays_on_one_line_at_any_width() {
    let card = reading("/Users/example/dev/Lattice/src/kernel/inspect.rs");
    for width in [30usize, 44, 60, 70, 90, 130] {
        let inner = width - 2;
        let lines = tool_lines(&card, false, inner);
        let indent = lines[0].1;
        let text = head_of(&card, inner);
        let used = indent as usize + wrap::str_cols(&text);
        assert!(
            used <= inner,
            "width {width}: the head wants {used} of {inner} — {text:?}"
        );
    }
}

#[test]
fn supplied_indentation_controls_head_budget_and_output_rows() {
    let mut card = tool(vec!["first".into(), "second".into()]);
    card.args = json!({"command":"a very long operation heading that needs clipping"});
    let rows = render(&card, '*', false, 32, 7, 11);
    assert_eq!(rows[0].1, 7);
    assert!(wrap::str_cols(&rows[0].0.to_string()) + 7 <= 32);
    assert_eq!(rows[1].1, 7);
    assert_eq!(rows[2].1, 11);
    card.name = "Edit".into();
    card.args = json!({"path":"file", "old":"a", "new":"b"});
    card.changed = Some("file".into());
    card.edit_diff = Some(lattice::edit_diff::EditDiff::between("a\n", "b\n"));
    let rows = render(&card, '*', true, 32, 7, 11);
    let diff_start = rows
        .iter()
        .position(|(line, _)| line.to_string().starts_with("@@"))
        .unwrap();
    assert!(rows[diff_start..].iter().all(|(_, indent)| *indent == 11));
}
