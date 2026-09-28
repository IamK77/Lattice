//! One complete display group, without requiring the rest of the transcript.

use std::io;

use serde::{Deserialize, Serialize};

use super::Entry;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptKind {
    Anchor,
    Work,
    Reply,
}

impl TranscriptKind {
    pub fn of(entry: &Entry) -> Self {
        match entry {
            Entry::User(_) | Entry::Wake(_) | Entry::Attachment(_) => Self::Anchor,
            Entry::Tool(_) | Entry::Thinking(_) => Self::Work,
            Entry::Agent(_) | Entry::Error(_) | Entry::Notice(_) | Entry::Approval(_) => {
                Self::Reply
            }
        }
    }
}

/// Ordinals belong to the entire displayed transcript, not to a loaded page.
/// `first == total` denotes the empty group immediately before live streaming.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptGroup {
    pub first: usize,
    pub total: usize,
    pub previous: Option<TranscriptKind>,
    pub entries: Vec<Entry>,
}

impl TranscriptGroup {
    pub fn from_entries(entries: &[Entry], index: usize) -> io::Result<Self> {
        from_entries(entries, index)
    }
}

pub(super) fn joins(left: &Entry, right: &Entry) -> bool {
    matches!((left, right), (Entry::User(_), Entry::User(_)))
        || (TranscriptKind::of(left) == TranscriptKind::Work
            && TranscriptKind::of(right) == TranscriptKind::Work)
}

pub(super) fn from_entries(entries: &[Entry], index: usize) -> io::Result<TranscriptGroup> {
    if index > entries.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transcript group is outside the displayed history",
        ));
    }
    let mut first = index;
    let mut end = index;
    if index < entries.len() {
        while first > 0 && joins(&entries[first - 1], &entries[first]) {
            first -= 1;
        }
        end += 1;
        while end < entries.len() && joins(&entries[end - 1], &entries[end]) {
            end += 1;
        }
    }
    Ok(TranscriptGroup {
        first,
        total: entries.len(),
        previous: first
            .checked_sub(1)
            .map(|at| TranscriptKind::of(&entries[at])),
        entries: entries[first..end].to_vec(),
    })
}
