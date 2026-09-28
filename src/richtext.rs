//! A tiny, language-neutral rich-text model, and a lightweight Markdown
//! renderer into it.
//!
//! Agent replies are Markdown; a modern terminal should show headings, bold,
//! inline code, code blocks, lists and quotes as such, not as raw asterisks
//! and backticks. This module turns Markdown into a flat list of styled lines
//! — a NEUTRAL structure (no terminal library in sight), so it is unit-
//! testable here and any frontend (the ratatui binary today, another
//! frontend tomorrow) maps the styles onto its own palette. Keeping the
//! parse in the core and the palette in the frontend is the same
//! data-boundary discipline as the rest of Lattice.
//!
//! The Markdown subset is deliberately small and practical: fenced code
//! blocks, ATX headings, unordered/ordered list items, blockquotes, GFM
//! tables, mathematics, and the inline spans `**bold**`, `*italic*`/`_italic_`, and
//! `` `code` ``. It is a renderer for agent chatter, not a spec-complete
//! CommonMark parser.
//!
//! Mathematics rides along: `\\(…\\)` inline and `$$…$$` or `\\[…\\]` as a
//! block, converted to Unicode by [`crate::mathtext`] and marked [`Style::Math`]
//! so a frontend can colour it. A LONE `$` is deliberately NOT a delimiter —
//! an agent's replies are full of `$PATH`, `$HOME` and `$ARGUMENTS`, and
//! treating those as mathematics would corrupt far more than it rendered.
//!
//! Tables are laid out here (not in the frontend) because alignment is a
//! text property: columns are padded to their widest cell measured in
//! DISPLAY width (CJK characters occupy two terminal cells), the header is
//! bolded, and a rule is drawn beneath it. All alignments are rendered
//! flush-left; escaped pipes and pipes inside inline code are not special.

/// How one run of text should look. The frontend chooses actual colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Bold,
    Italic,
    /// Inline `code`
    Code,
    /// A heading line (level in `Span`-less form: whole line is one heading)
    Heading,
    /// The bullet/number marker of a list item
    Marker,
    /// A line inside a fenced code block
    CodeBlock,
    /// A blockquote line
    Quote,
    /// The rule drawn between a table's header row and its body
    TableRule,
    /// Mathematics, already converted to Unicode (see [`crate::mathtext`])
    Math,
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// A validated HTTP(S) destination, separate from the visible label.
    pub link: Option<String>,
}

impl Span {
    fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
        }
    }
}

/// One rendered line: a sequence of styled spans.
pub type Line = Vec<Span>;

/// Render Markdown into styled lines.
pub fn render(markdown: &str) -> Vec<Line> {
    let raws: Vec<&str> = markdown.split('\n').collect();
    let mut lines: Vec<Line> = Vec::new();
    let mut in_code = false;
    let mut i = 0;

    while i < raws.len() {
        let raw = raws[i];
        let trimmed = raw.trim_start();
        i += 1;

        // Fenced code block toggling on ``` (kept verbatim inside)
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            lines.push(vec![Span::new(raw.to_string(), Style::CodeBlock)]);
            continue;
        }

        // Display mathematics: `$$ … $$` or `\[ … \]`, on one line or
        // spanning several. Each source line becomes one rendered line.
        if let Some((open, close)) = display_fence(trimmed) {
            if let Some(body) = trimmed
                .strip_prefix(open)
                .and_then(|r| r.strip_suffix(close))
                .filter(|_| trimmed.len() > open.len() + close.len())
            {
                lines.push(vec![Span::new(math(body), Style::Math)]);
                continue;
            }
            // An opening fence alone: gather until the closing one
            while i < raws.len() {
                let body = raws[i].trim();
                i += 1;
                if body == close || body.ends_with(close) {
                    let last = body.strip_suffix(close).unwrap_or("").trim();
                    if !last.is_empty() {
                        lines.push(vec![Span::new(math(last), Style::Math)]);
                    }
                    break;
                }
                lines.push(vec![Span::new(math(body), Style::Math)]);
            }
            continue;
        }

        // GFM table: a pipe row followed by a separator row. A lone pipe
        // line without the separator stays an ordinary paragraph.
        if is_table_row(trimmed) && raws.get(i).is_some_and(|n| is_table_separator(n.trim())) {
            let mut rows = vec![split_cells(trimmed)];
            i += 1; // past the separator row (it is drawn, not parsed)
            while i < raws.len() && is_table_row(raws[i].trim_start()) {
                rows.push(split_cells(raws[i].trim_start()));
                i += 1;
            }
            lines.extend(table_lines(&rows));
            continue;
        }

        // ATX heading: one or more '#', a space, then text
        if let Some(rest) = heading_text(trimmed) {
            lines.push(emphasized(rest, Style::Heading));
            continue;
        }

        // Blockquote
        if let Some(rest) = trimmed
            .strip_prefix("> ")
            .or_else(|| trimmed.strip_prefix(">"))
        {
            let mut line = vec![Span::new("│ ", Style::Quote)];
            line.extend(inline(rest));
            lines.push(line);
            continue;
        }

        // List item (unordered - * +, or ordered "N.")
        if let Some((marker, rest)) = list_item(trimmed) {
            let mut line = vec![Span::new(marker, Style::Marker)];
            line.extend(inline(rest));
            lines.push(line);
            continue;
        }

        // Ordinary paragraph line (blank lines stay blank)
        if raw.trim().is_empty() {
            lines.push(Vec::new());
        } else {
            lines.push(inline(raw));
        }
    }

    lines
}

