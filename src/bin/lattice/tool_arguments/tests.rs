use super::*;
use serde_json::json;

#[test]
fn tool_layout_prefers_text_over_flags_and_marks_multiline_summaries() {
    let (summary, _) = tool_argument_layout(
        &json!({"background":false,"instruction":"Inspect the module"}),
        false,
        60,
        60,
    );
    assert_eq!(summary, "Inspect the module");
    let (summary, _) = tool_argument_layout(&json!({"command":"python3\nprint(1)"}), false, 60, 60);
    assert!(summary.contains("2 lines"));
}

#[test]
fn tool_argument_clipping_counts_columns_not_characters() {
    let args = format_tool_args(
        &json!({
            "content": "很长很长很长很长很长很长很长很长很长很长很长很长的内容",
            "path": "notes.md",
        }),
        52,
    );
    assert!(
        wrap::str_cols(&args) <= 52,
        "a card head is one line: {args}"
    );
}

#[test]
fn args_show_a_lone_value_without_a_label() {
    assert_eq!(format_tool_args(&json!({"command":"ls -la"}), 52), "ls -la");
    let multi = format_tool_args(&json!({"path":"a", "content":"x"}), 52);
    assert!(multi.contains("path=a") && multi.contains("content=x"));
}

#[test]
fn short_arguments_leave_their_unused_columns_for_long_values() {
    assert_eq!(shares(&[2, 4, 40], 26), vec![2, 4, 20]);
    assert_eq!(shares(&[2, 4, 40], 60), vec![2, 4, 40]);
    assert_eq!(shares(&[], 0), Vec::<usize>::new());
}

#[test]
fn expanded_values_preserve_lines_instead_of_flattening_or_clipping() {
    let args = json!({"command":"python3\nprint('中文')\n", "cwd":"/a/very/long/path"});
    let (_, details) = tool_argument_layout(&args, true, 6, 8);
    let text = details
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("print('中文')"), "{text}");
    assert!(text.contains("/a/very/long/path"), "{text}");
    assert!(!text.contains('⏎'), "{text}");
}
