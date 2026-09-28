//! Shared terminal-column clipping. Use the same width rules as wrapping.

use lattice::wrap;

/// The mark left where text was cut away.
pub(super) const ELLIPSIS: char = '…';

/// Truncate a string to `w` display columns, adding an ellipsis if clipped.
///
/// Measured with the SAME ruler the wrapper uses ([`wrap::char_cols`]), and
/// that is the whole point rather than a detail. Counting characters would let
/// 44 Chinese characters through as 88 columns; but counting them with the
/// other width table is just as wrong in a quieter way — `…`, `✓` and the
/// arrows are all "ambiguous" characters, which that table calls one column and
/// this one calls two. A string clipped by one ruler and laid out by the other
/// fits by the first measure, overflows by the second, and wraps. That is how
/// three ellipses pushed a tool card's head onto a second line while every
/// arithmetic in sight said it fit.
pub(super) fn clip(s: &str, w: usize) -> String {
    if wrap::str_cols(s) <= w {
        return s.to_string();
    }
    // The ellipsis takes room too, and by this ruler it takes TWO columns —
    // it is one of the ambiguous characters itself.
    let budget = w.saturating_sub(wrap::char_cols(ELLIPSIS));
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let cw = wrap::char_cols(c).max(1);
        if used + cw > budget {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push(ELLIPSIS);
    out
}

/// Clip from the FRONT, marking the cut with a leading `…`.
pub(super) fn clip_front(s: &str, w: usize) -> String {
    if wrap::str_cols(s) <= w {
        return s.to_string();
    }
    let budget = w.saturating_sub(wrap::char_cols(ELLIPSIS));
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0usize;
    for c in s.chars().rev() {
        let cw = wrap::char_cols(c).max(1);
        if used + cw > budget {
            break;
        }
        kept.push(c);
        used += cw;
    }
    let mut out = String::from(ELLIPSIS);
    out.extend(kept.into_iter().rev());
    out
}

/// Trim a value from whichever end matters least. Paths are identified by
/// their final component, so keep that end when they have to lose columns.
pub(super) fn fit(raw: &str, room: usize) -> String {
    if wrap::str_cols(raw) > room && looks_like_a_path(raw) {
        return clip_front(raw, room);
    }
    clip(raw, room)
}

/// A display heuristic, never a filesystem query. URLs follow the same rule.
fn looks_like_a_path(s: &str) -> bool {
    s.contains('/') && !s.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_summaries_keep_the_end_without_consulting_the_filesystem() {
        assert_eq!(fit("/not/on/disk/report.txt", 12), "…report.txt");
        assert_eq!(fit("https://example.com/report.txt", 12), "…report.txt");
        assert_eq!(fit("run /not/on/disk/report.txt", 12), "run /not/o…");
        assert_eq!(fit("a/b", 12), "a/b");
    }

    #[test]
    fn clipping_counts_columns_not_characters() {
        assert_eq!(clip("abcdef", 10), "abcdef");
        assert_eq!(clip("abcdefghijkl", 6), "abcd…");
        assert_eq!(clip("中文中文中文", 6), "中文…");
        assert_eq!(clip_front("abcdefghijkl", 6), "…ijkl");
        assert_eq!(clip_front("甲乙丙丁戊己", 6), "…戊己");
        for text in [
            "中文…箭头→",
            "long ASCII text",
            "abcdef",
            "abcdefghijkl",
            "中文中文中文",
            "mixed 中文 x",
        ] {
            for room in 2..16 {
                assert!(wrap::str_cols(&clip(text, room)) <= room);
                assert!(wrap::str_cols(&clip_front(text, room)) <= room);
            }
        }
    }
}
