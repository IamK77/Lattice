//! A small line editor: the input buffer, a cursor, and a history — as neutral
//! data, no terminal library in sight.
//!
//! The input box is used constantly, so it deserves the editing every shell has:
//! move the cursor, jump by word, delete to the line's end, recall past lines.
//! Keeping that here (a pure, unit-tested type) rather than inline in the key
//! loop is the same discipline as [`crate::view`]: logic in the library, colors
//! and key bindings in the frontend.
//!
//! Positions are byte indices into a UTF-8 string, always kept on a character
//! boundary — every move steps by whole characters, so multi-byte text (CJK,
//! emoji) is safe. The buffer may contain `\n`: a frontend that wants multi-line
//! input inserts newlines and renders the rows itself.

/// Pastes at or above this many lines are folded to a placeholder. Below it,
/// a paste is just typing that arrived quickly and should look like typing.
const FOLD_LINES: usize = 5;
/// …as is a paste this long on one line, which floods the box just as badly.
const FOLD_BYTES: usize = 800;

use std::collections::BTreeMap;

mod layout;
pub use layout::InputLayout;

/// A fold occupies one character, but its identity belongs to its byte position.
/// The same character pasted literally has no associated fold and stays text.
const FOLD_BASE: u32 = 0xE000;
/// Maximum simultaneously attached folds; deleting a mark releases its slot.
const FOLD_MAX: usize = 64;

/// What one folded mark stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fold {
    /// A big paste. It IS text and comes back as text when the line is sent.
    Text(String),
    /// A picture. It is not text and contributes nothing to the sentence — it
    /// travels beside it. The label is what the person calls it; `id` is what
    /// the frontend matches back to the stored file.
    ///
    /// Same mechanism as a folded paste on purpose: an attachment then behaves
    /// like everything else in the box. Arrow keys step over it, backspace
    /// takes it away, and "delete the placeholder" means "never mind that
    /// picture" without anything extra being written to make that true.
    Image { label: String, id: usize },
    /// A file dragged onto the terminal. TEXT, like a paste — its path is what
    /// gets sent, and the agent opens the file itself — but shown as the file
    /// it is rather than as a count of characters.
    ///
    /// Folded for the same reason a picture is: a dragged path is sixty
    /// characters of somebody else's directory layout filling a box three rows
    /// tall, and taking it back should be one backspace rather than sixty.
    File { path: String, label: String },
}

/// A single- or multi-line text buffer with a cursor and an input history.
///
/// A big paste is FOLDED: the buffer keeps one character standing for it and
/// the body goes in `folds`. That one character is what makes the rest of this
/// type work unchanged — every cursor move, every delete, every word jump
/// already steps by whole characters. All range edits share one implementation
/// that moves or removes fold identities with their marked character.
///
/// Two projections out, and the difference matters: [`shown`](Self::shown) is
/// what the box displays, [`expanded`](Self::expanded) is what gets sent.
#[derive(Debug, Default, Clone)]
pub struct Editor {
    /// The buffer, with a folded paste standing as one Private Use character.
    text: String,
    /// Byte offset of the cursor, on a char boundary, in `0..=text.len()`
    cursor: usize,
    /// Fold identities indexed by byte position, never by character value.
    folds: BTreeMap<usize, Fold>,
    /// Preferred display column during consecutive vertical cursor moves.
    vertical_column: Option<usize>,
    /// Submitted lines, oldest first
    history: Vec<String>,
    /// While browsing history: index into `history`; `None` = editing live
    browse: Option<usize>,
    /// The live line, stashed while browsing history so it can be restored
    stash: String,
    stash_folds: BTreeMap<usize, Fold>,
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    // ── editing ────────────────────────────────────────────

    pub fn insert(&mut self, c: char) {
        self.insert_str(c.encode_utf8(&mut [0; 4]));
    }

    /// Insert literal text. Fold identities can only be created by `fold`.
    pub fn insert_str(&mut self, s: &str) {
        self.replace(self.cursor..self.cursor, s);
    }

    /// The only range-edit operation: keep fold positions and text inseparable.
    fn replace(&mut self, range: std::ops::Range<usize>, replacement: &str) {
        let removed = range.end - range.start;
        self.folds = std::mem::take(&mut self.folds)
            .into_iter()
            .filter_map(|(at, fold)| {
                if at < range.start {
                    Some((at, fold))
                } else if at >= range.end {
                    Some((at - removed + replacement.len(), fold))
                } else {
                    None
                }
            })
            .collect();
        self.text.replace_range(range.clone(), replacement);
        self.cursor = range.start + replacement.len();
        self.detach();
    }

