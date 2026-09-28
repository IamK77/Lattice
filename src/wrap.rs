//! Word-wrapping for styled text, as neutral data.
//!
//! A frontend that needs to know *which rendered row* a click landed on can't
//! rely on the terminal library's internal wrapping — it has to wrap the text
//! itself, so one logical line becomes a known number of rows. This module does
//! that on a neutral model: a row is a list of styled runs, a run is text plus
//! an opaque style id. The frontend maps its own spans onto [`Run`]s, wraps,
//! and maps back — the wrapping logic stays here, unit-tested, terminal-free.
//!
//! Width is measured in terminal columns: CJK and other wide characters count
//! as two. Breaks prefer spaces; a word longer than the width is hard-split.

/// A run of text sharing one style. `style` is an opaque id the caller assigns
/// (e.g. an index into its own style table); wrapping only ever compares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: usize,
}

impl Run {
    pub fn new(text: impl Into<String>, style: usize) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// Display width of a character in terminal columns (wide = 2).
///
/// East Asian AMBIGUOUS characters — `×`, `≤`, `≥`, `·`, `→`, `π`, `…` — count
/// as WIDE. Unicode leaves their width to the environment, and a terminal
/// showing CJK text almost always draws them in two columns. Guessing narrow
/// is the dangerous guess: the app then packs one column too many into a row,
/// the terminal wraps the overflow onto the next line, and every line below
/// slides — which is how a screen full of tidy text turns into interleaved
/// wreckage. Guessing wide when the terminal is narrow costs a ragged right
/// edge and nothing else, so the safe side is the wide one.
///
/// Box drawing and block elements are the exception: they are ambiguous too,
/// but terminals that draw frames render them in one column (a full-width
/// separator built from them lands on one line, which is the evidence this
/// exception was written from), and our own rules and separators are made of
/// them — counting them wide would make every table rule twice its column.
pub fn char_cols(c: char) -> usize {
    if (0x2500..=0x259F).contains(&(c as u32)) {
        return 1;
    }
    unicode_width::UnicodeWidthChar::width_cjk(c).unwrap_or(1)
}

/// Display width of a string in terminal columns.
pub fn str_cols(s: &str) -> usize {
    s.chars().map(char_cols).sum()
}

/// Columns a tab opens out to. Four rather than eight: this is a transcript in
/// a terminal that also holds a gutter and an indent, not a source file.
pub const TAB_STOP: usize = 4;

/// Wrap one row of styled runs to `width` columns. Returns one or more rows,
/// each a list of runs; styles are preserved and adjacent same-style text is
/// merged. An empty input yields a single empty row (so a blank line stays a
/// blank row). `width == 0` returns the input unwrapped.
pub fn wrap(runs: &[Run], width: usize) -> Vec<Vec<Run>> {
    if width == 0 {
        return vec![runs.to_vec()];
    }
    // Flatten to (char, style) cells, with tabs opened out and every other
    // control character dropped.
    //
    // A cell grid has no room for a character that means "move somewhere
    // else". A tab written into a cell counts as one column here and moves the
    // real cursor to the next tab stop, so everything after it on that row
    // lands somewhere this code did not put it, and whatever the previous
    // frame left there shows through. Seen for real: a Go listing (tabs) came
    // out with fragments of its own lines scattered across the right-hand side
    // while a Rust listing (spaces) in the same reply was clean.
    let mut cells: Vec<(char, usize)> = Vec::new();
    for r in runs {
        for ch in r.text.chars() {
            match ch {
                '\t' => {
                    let stop = (cells.len() / TAB_STOP + 1) * TAB_STOP;
                    while cells.len() < stop {
                        cells.push((' ', r.style));
                    }
                }
                c if c.is_control() => {}
                c => cells.push((c, r.style)),
            }
        }
    }

    let mut rows: Vec<Vec<(char, usize)>> = Vec::new();
    let mut cur: Vec<(char, usize)> = Vec::new();
    let mut cur_w = 0usize;
    let mut last_space: Option<usize> = None; // index in `cur` of the last ' '

    for (ch, st) in cells {
        let cw = char_cols(ch);
        // Breaking once is not always enough. What carries over from a break
        // can already be as wide as the row, and then this character still
        // does not fit — asking only once let it through, and a row one column
        // over is drawn across the row below it, which is the whole thing this
        // module exists to prevent. Two passes at most: the second finds no
        // space to break on and empties the row outright.
        while cur_w + cw > width && !cur.is_empty() {
            if let Some(sp) = last_space {
                // Break after the last space: [0..sp] is a finished row; drop
                // the space at `sp`; the rest carries to the next row.
                let mut rest = cur.split_off(sp);
                rest.remove(0); // the space itself
                let finished = std::mem::take(&mut cur);
                cur = rest;
                // Unless the space WAS the row — an indented line breaking on
                // its own indent finishes nothing, and a row of nothing is a
                // blank line on the screen that no one wrote.
                if !finished.is_empty() {
                    rows.push(finished);
                }
            } else {
                // No space to break on — hard-split the long word.
                rows.push(std::mem::take(&mut cur));
            }
            cur_w = cur.iter().map(|(c, _)| char_cols(*c)).sum();
            last_space = None;
        }
        if ch == ' ' {
            last_space = Some(cur.len());
        }
        cur.push((ch, st));
        cur_w += cw;
    }
    if !cur.is_empty() || rows.is_empty() {
        rows.push(cur);
    }

    rows.iter().map(|row| coalesce(row)).collect()
}

/// Merge consecutive same-style cells back into runs.
fn coalesce(cells: &[(char, usize)]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for &(ch, st) in cells {
        match runs.last_mut() {
            Some(last) if last.style == st => last.text.push(ch),
            _ => runs.push(Run::new(ch.to_string(), st)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {

    /// A tab is opened out, not written into a cell.
    ///
    /// A cell grid has no room for a character that means "move somewhere
    /// else": written as one cell it counts as one column here and moves the
    /// real cursor to the next tab stop, so the rest of the row lands where
    /// this code did not put it. A Go listing came out with fragments of its
    /// own lines scattered across the screen; the Rust listing beside it,
    /// indented with spaces, was clean.
    #[test]
    fn a_tab_becomes_spaces_up_to_the_next_stop() {
        let text =
            |rows: Vec<Vec<Run>>| -> String { rows[0].iter().map(|r| r.text.clone()).collect() };
        let out = text(wrap(&[Run::new("\t\"context\"".to_string(), 0)], 80));
        assert_eq!(out, "    \"context\"", "one leading tab is four columns");
        assert!(!out.contains('\t'), "and no tab survives into a cell");

        // To the next STOP, not a fixed four: a tab after two characters
        // fills the remaining two.
        assert_eq!(text(wrap(&[Run::new("ab\tc".to_string(), 0)], 80)), "ab  c");
        // Two tabs are two levels
        assert_eq!(
            text(wrap(&[Run::new("\t\tx".to_string(), 0)], 80)),
            "        x"
        );
    }

    /// Any other control character is dropped rather than written. Same
    /// reason: it does something to the cursor instead of taking a column.
    #[test]
    fn other_control_characters_never_reach_a_cell() {
        let rows = wrap(&[Run::new("a\u{0}b\u{7}c\rd".to_string(), 0)], 80);
        let out: String = rows[0].iter().map(|r| r.text.clone()).collect();
        assert_eq!(out, "abcd");
    }
    use super::*;

    fn row_text(row: &[Run]) -> String {
        row.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn breaks_at_spaces() {
        let rows = wrap(&[Run::new("the quick brown fox", 0)], 10);
        let texts: Vec<String> = rows.iter().map(|r| row_text(r)).collect();
        assert_eq!(texts, vec!["the quick", "brown fox"]);
    }

    #[test]
    fn hard_splits_an_overlong_word() {
        let rows = wrap(&[Run::new("abcdefghijklmno", 0)], 5);
        let texts: Vec<String> = rows.iter().map(|r| row_text(r)).collect();
        assert_eq!(texts, vec!["abcde", "fghij", "klmno"]);
    }

    #[test]
    fn preserves_styles_across_a_break() {
        // "aaaa " in style 0, "bbbb" in style 1, width forces a wrap
        let rows = wrap(&[Run::new("aaaa ", 0), Run::new("bbbb", 1)], 5);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec![Run::new("aaaa", 0)]);
        assert_eq!(rows[1], vec![Run::new("bbbb", 1)]);
    }

    #[test]
    fn wide_characters_count_as_two_columns() {
        // four wide chars = 8 cols; width 4 fits two per row
        let rows = wrap(&[Run::new("你好世界", 0)], 4);
        let texts: Vec<String> = rows.iter().map(|r| row_text(r)).collect();
        assert_eq!(texts, vec!["你好", "世界"]);
    }

    #[test]
    fn an_empty_row_stays_one_row() {
        assert_eq!(wrap(&[], 10).len(), 1);
        assert_eq!(wrap(&[Run::new("", 0)], 10).len(), 1);
    }

    #[test]
    fn short_text_is_one_row() {
        let rows = wrap(&[Run::new("short", 0)], 20);
        assert_eq!(rows.len(), 1);
        assert_eq!(row_text(&rows[0]), "short");
    }
}

#[cfg(test)]
mod ambiguous_width {
    use super::*;

    /// The characters that actually turned up in real output and tore a real
    /// screen: a table row carrying `×` and one carrying `·π` each ran one or
    /// two columns past the pane, the terminal wrapped the overflow, and every
    /// line below slid out of place. Unicode calls them AMBIGUOUS and leaves
    /// the width to the environment; a terminal showing Chinese draws them
    /// wide, so wide is what must be reserved.
    #[test]
    fn ambiguous_characters_reserve_two_columns() {
        for c in ['×', '≤', '≥', '·', '→', '…', '○'] {
            assert_eq!(char_cols(c), 2, "{c} (U+{:04X}) must reserve two", c as u32);
        }
        // Greek is the known gap: `unicodedata` calls π ambiguous, the
        // unicode-width table does not, and a CJK terminal may well draw it
        // wide anyway. We follow the maintained table rather than keep a
        // private list of exceptions — a formula-heavy reply can still run one
        // column long, and this is the note that says so on purpose.
        assert_eq!(char_cols('π'), 1, "the table's judgement, not ours");
    }

    /// Box drawing is ambiguous too, and terminals that draw frames render it
    /// narrow — the evidence being a full-width separator built from it that
    /// lands on one line rather than two. Our own rules and separators are
    /// made of these, and `"─".repeat(w)` is meant to be `w` columns wide.
    #[test]
    fn box_drawing_stays_one_column() {
        for c in ['─', '│', '┌', '┘', '├', '█', '░'] {
            assert_eq!(char_cols(c), 1, "{c} (U+{:04X}) must stay narrow", c as u32);
        }
        assert_eq!(
            str_cols(&"─".repeat(40)),
            40,
            "a rule is as wide as it is long"
        );
    }

    /// CJK and emoji were already wide and must stay so.
    #[test]
    fn what_was_wide_before_is_wide_still() {
        for c in ['中', '文', '（', '）', '한', '🙂'] {
            assert_eq!(char_cols(c), 2, "{c} (U+{:04X})", c as u32);
        }
        for c in ['a', ' ', '1', '✦', '∙', '∘'] {
            assert_eq!(char_cols(c), 1, "{c} (U+{:04X})", c as u32);
        }
    }

    /// An indented line whose first row holds no space after the indent, ending
    /// on a wide character — an indented URL, path or hash followed by one CJK
    /// character, which is an ordinary thing to write here.
    ///
    /// Breaking on the leading space carries the whole run over to the next
    /// row, and the wide character is then appended to it without asking again
    /// whether it still fits. One column over is exactly the overrun this
    /// module exists to prevent: the row is drawn across the one below it.
    #[test]
    fn a_wide_character_after_an_indent_does_not_push_a_row_over() {
        for width in [20usize, 40, 79, 120] {
            for tail in ['\u{00d7}', '\u{4e2d}'] {
                let text = format!(" {}{tail}", "a".repeat(width - 1));
                let rows = wrap(&[Run::new(text, 0)], width);
                for row in &rows {
                    let rendered: String = row.iter().map(|r| r.text.as_str()).collect();
                    assert!(
                        str_cols(&rendered) <= width,
                        "width {width}, ending {tail:?}: a row is {} columns",
                        str_cols(&rendered)
                    );
                }
                assert!(
                    !rows.iter().any(|row| row_text(row).is_empty()),
                    "width {width}, ending {tail:?}: breaking left a blank row behind"
                );
            }
        }
    }

    fn row_text(row: &[Run]) -> String {
        row.iter().map(|r| r.text.as_str()).collect()
    }

    /// The point of all of it: a line of mixed text never comes back from the
    /// wrapper wider than the pane it was wrapped to.
    #[test]
    fn no_wrapped_row_overruns_the_width() {
        let text = "缓动方向错误  1 - cos(t·π/2) 是 ease-IN（先慢后快），弧光卡着蹭进来  \
                    自适应：短句（≤6字）= 窄弧聚焦，长句（≥16字）= 宽弧覆盖 half×0.45 → done …";
        for width in [20usize, 37, 60, 79, 135, 138] {
            for row in wrap(&[Run::new(text.to_string(), 0)], width) {
                let rendered: String = row.iter().map(|r| r.text.as_str()).collect();
                assert!(
                    str_cols(&rendered) <= width,
                    "row {rendered:?} is {} columns at width {width}",
                    str_cols(&rendered)
                );
            }
        }
    }
}