/// The delimiter pair opening a display-mathematics block on this line, if
/// any. `$$` and `\[` only — never a lone `$`, which in an agent's output is
/// almost always a shell variable.
fn display_fence(line: &str) -> Option<(&'static str, &'static str)> {
    if line.starts_with("$$") {
        Some(("$$", "$$"))
    } else if line.starts_with("\\[") {
        Some(("\\[", "\\]"))
    } else {
        None
    }
}

/// Convert a mathematics fragment, collapsing the result to one line.
fn math(latex: &str) -> String {
    crate::mathtext::to_unicode(latex.trim())
}

/// A table row: starts with a pipe and has something after it.
fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.len() > 1
}

/// The header/body separator row: pipes, dashes, colons and spaces only,
/// with at least one dash and one pipe (`|---|:---:|`).
fn is_table_separator(line: &str) -> bool {
    !line.is_empty()
        && line.contains('-')
        && line.contains('|')
        && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Split a pipe row into trimmed cell texts (outer pipes dropped).
fn split_cells(row: &str) -> Vec<String> {
    let inner = row.trim();
    let inner = inner.strip_prefix('|').unwrap_or(inner);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// Lay out parsed table rows: each column padded to its widest cell by
/// DISPLAY width, header bolded, a dim rule under the header. Cells keep
/// their inline styling; widths are measured on the rendered text, so
/// `**bold**` counts 4 columns, not 8.
fn table_lines(rows: &[Vec<String>]) -> Vec<Line> {
    use unicode_width::UnicodeWidthStr;
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let parsed: Vec<Vec<Vec<Span>>> = rows
        .iter()
        .map(|r| {
            (0..cols)
                .map(|j| inline(r.get(j).map(String::as_str).unwrap_or("")))
                .collect()
        })
        .collect();
    let cell_width =
        |cell: &[Span]| -> usize { cell.iter().map(|s| s.text.as_str().width()).sum() };
    let mut widths = vec![0usize; cols];
    for row in &parsed {
        for (j, cell) in row.iter().enumerate() {
            widths[j] = widths[j].max(cell_width(cell));
        }
    }

    const GAP: &str = "  ";
    let mut out = Vec::new();
    for (r, row) in parsed.iter().enumerate() {
        let mut line: Line = Vec::new();
        for (j, cell) in row.iter().enumerate() {
            let w = cell_width(cell);
            for s in cell {
                if r == 0 && s.style == Style::Plain {
                    let mut span = s.clone();
                    span.style = Style::Bold;
                    line.push(span);
                } else {
                    line.push(s.clone());
                }
            }
            if j + 1 < cols {
                line.push(Span::new(" ".repeat(widths[j] - w) + GAP, Style::Plain));
            }
        }
        out.push(line);
        if r == 0 {
            let mut rule: Line = Vec::new();
            for (j, w) in widths.iter().enumerate() {
                rule.push(Span::new("─".repeat(*w), Style::TableRule));
                if j + 1 < cols {
                    rule.push(Span::new(GAP, Style::Plain));
                }
            }
            out.push(rule);
        }
    }
    out
}

/// If the line is an ATX heading, return the text after the marker.
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if let Some(text) = rest.strip_prefix(' ') {
            return Some(text.trim_end());
        }
    }
    None
}