    /// Take a paste, folding it to a placeholder if it is big.
    ///
    /// Unfolded, a long paste filled the box, which shows a few rows at most —
    /// so you could see neither how much arrived nor where it ended, and the
    /// caret sat pinned on the last visible row.
    ///
    /// Small pastes are inserted literally: below the threshold a paste is just
    /// typing that arrived quickly, and hiding it would be worse than showing it.
    pub fn paste(&mut self, s: &str) {
        let lines = s.lines().count().max(1);
        if (lines < FOLD_LINES && s.len() < FOLD_BYTES) || self.folds.len() >= FOLD_MAX {
            self.insert_str(s);
            return;
        }
        self.fold(Fold::Text(s.to_string()));
    }

    /// Put a picture in the line, as a placeholder among the words.
    ///
    /// `id` is the frontend's handle on the stored file; [`images`](Self::images)
    /// gives back the ones STILL in the buffer, in the order they appear.
    /// Nothing else is needed to cancel one: deleting the placeholder is the
    /// cancellation.
    pub fn attach(&mut self, label: &str, id: usize) -> bool {
        if self.folds.len() >= FOLD_MAX {
            return false;
        }
        self.fold(Fold::Image {
            label: label.to_string(),
            id,
        });
        true
    }

    /// Put a dragged file in the line as one placeholder, shown as `label`.
    ///
    /// Its PATH is what the line expands to, so what reaches the model is the
    /// same sentence it would have got from a bare paste. Only the box changes.
    pub fn attach_file(&mut self, path: &str, label: &str) -> bool {
        if self.folds.len() >= FOLD_MAX {
            return false;
        }
        self.fold(Fold::File {
            path: path.to_string(),
            label: label.to_string(),
        });
        true
    }

    fn fold(&mut self, what: Fold) {
        let at = self.cursor;
        self.insert(char::from_u32(FOLD_BASE).expect("a valid private-use character"));
        self.folds.insert(at, what);
    }

