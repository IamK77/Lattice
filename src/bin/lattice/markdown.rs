//! Terminal Markdown presentation and content-keyed caching. Neutral parsing
//! lives in lattice::richtext; link-aware wrapping is a sibling responsibility.

use super::linked_line::LinkedLine;
use super::theme::{ACCENT, CODE_BG, CODE_FG, DIM, FG};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[cfg(test)]
#[path = "markdown/tests.rs"]
mod tests;

/// The language named on each fenced block, in the order the blocks appear.
///
/// `richtext` keeps code lines verbatim and drops the fence, which is right
/// for a neutral model — a language tag is a hint for colouring, not content.
/// The colouring lives here, so the tag is recovered here, by walking the same
/// source in the same order.
fn fence_languages(md: &str) -> Vec<String> {
    let mut langs = Vec::new();
    let mut open = false;
    for line in md.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("```") {
            continue;
        }
        if open {
            open = false;
        } else {
            open = true;
            langs.push(trimmed.trim_start_matches('`').trim().to_string());
        }
    }
    langs
}

/// Syntect's defaults, loaded once. Loading them per render would cost more
/// than the drawing.
fn syntaxes() -> &'static syntect::parsing::SyntaxSet {
    static SET: std::sync::OnceLock<syntect::parsing::SyntaxSet> = std::sync::OnceLock::new();
    SET.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

fn code_theme() -> &'static syntect::highlighting::Theme {
    static THEME: std::sync::OnceLock<syntect::highlighting::Theme> = std::sync::OnceLock::new();
    THEME.get_or_init(|| {
        let mut themes = syntect::highlighting::ThemeSet::load_defaults();
        themes
            .themes
            .remove("base16-ocean.dark")
            .expect("syntect ships this theme")
    })
}

/// One code line, coloured by syntax. No background band: the colours are what
/// says "this is code", and a block-wide background fought with the terminal's
/// own and had to be padded to a rectangle to look right at all.
fn code_line(
    text: &str,
    highlighter: &mut Option<syntect::easy::HighlightLines<'static>>,
) -> Line<'static> {
    let Some(h) = highlighter else {
        return Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(CODE_FG),
        ));
    };
    let with_newline = format!("{text}\n");
    let Ok(ranges) = h.highlight_line(&with_newline, syntaxes()) else {
        return Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(CODE_FG),
        ));
    };
    let mut spans = vec![Span::raw("  ")];
    for (style, piece) in ranges {
        let piece = piece.trim_end_matches('\n');
        if piece.is_empty() {
            continue;
        }
        let c = style.foreground;
        spans.push(Span::styled(
            piece.to_string(),
            Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
        ));
    }
    Line::from(spans)
}

/// Completed blocks accumulate here; live streaming text bypasses this cache.
/// A miss when full clears the whole cache, then inserts the new block.
const RENDER_CACHE_MAX: usize = 512;

thread_local! {
    /// Rendered markdown, keyed by the text it came from.
    ///
    /// Highlighting is the most expensive thing the frontend does — 88% of a
    /// frame, measured — and every bit of it re-derives a result that cannot
    /// have changed, because the text of an entry is fixed the moment it is in
    /// the transcript. Keyed by content rather than by position so that
    /// folding, scrolling and reordering cannot serve a stale line.
    static RENDERED: std::cell::RefCell<std::collections::HashMap<String, Vec<LinkedLine>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(test)]
pub(super) fn clear_render_cache() {
    RENDERED.with(|cache| cache.borrow_mut().clear());
}

#[cfg(test)]
pub(super) fn render_cache_len() -> usize {
    RENDERED.with(|cache| cache.borrow().len())
}

/// Render Markdown to styled terminal lines using the neutral richtext model.
/// Code blocks use syntax colors when known and a flat code color otherwise.
/// Memoised by content, before viewport wrapping.
pub(super) fn markdown_lines(md: &str) -> Vec<Line<'static>> {
    markdown_rows(md).into_iter().map(|row| row.line).collect()
}

pub(super) fn markdown_rows(md: &str) -> Vec<LinkedLine> {
    RENDERED.with(|cache| {
        if let Some(done) = cache.borrow().get(md) {
            return done.clone();
        }
        let built = markdown_rows_uncached(md);
        let mut cache = cache.borrow_mut();
        if cache.len() >= RENDER_CACHE_MAX {
            cache.clear();
        }
        cache.insert(md.to_string(), built.clone());
        built
    })
}

#[cfg(test)]
fn markdown_lines_uncached(md: &str) -> Vec<Line<'static>> {
    markdown_rows_uncached(md)
        .into_iter()
        .map(|row| row.line)
        .collect()
}

/// Used for live streaming buffers: no lookup or insertion, and no cursor.
pub(super) fn markdown_rows_uncached(md: &str) -> Vec<LinkedLine> {
    use lattice::richtext::render;
    let rendered = render(md);
    let langs = fence_languages(md);
    let mut out = Vec::new();
    let mut i = 0;
    let mut block = 0usize;
    while i < rendered.len() {
        if is_code_line(&rendered[i]) {
            let lang = langs.get(block).map(String::as_str).unwrap_or("");
            block += 1;
            let syntax = (!lang.is_empty())
                .then(|| {
                    syntaxes()
                        .find_syntax_by_token(lang)
                        .or_else(|| syntaxes().find_syntax_by_extension(lang))
                })
                .flatten();
            let mut highlighter =
                syntax.map(|s| syntect::easy::HighlightLines::new(s, code_theme()));
            while i < rendered.len() && is_code_line(&rendered[i]) {
                out.push(LinkedLine {
                    line: code_line(&rendered[i][0].text, &mut highlighter),
                    links: Vec::new(),
                });
                i += 1;
            }
        } else {
            out.push(LinkedLine {
                line: map_line(&rendered[i]),
                links: rendered[i].iter().map(|s| s.link.clone()).collect(),
            });
            i += 1;
        }
    }
    out
}

/// A richtext line that is a single fenced-code-block span.
fn is_code_line(line: &[lattice::richtext::Span]) -> bool {
    matches!(line, [s] if s.style == lattice::richtext::Style::CodeBlock)
}

/// Map one non-code richtext line onto the TUI palette.
fn map_line(line: &[lattice::richtext::Span]) -> Line<'static> {
    use lattice::richtext::Style as R;
    let spans: Vec<Span> = line
        .iter()
        .map(|s| {
            let style = match s.style {
                R::Plain => Style::default().fg(FG),
                R::Bold => Style::default().fg(FG).add_modifier(Modifier::BOLD),
                R::Italic => Style::default().fg(FG).add_modifier(Modifier::ITALIC),
                R::Code => Style::default().fg(FG).bg(CODE_BG),
                R::Heading => Style::default().fg(FG).add_modifier(Modifier::BOLD),
                R::Marker => Style::default().fg(DIM),
                // A fenced line never reaches here — `markdown_lines`
                // colours it by syntax instead.
                R::CodeBlock => Style::default().fg(CODE_FG),
                R::Quote => Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                R::TableRule => Style::default().fg(DIM),
                // Mathematics reads as one object, not as prose: the accent
                // sets it apart from the sentence around it
                R::Math => Style::default().fg(ACCENT),
            };
            let style = if s.link.is_some() {
                style.fg(ACCENT).add_modifier(Modifier::UNDERLINED)
            } else {
                style
            };
            Span::styled(s.text.clone(), style)
        })
        .collect();
    Line::from(spans)
}