/// If the line is a list item, return (marker-to-show, content).
fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ['-', '*', '+'] {
        if let Some(rest) = line.strip_prefix(bullet).and_then(|r| r.strip_prefix(' ')) {
            return Some(("• ".to_string(), rest));
        }
    }
    // Ordered: leading digits, then '.' or ')', then space
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let after = &line[digits..];
        if let Some(rest) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return Some((format!("{}. ", &line[..digits]), rest));
        }
    }
    None
}

/// Validate again at the opening boundary, not just while rendering. Keeping
/// the URL as data avoids both terminal escape injection and shell parsing.
pub fn http_url(raw: &str) -> Option<String> {
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (scheme, _) = raw.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let url = reqwest::Url::parse(raw).ok()?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url.to_string())
}

/// Match once, rather than rescanning the suffix for every unmatched '['.
fn delimiter_ends(chars: &[char], open: char, close: char) -> Vec<Option<usize>> {
    let mut ends = vec![None; chars.len()];
    let mut stack = Vec::new();
    let mut escaped = false;
    for (i, &ch) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == open {
            stack.push(i);
        } else if ch == close {
            if let Some(start) = stack.pop() {
                ends[start] = Some(i);
            }
        }
    }
    ends
}

fn bare_url_end(chars: &[char], start: usize) -> usize {
    let mut end = start;
    let mut parentheses = 0usize;
    while let Some(&ch) = chars.get(end) {
        if ch.is_whitespace()
            || ch.is_control()
            || matches!(
                ch,
                '<' | '>'
                    | '"'
                    | '\''
                    | '`'
                    | '，'
                    | '。'
                    | '；'
                    | '！'
                    | '？'
                    | '、'
                    | '（'
                    | '）'
                    | '【'
                    | '】'
                    | '《'
                    | '》'
            )
        {
            break;
        }
        if ch == '(' {
            parentheses += 1;
        }
        if ch == ')' {
            if parentheses == 0 {
                break;
            }
            parentheses -= 1;
        }
        end += 1;
    }
    while end > start && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?' | ']') {
        end -= 1;
    }
    end
}

/// Parse inline code, emphasis, and HTTP(S) links within one line.
fn inline(text: &str) -> Vec<Span> {
    inline_at(text, 0)
}

