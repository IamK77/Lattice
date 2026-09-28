//! One screen-row map for wrapping, cursor placement and vertical movement.
//! Output is safe terminal text: input control characters are never commands.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
pub struct InputLayout {
    pub rows: Vec<String>,
    // Input byte boundaries and their screen positions, in input order.
    positions: Vec<(usize, usize, usize)>,
}

impl InputLayout {
    pub fn new(text: &str, width: usize) -> Self {
        let width = width.max(1);
        let mut result = Self {
            rows: vec![String::new()],
            positions: vec![(0, 0, 0)],
        };
        let mut column = 0;
        for (at, grapheme) in text.grapheme_indices(true) {
            if matches!(grapheme, "\n" | "\r" | "\r\n") {
                result.rows.push(String::new());
                column = 0;
            } else {
                let visible = if grapheme == "\t" {
                    " ".repeat(4 - column % 4)
                } else {
                    grapheme
                        .chars()
                        .map(|c| {
                            if c.is_control() {
                                format!("\\u{{{:x}}}", c as u32)
                            } else {
                                c.to_string()
                            }
                        })
                        .collect::<String>()
                };
                for (part, symbol) in visible.graphemes(true).enumerate() {
                    let symbol = if symbol.width() > width {
                        "�"
                    } else {
                        symbol
                    };
                    let size = symbol.width();
                    if column + size > width {
                        result.rows.push(String::new());
                        column = 0;
                        // At a soft break the caret before this character is
                        // on the continuation row, not beyond the prior edge.
                        if part == 0 {
                            if let Some(position) = result.positions.last_mut() {
                                *position = (at, result.rows.len() - 1, 0);
                            }
                        }
                    }
                    result.rows.last_mut().unwrap().push_str(symbol);
                    column += size;
                }
            }
            result
                .positions
                .push((at + grapheme.len(), result.rows.len() - 1, column));
        }
        result
    }

    /// Positions inside a grapheme resolve to its leading boundary.
    pub fn position(&self, byte: usize) -> (usize, usize) {
        let i = self
            .positions
            .partition_point(|(at, _, _)| *at <= byte)
            .saturating_sub(1);
        let (_, row, column) = self.positions[i];
        (row, column)
    }

    /// Keep the caret inside the available rows, including a short terminal.
    pub fn visible_start(&self, cursor: usize, height: usize) -> usize {
        self.position(cursor).0.saturating_sub(height.max(1) - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_wrap_and_cursor_share_display_columns() {
        let layout = InputLayout::new("ab中cd", 4);
        assert_eq!(layout.rows, ["ab中", "cd"]);
        assert_eq!(layout.position("ab中".len()), (1, 0));
        assert_eq!(layout.position("ab中c".len()), (1, 1));
    }

    #[test]
    fn line_endings_and_blank_lines_have_one_meaning() {
        for text in ["a\nb\n\nc", "a\rb\r\rc", "a\r\nb\r\n\r\nc"] {
            let layout = InputLayout::new(text, 20);
            assert_eq!(layout.rows, ["a", "b", "", "c"]);
            assert_eq!(layout.position(text.len()), (3, 1));
        }
    }

    #[test]
    fn controls_never_reach_a_terminal_and_tabs_take_real_columns() {
        let layout = InputLayout::new("a\tb\x1b[2J", 40);
        assert_eq!(layout.rows, ["a   b\\u{1b}[2J"]);
        assert!(!layout.rows.iter().any(|s| s.chars().any(char::is_control)));
        assert_eq!(layout.position(2), (0, 4));
    }

    #[test]
    fn combined_emoji_is_not_split_or_counted_multiple_times() {
        let layout = InputLayout::new("a👩‍💻b", 3);
        assert_eq!(layout.rows, ["a👩‍💻", "b"]);
        assert_eq!(layout.position("a👩‍💻".len()), (1, 0));
    }

    #[test]
    fn viewport_and_huge_input_do_not_depend_on_u16_offsets() {
        let text = "x".repeat(70_000);
        let layout = InputLayout::new(&text, 40);
        let (row, col) = layout.position(text.len());
        assert_eq!((row, col), (1749, 40));
        assert_eq!(layout.visible_start(text.len(), 6), 1744);
        assert_eq!(layout.visible_start(text.len(), 1), row);
        assert_eq!(InputLayout::new("中", 0).rows, ["�"]);
    }
}
