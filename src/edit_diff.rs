//! A bounded, serializable snapshot of a file change, captured by the writer.
//! Readers render this evidence; they never reopen the current file for context.
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::time::Duration;

const CONTEXT: usize = 3;
const MAX_LINES: usize = 512;
const MAX_BYTES: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditDiff {
    pub hunks: Vec<DiffHunk>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffHunk {
    /// One-based positions of the next old/new line at the start of this hunk.
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
    pub newline: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Equal,
    Delete,
    Insert,
}

impl EditDiff {
    pub fn between(old: &str, new: &str) -> Self {
        // A deadline bounds pathological comparisons; the fallback is still a
        // valid diff, though it need not be the smallest one possible.
        let diff = TextDiff::configure()
            .timeout(Duration::from_millis(100))
            .diff_lines(old, new);
        let mut snapshot = Self {
            hunks: Vec::new(),
            truncated: false,
        };
        let (mut lines, mut bytes) = (0, 0);
        for group in diff.grouped_ops(CONTEXT) {
            let Some(first) = group.first() else { continue };
            let mut hunk = DiffHunk {
                old_start: first.old_range().start + 1,
                new_start: first.new_range().start + 1,
                lines: Vec::new(),
            };
            for op in group {
                for change in diff.iter_changes(&op) {
                    let value = change.value();
                    if lines == MAX_LINES || bytes + value.len() > MAX_BYTES {
                        snapshot.truncated = true;
                        if !hunk.lines.is_empty() {
                            snapshot.hunks.push(hunk);
                        }
                        return snapshot;
                    }
                    lines += 1;
                    bytes += value.len();
                    hunk.lines.push(DiffLine {
                        kind: match change.tag() {
                            ChangeTag::Equal => DiffKind::Equal,
                            ChangeTag::Delete => DiffKind::Delete,
                            ChangeTag::Insert => DiffKind::Insert,
                        },
                        text: value.strip_suffix('\n').unwrap_or(value).to_owned(),
                        newline: !change.missing_newline(),
                    });
                }
            }
            snapshot.hunks.push(hunk);
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> String {
        (1..=n).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn changes_have_three_context_lines_and_absolute_positions() {
        let old = numbered(20);
        let new = old.replace("line 8\n", "中文甲\n中文乙\n");
        let diff = EditDiff::between(&old, &new);
        assert!(!diff.truncated);
        assert_eq!(diff.hunks.len(), 1);
        let h = &diff.hunks[0];
        assert_eq!((h.old_start, h.new_start), (5, 5));
        assert_eq!(h.lines.first().unwrap().text, "line 5");
        assert_eq!(h.lines.last().unwrap().text, "line 11");
        assert_eq!(
            h.lines.iter().filter(|l| l.kind == DiffKind::Equal).count(),
            6
        );
        assert_eq!(
            h.lines
                .iter()
                .filter(|l| l.kind == DiffKind::Delete)
                .count(),
            1
        );
        assert_eq!(
            h.lines
                .iter()
                .filter(|l| l.kind == DiffKind::Insert)
                .count(),
            2
        );
    }

    #[test]
    fn overlapping_context_merges_and_distant_changes_keep_shifted_positions() {
        let old = numbered(40);
        let nearby = old
            .replace("line 8\n", "first\nextra\n")
            .replace("line 12\n", "second\n");
        assert_eq!(EditDiff::between(&old, &nearby).hunks.len(), 1);
        let distant = old
            .replace("line 8\n", "first\nextra\n")
            .replace("line 30\n", "second\n");
        let diff = EditDiff::between(&old, &distant);
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!((diff.hunks[1].old_start, diff.hunks[1].new_start), (27, 28));
    }

    #[test]
    fn empty_files_boundaries_partial_lines_and_missing_newlines_are_preserved() {
        assert!(EditDiff::between("same", "same").hunks.is_empty());
        let inserted = EditDiff::between("", "中文");
        assert_eq!(inserted.hunks[0].lines[0].kind, DiffKind::Insert);
        assert!(!inserted.hunks[0].lines[0].newline);
        let deleted = EditDiff::between("one\n", "");
        assert_eq!(deleted.hunks[0].lines[0].kind, DiffKind::Delete);
        let partial = EditDiff::between("prefix old suffix\n", "prefix new suffix\n");
        assert_eq!(partial.hunks[0].lines[0].text, "prefix old suffix");
        assert_eq!(partial.hunks[0].lines[1].text, "prefix new suffix");
        let endings = EditDiff::between("same", "same\n");
        assert!(!endings.hunks[0].lines[0].newline);
        assert!(endings.hunks[0].lines[1].newline);
    }

    #[test]
    fn cr_and_crlf_line_endings_are_not_mistaken_for_missing_newlines() {
        for ending in ["\r", "\r\n"] {
            let old = format!("a{ending}b{ending}");
            let new = format!("a{ending}B{ending}");
            let diff = EditDiff::between(&old, &new);
            assert_eq!(diff.hunks[0].lines.len(), 3);
            assert!(
                diff.hunks[0].lines.iter().all(|line| line.newline),
                "{diff:?}"
            );
        }
    }

    #[test]
    fn large_snapshots_say_when_they_are_cut_short() {
        let diff = EditDiff::between("", &numbered(MAX_LINES + 1));
        assert!(diff.truncated);
        assert_eq!(diff.hunks[0].lines.len(), MAX_LINES);
        let diff = EditDiff::between("", &"x".repeat(MAX_BYTES + 1));
        assert!(diff.truncated);
        assert!(diff.hunks.is_empty());
    }
}