fn inline_at(text: &str, depth: usize) -> Vec<Span> {
    if depth >= 32 {
        return vec![Span::new(text, Style::Plain)];
    }
    let chars: Vec<char> = text.chars().collect();
    let (brackets, parentheses) = if chars.contains(&'[') {
        (
            delimiter_ends(&chars, '[', ']'),
            delimiter_ends(&chars, '(', ')'),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let angles = if chars.contains(&'<') {
        delimiter_ends(&chars, '<', '>')
    } else {
        Vec::new()
    };
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    let flush = |spans: &mut Vec<Span>, buf: &mut String| {
        if !buf.is_empty() {
            spans.push(Span::new(std::mem::take(buf), Style::Plain));
        }
    };

    while i < chars.len() {
        // Inline code: `...`
        if chars[i] == '`' {
            if let Some(end) = find_char(&chars, i + 1, '`') {
                flush(&mut spans, &mut buf);
                spans.push(Span::new(collect(&chars, i + 1, end), Style::Code));
                i = end + 1;
                continue;
            }
        }
        // Consume rejected destinations as one literal object, so an unsafe
        // outer scheme cannot expose a clickable URL nested inside it.
        let image = chars[i] == '!' && chars.get(i + 1) == Some(&'[');
        let bracket = i + usize::from(image);
        if chars.get(bracket) == Some(&'[') {
            if let Some(label_end) = brackets[bracket] {
                if chars.get(label_end + 1) == Some(&'(') {
                    if let Some(end) = parentheses[label_end + 1] {
                        flush(&mut spans, &mut buf);
                        let target = collect(&chars, label_end + 2, end);
                        let target = target
                            .strip_prefix('<')
                            .and_then(|s| s.strip_suffix('>'))
                            .unwrap_or(&target);
                        if let Some(url) = http_url(target).filter(|_| !image) {
                            for mut span in
                                inline_at(&collect(&chars, bracket + 1, label_end), depth + 1)
                            {
                                span.link = Some(url.clone());
                                spans.push(span);
                            }
                        } else {
                            spans.push(Span::new(collect(&chars, i, end + 1), Style::Plain));
                        }
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        if chars[i] == '<' {
            if let Some(end) = angles[i] {
                let label = collect(&chars, i + 1, end);
                if let Some(url) = http_url(&label) {
                    flush(&mut spans, &mut buf);
                    let mut span = Span::new(label, Style::Plain);
                    span.link = Some(url);
                    spans.push(span);
                    i = end + 1;
                    continue;
                }
            }
        }
        if matches!(chars[i], 'h' | 'H') {
            let prefix: String = chars[i..].iter().take(8).collect();
            if prefix.to_ascii_lowercase().starts_with("https://")
                || prefix.to_ascii_lowercase().starts_with("http://")
            {
                let end = bare_url_end(&chars, i);
                let label = collect(&chars, i, end);
                if let Some(url) = http_url(&label) {
                    flush(&mut spans, &mut buf);
                    let mut span = Span::new(label, Style::Plain);
                    span.link = Some(url);
                    spans.push(span);
                    i = end;
                    continue;
                }
            }
        }
        // Inline mathematics: \( … \). Written out in full because a lone
        // `$` cannot be trusted to mean mathematics here.
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'(') {
            if let Some(end) = find_seq(&chars, i + 2, '\\', ')') {
                flush(&mut spans, &mut buf);
                spans.push(Span::new(math(&collect(&chars, i + 2, end)), Style::Math));
                i = end + 2;
                continue;
            }
        }
        // Bold: **...**
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(end) = find_pair(&chars, i + 2, '*') {
                flush(&mut spans, &mut buf);
                spans.extend(emphasized_at(
                    &collect(&chars, i + 2, end),
                    Style::Bold,
                    depth + 1,
                ));
                i = end + 2;
                continue;
            }
        }
        // Italic: *...* or _..._
        if chars[i] == '*' || chars[i] == '_' {
            let marker = chars[i];
            if let Some(end) = find_char(&chars, i + 1, marker) {
                if end > i + 1 {
                    flush(&mut spans, &mut buf);
                    spans.extend(emphasized_at(
                        &collect(&chars, i + 1, end),
                        Style::Italic,
                        depth + 1,
                    ));
                    i = end + 1;
                    continue;
                }
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut spans, &mut buf);
    if spans.is_empty() {
        spans.push(Span::new(String::new(), Style::Plain));
    }
    spans
}

fn emphasized(text: &str, style: Style) -> Vec<Span> {
    emphasized_at(text, style, 0)
}

fn emphasized_at(text: &str, style: Style, depth: usize) -> Vec<Span> {
    inline_at(text, depth)
        .into_iter()
        .map(|mut span| {
            if span.style == Style::Plain {
                span.style = style;
            }
            span
        })
        .collect()
}

/// Find the start of the two-character sequence `a`,`b`.
fn find_seq(chars: &[char], from: usize, a: char, b: char) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&j| chars[j] == a && chars[j + 1] == b)
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

/// Find the start of a `cc` pair (two of the same char), for `**`.
fn find_pair(chars: &[char], from: usize, c: char) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == c && chars[j + 1] == c {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn collect(chars: &[char], from: usize, to: usize) -> String {
    chars[from..to].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Span {
        Span::new(text, Style::Plain)
    }

    #[test]
    fn links_render_labels_and_preserve_destinations() {
        let out = render("[文档](https://example.com/a_(b)?x=1&y=2#part) <https://example.org/a_b> https://example.net/end.");
        let text: String = out[0].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            text,
            "文档 https://example.org/a_b https://example.net/end."
        );
        let links: Vec<_> = out[0].iter().filter_map(|s| s.link.as_deref()).collect();
        assert_eq!(
            links,
            [
                "https://example.com/a_(b)?x=1&y=2#part",
                "https://example.org/a_b",
                "https://example.net/end"
            ]
        );
    }

    #[test]
    fn links_work_inside_emphasis_headings_and_tables() {
        for md in [
            "**[文档](https://example.com/)**",
            "# [文档](https://example.com/)",
            "| [文档](https://example.com/) |\n|---|",
        ] {
            let out = render(md);
            assert!(
                out.iter()
                    .flatten()
                    .any(|s| s.text == "文档" && s.link.as_deref() == Some("https://example.com/")),
                "{md}"
            );
        }
    }

    #[test]
    fn links_do_not_activate_code_images_or_unsafe_destinations() {
        for md in [
            "`https://example.com/`",
            "```\nhttps://example.com/\n```",
            "![image](https://example.com/)",
            "[bad](javascript:alert(1))",
            "[bad](file:///tmp/x)",
            "[bad](https://user:password@example.com/)",
            "[bad](https://example.com/\u{1b}[0m)",
        ] {
            assert!(
                render(md).iter().flatten().all(|s| s.link.is_none()),
                "{md}"
            );
        }
    }

    #[test]
    fn links_stop_at_prose_punctuation_and_keep_balanced_parentheses() {
        let out = render("见 https://example.com/a_(b)，然后 (https://example.org/x). next");
        let links: Vec<_> = out[0].iter().filter_map(|s| s.link.as_deref()).collect();
        assert_eq!(
            links,
            ["https://example.com/a_(b)", "https://example.org/x"]
        );
    }

    #[test]
    fn links_bound_nested_labels_and_handle_unmatched_brackets() {
        let mut nested = "label".to_string();
        for _ in 0..100 {
            nested = format!("[{nested}](https://example.com/)");
        }
        let out = render(&nested);
        assert!(out.iter().flatten().any(|s| s.text.contains("label")));
        for marker in ["[", "<"] {
            let unmatched = marker.repeat(16000);
            assert_eq!(render(&unmatched), vec![vec![plain(&unmatched)]]);
        }
        for url in ["HTTP://example.com/a", "https://example.com/a?q=x&z=y#part"] {
            assert!(http_url(url).is_some());
        }
        for url in [
            "https://",
            "mailto:a@example.com",
            "http:example.com",
            "https://example.com/\nnext",
        ] {
            assert!(http_url(url).is_none());
        }
    }

    #[test]
    fn inline_bold_italic_code() {
        let line = &render("a **b** c `d` e *f*")[0];
        assert_eq!(
            line,
            &vec![
                plain("a "),
                Span::new("b", Style::Bold),
                plain(" c "),
                Span::new("d", Style::Code),
                plain(" e "),
                Span::new("f", Style::Italic),
            ]
        );
    }

    #[test]
    fn headings_and_lists() {
        let out = render("# Title\n- one\n- two\n1. first");
        assert_eq!(out[0], vec![Span::new("Title", Style::Heading)]);
        assert_eq!(out[1][0], Span::new("• ", Style::Marker));
        assert_eq!(out[1][1], plain("one"));
        assert_eq!(out[3][0], Span::new("1. ", Style::Marker));
    }

    #[test]
    fn fenced_code_block_is_verbatim() {
        let out = render("before\n```\nlet x = **not bold**;\n```\nafter");
        // the fence lines themselves are dropped; inner line is verbatim
        assert_eq!(out[0], vec![plain("before")]);
        assert_eq!(
            out[1],
            vec![Span::new("let x = **not bold**;", Style::CodeBlock)]
        );
        assert_eq!(out[2], vec![plain("after")]);
    }

    #[test]
    fn blockquote_gets_a_bar() {
        let out = render("> quoted **bold**");
        assert_eq!(out[0][0], Span::new("│ ", Style::Quote));
        assert_eq!(out[0][1], plain("quoted "));
        assert_eq!(out[0][2], Span::new("bold", Style::Bold));
    }

    #[test]
    fn unterminated_markers_stay_literal() {
        // a lone * or ` must not eat the rest of the line
        let out = render("2 * 3 = 6 and `oops");
        assert_eq!(out[0], vec![plain("2 * 3 = 6 and `oops")]);
    }

    /// Concatenate a rendered line back to visible text.
    fn text_of(line: &Line) -> String {
        line.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn a_table_aligns_columns_by_display_width() {
        // CJK cells are two terminal cells wide; padding must account for
        // that or every column drifts.
        let out = render("| 工具 | 说明 |\n|---|---|\n| read | 读写文件 |\n| ls | 短 |");
        // header, rule, two body rows
        assert_eq!(out.len(), 4);
        // header cells are bold
        assert_eq!(out[0][0], Span::new("工具", Style::Bold));
        // column widths: max(工具=4, read=4, ls=2)=4; the rule mirrors them
        assert_eq!(out[1][0], Span::new("────", Style::TableRule));
        assert_eq!(out[1][2], Span::new("────────", Style::TableRule));
        // "ls" (width 2) pads to 4 plus the 2-space gap before column two
        assert_eq!(text_of(&out[3]), "ls    短");
        // "read" (width 4) needs only the gap
        assert_eq!(text_of(&out[2]), "read  读写文件");
    }

    #[test]
    fn table_cell_width_is_measured_on_rendered_text() {
        // `**read**` renders as 4 columns of bold text, not 8 of markup
        let out = render("| a | b |\n|---|---|\n| **read** | x |");
        assert_eq!(out[2][0], Span::new("read", Style::Bold));
        assert_eq!(text_of(&out[2]), "read  x");
        assert_eq!(out[1][0], Span::new("────", Style::TableRule));
    }

    #[test]
    fn a_pipe_line_without_a_separator_is_not_a_table() {
        let out = render("| just | text |\nplain after");
        assert_eq!(out[0], vec![plain("| just | text |")]);
        assert_eq!(out[1], vec![plain("plain after")]);
    }

    #[test]
    fn a_table_ends_at_the_first_non_pipe_line() {
        // Two columns, so a wrongly-swallowed trailing line would show up
        // as a padded table row instead of a bare paragraph.
        let out = render("| a | b |\n|---|---|\n| c | d |\nafter the table");
        assert_eq!(text_of(&out[2]), "c  d");
        assert_eq!(out[3], vec![plain("after the table")]);
    }

    /// The whole point of the delimiter choice: an agent's prose is full of
    /// `$PATH` and `$ARGUMENTS`, and none of it is mathematics.
    #[test]
    fn a_lone_dollar_is_never_mathematics() {
        let out = render("set $PATH and $HOME, then pay $5");
        assert_eq!(out[0], vec![plain("set $PATH and $HOME, then pay $5")]);
    }

    #[test]
    fn inline_math_becomes_unicode_in_its_own_span() {
        let out = render(r"so \(x^2 + \alpha\) holds");
        assert_eq!(
            out[0],
            vec![
                plain("so "),
                Span::new("x² + α", Style::Math),
                plain(" holds"),
            ]
        );
    }

    #[test]
    fn display_math_takes_its_own_line_on_one_line_or_several() {
        let out = render("before\n$$ \\frac{a+b}{2} $$\nafter");
        assert_eq!(out[1], vec![Span::new("(a+b)/2", Style::Math)]);
        assert_eq!(out[2], vec![plain("after")]);

        let out = render("$$\nx = \\sum_{i=1}^{n} i\n$$\ndone");
        assert_eq!(out[0], vec![Span::new("x = ∑ᵢ₌₁ⁿ i", Style::Math)]);
        assert_eq!(out[1], vec![plain("done")]);
    }

    /// Code is quoted, not interpreted — a backslash inside it is a backslash.
    #[test]
    fn math_is_not_touched_inside_code() {
        let out = render("```\n\\frac{1}{2}\n```");
        assert_eq!(out[0], vec![Span::new("\\frac{1}{2}", Style::CodeBlock)]);
        let out = render(r"use `\alpha` here");
        assert_eq!(out[0][1], Span::new("\\alpha", Style::Code));
    }
}