    /// The pictures still in the line, in the order they appear in it.
    ///
    /// Read off the BUFFER, never off the list: a placeholder the person
    /// deleted must not come back at send time.
    pub fn images(&self) -> Vec<usize> {
        self.folds
            .values()
            .filter_map(|fold| match fold {
                Fold::Image { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn fold_at(&self, at: usize) -> Option<&Fold> {
        self.folds.get(&at)
    }

    /// How a fold reads in the box.
    fn placeholder(what: &Fold) -> String {
        match what {
            Fold::Image { label, .. } => format!("[image {label}]"),
            Fold::File { label, .. } => format!("[file {label}]"),
            Fold::Text(body) => {
                let lines = body.lines().count();
                if lines > 1 {
                    format!("[pasted {lines} lines]")
                } else {
                    format!("[pasted {} chars]", body.chars().count())
                }
            }
        }
    }

    /// What the input box SHOWS: each folded paste as one placeholder.
    ///
    /// Borrowed when nothing is folded, which is almost always.
    pub fn shown(&self) -> std::borrow::Cow<'_, str> {
        if self.folds.is_empty() {
            return std::borrow::Cow::Borrowed(&self.text);
        }
        let mut out = String::with_capacity(self.text.len());
        for (at, c) in self.text.char_indices() {
            match self.fold_at(at) {
                Some(what) => out.push_str(&Self::placeholder(what)),
                None => out.push(c),
            }
        }
        std::borrow::Cow::Owned(out)
    }

    /// Where the cursor sits in [`shown`](Self::shown) — a byte offset into
    /// THAT string, which is longer than the buffer wherever a paste is folded.
    pub fn shown_cursor(&self) -> usize {
        let mut at = 0;
        for (offset, c) in self.text[..self.cursor].char_indices() {
            at += match self.fold_at(offset) {
                Some(what) => Self::placeholder(what).len(),
                None => c.len_utf8(),
            };
        }
        at
    }

    /// What gets SENT: every folded paste back to its body, and every picture
    /// gone — a picture is not part of the sentence, it travels beside it (see
    /// [`images`](Self::images)).
    pub fn expanded(&self) -> std::borrow::Cow<'_, str> {
        if self.folds.is_empty() {
            return std::borrow::Cow::Borrowed(&self.text);
        }
        let mut out = String::with_capacity(self.text.len());
        for (at, c) in self.text.char_indices() {
            match self.fold_at(at) {
                Some(Fold::Text(body)) => out.push_str(body),
                Some(Fold::File { path, .. }) => out.push_str(path),
                Some(Fold::Image { .. }) => {}
                None => out.push(c),
            }
        }
        std::borrow::Cow::Owned(out)
    }

    /// Replace the whole buffer, cursor to the end (used by completion/history).
    pub fn set(&mut self, s: impl Into<String>) {
        self.text = s.into();
        self.folds.clear();
        self.cursor = self.text.len();
        self.detach();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.browse = None;
        self.stash.clear();
        self.stash_folds.clear();
        self.vertical_column = None;
        self.folds.clear();
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.replace(self.cursor - c.len_utf8()..self.cursor, "");
        }
    }

    /// Delete the character under the cursor (the Delete key).
    pub fn delete(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.replace(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    /// Delete the word before the cursor (Ctrl-W).
    pub fn delete_word(&mut self) {
        self.replace(word_start(&self.text, self.cursor)..self.cursor, "");
    }

    /// Delete from the cursor to the end of the current line (Ctrl-K).
    pub fn kill_to_end(&mut self) {
        let end = line_end(&self.text, self.cursor);
        if end > self.cursor {
            self.replace(self.cursor..end, "");
        }
    }

    /// Delete from the start of the current line to the cursor (Ctrl-U).
    pub fn kill_to_start(&mut self) {
        let start = line_start(&self.text, self.cursor);
        if self.cursor > start {
            self.replace(start..self.cursor, "");
        }
    }

    // ── cursor movement ────────────────────────────────────

    pub fn left(&mut self) {
        self.vertical_column = None;
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        self.vertical_column = None;
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn word_left(&mut self) {
        self.vertical_column = None;
        self.cursor = word_start(&self.text, self.cursor);
    }

    pub fn word_right(&mut self) {
        self.vertical_column = None;
        let right = &self.text[self.cursor..];
        let ws = right.len() - right.trim_start_matches(char::is_whitespace).len();
        let after = &right[ws..];
        let word = after.len() - after.trim_start_matches(|c: char| !c.is_whitespace()).len();
        self.cursor += ws + word;
    }

    /// To the start of the current line (Home / Ctrl-A).
    pub fn home(&mut self) {
        self.vertical_column = None;
        self.cursor = line_start(&self.text, self.cursor);
    }

    /// To the end of the current line (End / Ctrl-E).
    pub fn end(&mut self) {
        self.vertical_column = None;
        self.cursor = line_end(&self.text, self.cursor);
    }

    /// Move between displayed rows, preserving the preferred column. Fold
    /// labels may wrap, but the cursor can only land before or after a fold.
    /// False means there is no editable row in that direction (history may act).
    pub fn vertical(&mut self, width: usize, down: bool) -> bool {
        let shown = self.shown();
        let layout = InputLayout::new(&shown, width);
        let (row, column) = layout.position(self.shown_cursor());
        let goal = self.vertical_column.unwrap_or(column);
        let mut shown_at = 0;
        let mut candidates = Vec::new();
        for (at, c) in self.text.char_indices() {
            let (r, col) = layout.position(shown_at);
            candidates.push((at, r, col));
            shown_at += self
                .fold_at(at)
                .map_or(c.len_utf8(), |f| Self::placeholder(f).len());
        }
        let (r, col) = layout.position(shown_at);
        candidates.push((self.text.len(), r, col));
        let target = candidates
            .into_iter()
            .filter(|(_, r, _)| if down { *r > row } else { *r < row })
            .min_by_key(|(_, r, col)| (r.abs_diff(row), col.abs_diff(goal)));
        if let Some((at, _, _)) = target {
            self.cursor = at;
            self.vertical_column = Some(goal);
            true
        } else {
            false
        }
    }

    // ── submit & history ───────────────────────────────────

    /// Take the current line, clearing the buffer and recording it in history
    /// (skipping blanks and consecutive duplicates).
    pub fn submit(&mut self) -> String {
        // EXPANDED, and the expansion happens before the buffer is dropped:
        // what gets sent, and what history recalls, is the text itself. A mark
        // is a display device and must never leave this type.
        let line = self.expanded().into_owned();
        self.text.clear();
        self.folds.clear();
        self.cursor = 0;
        self.browse = None;
        self.stash.clear();
        self.stash_folds.clear();
        self.vertical_column = None;
        if !line.trim().is_empty() && self.history.last().map(String::as_str) != Some(line.as_str())
        {
            self.history.push(line.clone());
        }
        line
    }

    /// Recall an older line (Up).
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.browse {
            None => {
                self.stash = std::mem::take(&mut self.text);
                self.stash_folds = std::mem::take(&mut self.folds);
                self.history.len() - 1
            }
            Some(0) => return,
            Some(i) => i - 1,
        };
        self.browse = Some(next);
        self.text = self.history[next].clone();
        self.folds.clear();
        self.vertical_column = None;
        self.cursor = self.text.len();
    }

    /// Move toward newer lines, restoring the stashed live line past the end (Down).
    pub fn history_next(&mut self) {
        let Some(i) = self.browse else {
            return;
        };
        if i + 1 < self.history.len() {
            self.browse = Some(i + 1);
            self.text = self.history[i + 1].clone();
            self.folds.clear();
        } else {
            self.browse = None;
            self.text = std::mem::take(&mut self.stash);
            self.folds = std::mem::take(&mut self.stash_folds);
        }
        self.vertical_column = None;
        self.cursor = self.text.len();
    }

    /// Editing the buffer commits it as the live line (stops history browsing).
    fn detach(&mut self) {
        self.browse = None;
        self.stash.clear();
        self.stash_folds.clear();
        self.vertical_column = None;
    }
}

/// Start of the word before `at`: skip trailing whitespace, then a run of
/// non-whitespace.
fn word_start(text: &str, at: usize) -> usize {
    let left = &text[..at];
    let no_space = left.trim_end_matches(char::is_whitespace);
    no_space
        .trim_end_matches(|c: char| !c.is_whitespace())
        .len()
}

/// Byte offset of the start of the line containing `at` (after the prior `\n`).
fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Byte offset of the end of the line containing `at` (before the next `\n`).
fn line_end(text: &str, at: usize) -> usize {
    let rest = &text[at..];
    at + rest.find('\n').unwrap_or(rest.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> Editor {
        let mut e = Editor::new();
        e.set(s);
        e
    }

    #[test]
    fn insert_and_move_and_delete() {
        let mut e = Editor::new();
        for c in "abc".chars() {
            e.insert(c);
        }
        assert_eq!(e.text(), "abc");
        assert_eq!(e.cursor(), 3);
        e.left();
        e.left();
        assert_eq!(e.cursor(), 1);
        e.insert('X');
        assert_eq!(e.text(), "aXbc");
        assert_eq!(e.cursor(), 2);
        e.backspace();
        assert_eq!(e.text(), "abc");
        e.delete();
        assert_eq!(e.text(), "ac");
    }

    #[test]
    fn multibyte_moves_by_whole_characters() {
        let mut e = ed("你好");
        assert_eq!(e.cursor(), 6); // two 3-byte chars, cursor at end
        e.left();
        assert_eq!(e.cursor(), 3); // one char back, on a boundary
        e.insert('x');
        assert_eq!(e.text(), "你x好");
    }

    #[test]
    fn word_jumps_and_word_delete() {
        let mut e = ed("hello  world");
        e.word_left();
        assert_eq!(e.cursor(), 7, "to the start of 'world'");
        e.word_left();
        assert_eq!(e.cursor(), 0, "to the start of 'hello'");
        e.end();
        e.delete_word();
        assert_eq!(e.text(), "hello  ");
    }

    #[test]
    fn home_end_and_kills_are_line_aware() {
        let mut e = ed("one\ntwo three");
        e.home();
        assert_eq!(e.cursor(), 4, "start of the second line");
        e.end();
        assert_eq!(e.cursor(), 13, "end of the second line");
        // kill to start of line, from mid-line
        let mut e = ed("one\ntwo three");
        e.home();
        e.word_right(); // past "two"
        e.kill_to_end();
        assert_eq!(e.text(), "one\ntwo");
    }

    #[test]
    fn history_recall_and_restore() {
        let mut e = Editor::new();
        e.set("first");
        assert_eq!(e.submit(), "first");
        e.set("second");
        assert_eq!(e.submit(), "second");
        // typing a live line, then browsing back
        e.insert('h');
        e.insert('i');
        e.history_prev();
        assert_eq!(e.text(), "second");
        e.history_prev();
        assert_eq!(e.text(), "first");
        e.history_prev();
        assert_eq!(e.text(), "first", "at the oldest, stays put");
        e.history_next();
        assert_eq!(e.text(), "second");
        e.history_next();
        assert_eq!(e.text(), "hi", "past the newest restores the live line");
    }

    /// The whole point: the box shows a placeholder, the wire gets the text.
    #[test]
    fn a_big_paste_shows_as_one_line_and_sends_as_all_of_them() {
        let body: String = (1..=342).map(|n| format!("line {n}\n")).collect();
        let mut e = Editor::new();
        e.insert_str("look at ");
        e.paste(&body);
        e.insert_str(" please");

        assert_eq!(e.shown(), "look at [pasted 342 lines] please");
        assert_eq!(
            e.shown().split('\n').count(),
            1,
            "one row, so the box does not fill up"
        );
        assert_eq!(e.expanded(), format!("look at {body} please"));
        assert_eq!(
            e.submit(),
            format!("look at {body} please"),
            "and what is sent is the text, never a mark"
        );
    }

    /// The whole point of sharing the mechanism: a picture behaves like every
    /// other thing in the box. It sits among the words, arrows step over it,
    /// and deleting the placeholder is how you say "never mind that one" —
    /// nothing else has to be written to make that true.
    #[test]
    fn a_picture_sits_in_the_line_and_deleting_it_cancels_it() {
        let mut e = Editor::new();
        e.insert_str("what is ");
        assert!(e.attach("shot.png", 7));
        e.insert_str(" and ");
        assert!(e.attach("clipboard", 9));
        e.insert_str("?");

        assert_eq!(e.shown(), "what is [image shot.png] and [image clipboard]?");
        assert_eq!(
            e.expanded(),
            "what is  and ?",
            "a picture is not part of the sentence; it travels beside it"
        );
        assert_eq!(e.images(), vec![7, 9], "in the order they appear");

        // Backspace over the second placeholder takes the whole picture
        e.end();
        e.left(); // past '?'
        e.backspace();
        assert_eq!(e.shown(), "what is [image shot.png] and ?");
        assert_eq!(e.images(), vec![7], "and it is no longer being sent");
    }

    /// Read off the BUFFER, never off the list of what was attached: a
    /// placeholder the person deleted must not come back at send time.
    #[test]
    fn a_cancelled_picture_does_not_return_when_the_line_is_sent() {
        let mut e = Editor::new();
        assert!(e.attach("gone.png", 1));
        e.backspace();
        assert!(e.images().is_empty());
        assert_eq!(e.shown(), "");
        e.insert_str("never mind");
        assert_eq!(e.submit(), "never mind");
        assert!(e.images().is_empty());
    }

    /// Pictures and pastes share one mechanism, so they share one line without
    /// either one confusing the other.
    #[test]
    fn a_picture_and_a_paste_can_share_a_line() {
        let body: String = (1..=12).map(|n| format!("l{n}\n")).collect();
        let mut e = Editor::new();
        e.paste(&body);
        e.insert_str(" about ");
        assert!(e.attach("shot.png", 3));

        assert_eq!(e.shown(), "[pasted 12 lines] about [image shot.png]");
        assert_eq!(
            e.expanded(),
            format!("{body} about "),
            "the paste comes back as text, the picture does not"
        );
        assert_eq!(e.images(), vec![3]);
    }

    /// A paste small enough to read is just typing that arrived fast. Hiding it
    /// would be worse than showing it.
    #[test]
    fn a_small_paste_is_left_alone() {
        let mut e = Editor::new();
        e.paste("one\ntwo\nthree");
        assert_eq!(e.shown(), "one\ntwo\nthree");
        assert!(!e.shown().contains("pasted"));
    }

    /// One character in the buffer, so everything that already stepped by
    /// characters treats it as one thing — no special cases anywhere else.
    #[test]
    fn a_folded_paste_is_one_indivisible_character() {
        let body: String = (1..=20).map(|n| format!("l{n}\n")).collect();
        let mut e = Editor::new();
        e.insert_str("a");
        e.paste(&body);
        e.insert_str("b");

        // Left from the end: past 'b', then past the WHOLE paste, then 'a'.
        e.end();
        e.left();
        assert_eq!(e.expanded(), format!("a{body}b"), "moving changes nothing");
        e.left();
        assert_eq!(
            e.shown_cursor(),
            1,
            "one step clears the entire placeholder"
        );

        // Backspace at its right edge takes the whole paste, not one line of it.
        e.end();
        e.left();
        e.backspace();
        assert_eq!(e.expanded(), "ab");
        assert_eq!(e.shown(), "ab");
    }

    /// The caret must point at the character it appears to point at, or typing
    /// lands somewhere else than where it looks.
    #[test]
    fn the_caret_is_placed_within_what_is_shown() {
        let body: String = (1..=9).map(|n| format!("l{n}\n")).collect();
        let mut e = Editor::new();
        e.paste(&body);
        let mark = e.shown().len();
        assert_eq!(e.shown_cursor(), mark, "at the end of the placeholder");
        assert_eq!(
            e.shown_cursor(),
            "[pasted 9 lines]".len(),
            "which is as wide as the placeholder reads"
        );
        e.insert_str("!");
        assert_eq!(e.shown(), "[pasted 9 lines]!");
        assert_eq!(e.shown_cursor(), e.shown().len());
    }

    /// Two pastes in one line keep their own bodies.
    #[test]
    fn two_pastes_do_not_share_a_body() {
        let a: String = (1..=6).map(|n| format!("a{n}\n")).collect();
        let b: String = (1..=7).map(|n| format!("b{n}\n")).collect();
        let mut e = Editor::new();
        e.paste(&a);
        e.insert_str(" vs ");
        e.paste(&b);
        assert_eq!(e.shown(), "[pasted 6 lines] vs [pasted 7 lines]");
        assert_eq!(e.expanded(), format!("{a} vs {b}"));
    }

    /// A long single line floods the box just as badly as many short ones.
    #[test]
    fn a_long_single_line_folds_too_and_says_so_in_characters() {
        let body = "x".repeat(FOLD_BYTES + 1);
        let mut e = Editor::new();
        e.paste(&body);
        assert_eq!(e.shown(), format!("[pasted {} chars]", FOLD_BYTES + 1));
        assert_eq!(e.expanded(), body);
    }

    /// History holds what was SENT. Recalling it must not resurrect a mark,
    /// whose body was dropped when the line was submitted.
    #[test]
    fn history_recalls_the_text_not_the_placeholder() {
        let body: String = (1..=8).map(|n| format!("h{n}\n")).collect();
        let mut e = Editor::new();
        e.paste(&body);
        e.submit();
        e.history_prev();
        assert_eq!(e.shown(), body, "the real text comes back");
        assert_eq!(e.expanded(), body);
    }

    #[test]
    fn blanks_and_duplicates_are_not_recorded() {
        let mut e = Editor::new();
        e.set("cmd");
        e.submit();
        e.set("cmd");
        e.submit();
        e.set("   ");
        e.submit();
        assert_eq!(e.history, vec!["cmd".to_string()]);
    }

    // ── What holds after every keystroke, whatever the keystrokes were ─────
    //
    // The tests above each drive one editing move and check its result. What
    // none of them reaches is a LONG sequence: a paste folded, the cursor
    // walked into the middle of it by word, a picture attached, history
    // recalled over the top, then backspace. Positions here are byte offsets
    // into a string that a fold can lengthen or shorten under the cursor, and
    // the failure mode is not a wrong answer — it is a panic, in the input box,
    // while somebody is typing.
    //
    // These look inside the type on purpose. Whether a fold's body survives is
    // a relation between the buffer and `folds`, and the public API cannot ask
    // it: `expanded()` is exactly the thing under suspicion.

    /// xorshift64*, seeded and fixed. A failure names a seed and an operation
    /// count, and those two bring the same sequence back.
    struct Seeded(u64);

    impl Seeded {
        fn below(&mut self, n: u64) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
        }
    }

    /// One editing move. The character menu is the point: a combining mark
    /// binds to what precedes it, a wide character is two columns and several
    /// bytes, and a newline makes the buffer multi-line — each of them is a
    /// different way for a byte offset to land somewhere it should not.
    fn step(e: &mut Editor, rng: &mut Seeded) {
        const CHARS: [char; 10] = [
            'a', ' ', '\n', '中', '🙂', '\u{0301}', '\t', '.', '\u{e000}', '\u{e03f}',
        ];
        match rng.below(22) {
            0 => e.insert(CHARS[rng.below(CHARS.len() as u64) as usize]),
            1 => e.insert_str("ab"),
            2 => e.insert_str("中文字"),
            3 => e.paste("short paste"),
            // Over the byte ceiling: folded to one placeholder character.
            4 => e.paste(&"x".repeat(FOLD_BYTES + 10)),
            // Over the line ceiling: folded too, and shown as a line count.
            5 => e.paste(&"line\n".repeat(FOLD_LINES + 2)),
            6 => {
                e.attach("shot.png", rng.below(4) as usize);
            }
            7 => {
                e.attach_file("/tmp/some where/notes.txt", "notes.txt");
            }
            8 => e.backspace(),
            9 => e.delete(),
            10 => e.delete_word(),
            11 => e.kill_to_end(),
            12 => e.kill_to_start(),
            13 => e.left(),
            14 => e.right(),
            15 => e.word_left(),
            16 => e.word_right(),
            17 => e.home(),
            18 => e.end(),
            19 => {
                e.submit();
            }
            20 => e.history_prev(),
            _ => e.history_next(),
        }
    }

    /// Everything the type says about itself, asked of whatever state it is in.
    fn holds(e: &Editor, note: &str) {
        assert!(
            e.cursor <= e.text.len(),
            "{note}: cursor {} is past the end of {:?}",
            e.cursor,
            e.text
        );
        assert!(
            e.text.is_char_boundary(e.cursor),
            "{note}: cursor {} splits a character in {:?}",
            e.cursor,
            e.text
        );

        // `shown_cursor` is a byte offset into `shown`, so it has to be one.
        let shown = e.shown();
        let at = e.shown_cursor();
        assert!(
            at <= shown.len(),
            "{note}: shown cursor {at} is past the end of {shown:?}"
        );
        assert!(
            shown.is_char_boundary(at),
            "{note}: shown cursor {at} splits a character in {shown:?}"
        );

        // A fold whose mark is still in the buffer must still be carried; one
        // whose mark was deleted must be gone, mark and body together, because
        // deleting the placeholder is how a person takes the paste back.
        let expanded = e.expanded();
        for (n, fold) in &e.folds {
            assert!(
                e.text.is_char_boundary(*n),
                "{note}: fold splits a character"
            );
            assert_eq!(e.text[*n..].chars().next(), char::from_u32(FOLD_BASE));
            match fold {
                Fold::Text(body) => assert!(
                    expanded.contains(body.as_str()),
                    "{note}: paste {n} is still in the box and not in what would be sent"
                ),
                Fold::File { path, .. } => assert!(
                    expanded.contains(path.as_str()),
                    "{note}: file {n} is still in the box and not in what would be sent"
                ),
                // A picture is not part of the sentence. Its label must never
                // become text — it travels beside the message, not inside it.
                Fold::Image { label, .. } => assert!(
                    !expanded.contains(label.as_str()),
                    "{note}: picture {n}'s label leaked into the sentence"
                ),
            }
        }

        // Reading is not editing: asking twice gives the same answer.
        assert_eq!(e.shown(), shown, "{note}: shown() is not stable");
        assert_eq!(e.expanded(), expanded, "{note}: expanded() is not stable");
    }

    #[test]
    fn no_sequence_of_edits_leaves_the_box_in_a_broken_state() {
        for seed in [1u64, 3, 17, 99, 512, 2027, 65_537, 999_331] {
            let mut rng = Seeded(seed | 1);
            let mut e = Editor::new();
            for n in 0..400 {
                step(&mut e, &mut rng);
                holds(&e, &format!("seed {seed}, step {n}"));
            }
        }
    }

    /// The moves that do nothing must keep doing nothing. An empty box is the
    /// state a person is in most often, and every one of these is one key away.
    #[test]
    fn the_moves_that_do_nothing_are_safe_to_repeat() {
        let mut e = Editor::new();
        for _ in 0..3 {
            e.backspace();
            e.delete();
            e.delete_word();
            e.kill_to_end();
            e.kill_to_start();
            e.left();
            e.right();
            e.word_left();
            e.word_right();
            e.home();
            e.end();
            e.history_prev();
            e.history_next();
            e.clear();
            assert_eq!(e.text, "");
            assert_eq!(e.cursor, 0);
            holds(&e, "empty box");
        }
        assert_eq!(e.submit(), "", "an empty box submits nothing");
        assert_eq!(e.submit(), "", "and submitting again is still nothing");
        assert!(e.history.is_empty(), "and neither one is worth recalling");
    }
}
