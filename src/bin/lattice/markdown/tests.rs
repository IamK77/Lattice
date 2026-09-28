use super::*;
use crate::terminal_host::linked_line::wrap_linked_line;

/// The cache changes how often a block is derived, never what it derives.
/// A wrong key could silently serve another entry's colours.
#[test]
fn remembering_a_rendered_block_does_not_change_it() {
    let md = "a line\n\n```rust\nfn main() { let x: u8 = 1; }\n```\n\nand *after*";
    let shape = |lines: &[Line<'static>]| format!("{lines:?}");
    clear_render_cache();
    let uncached = markdown_lines_uncached(md);
    let miss = markdown_lines(md);
    let hit = markdown_lines(md);
    assert_eq!(shape(&miss), shape(&uncached), "the first pass is faithful");
    assert_eq!(
        shape(&hit),
        shape(&uncached),
        "and so is the remembered one"
    );
    let other = markdown_lines("```rust\nfn other() {}\n```");
    assert_ne!(shape(&other), shape(&hit), "keyed by its own text");
}

#[test]
fn a_full_cache_is_cleared_only_by_the_next_miss() {
    clear_render_cache();
    for index in 0..RENDER_CACHE_MAX {
        markdown_rows(&format!("entry {index}"));
    }
    assert_eq!(render_cache_len(), RENDER_CACHE_MAX);
    markdown_rows("entry 0");
    assert_eq!(render_cache_len(), RENDER_CACHE_MAX, "a hit does not evict");
    let fresh = markdown_rows_uncached("a new entry");
    assert_eq!(markdown_rows("a new entry"), fresh);
    assert_eq!(
        render_cache_len(),
        1,
        "a miss clears the full cache then inserts"
    );
    let mut copy = markdown_rows("a new entry");
    copy[0].line = Line::from("mutated copy");
    assert_eq!(
        markdown_rows("a new entry"),
        fresh,
        "callers do not mutate cached lines"
    );
}

/// Code is coloured by syntax, and carries no background of its own.
#[test]
fn a_fenced_block_is_coloured_by_syntax_and_has_no_background() {
    let lines = markdown_lines("```rust\nlet x = \"hi\"; // note\n```");
    assert_eq!(lines.len(), 1);
    let spans = &lines[0].spans;
    assert!(
        spans.iter().all(|s| s.style.bg.is_none()),
        "no background: {spans:?}"
    );
    let colours: std::collections::HashSet<_> = spans.iter().filter_map(|s| s.style.fg).collect();
    assert!(
        colours.len() > 1,
        "keyword, string and comment are told apart: {spans:?}"
    );
    let text: String = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(text, "  let x = \"hi\"; // note", "content is untouched");
}

/// Missing or unknown languages still draw in one flat code colour.
#[test]
fn a_fence_with_no_language_is_still_drawn() {
    for md in ["```\nplain text\n```", "```nosuchlang\nplain text\n```"] {
        let lines = markdown_lines(md);
        assert_eq!(lines.len(), 1, "{md}");
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "  plain text", "{md}");
        assert!(lines[0].spans.iter().all(|s| s.style.bg.is_none()));
    }
}

/// Each block gets ITS OWN language, in the order the blocks appear.
#[test]
fn two_blocks_get_their_own_languages() {
    assert_eq!(
        fence_languages("```rust\na\n```\ntext\n```python\nb\n```"),
        vec!["rust".to_string(), "python".to_string()]
    );
}

#[test]
fn url_links_wrap_without_losing_destinations_or_styles() {
    let md = "- [中文链接很长很长](https://example.com/a)[second](https://example.org/b)";
    let rows = markdown_rows(md);
    assert_eq!(
        rows,
        markdown_rows(md),
        "cached rendering retains destinations"
    );
    let wrapped = wrap_linked_line(&rows[0].line, &rows[0].links, 9);
    let mut labels = std::collections::HashMap::<String, String>::new();
    for row in &wrapped {
        for (span, link) in row.line.spans.iter().zip(&row.links) {
            if let Some(url) = link {
                labels
                    .entry(url.clone())
                    .or_default()
                    .push_str(&span.content);
                assert_eq!(span.style.fg, Some(ACCENT));
                assert!(span.style.add_modifier.contains(Modifier::UNDERLINED));
            }
        }
    }
    assert!(wrapped.len() > 2);
    assert_eq!(labels["https://example.com/a"], "中文链接很长很长");
    assert_eq!(labels["https://example.org/b"], "second");
}
