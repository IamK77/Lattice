//! Consume a draft and classify its submission; execution stays at the frontend.
use crate::terminal_host::{candidates, draft::Draft};
use lattice::view::EffortView;

#[derive(Debug, PartialEq)]
pub(super) enum Intent {
    Empty,
    Command(String),
    /// Candidate selection neither sends nor consumes registered images.
    SkillCandidate(String),
    Message {
        text: String,
        kept: Vec<usize>,
    },
}

pub(super) fn submit(
    draft: &mut Draft,
    skills: &[(String, String)],
    effort: &EffortView,
) -> Intent {
    // Match the raw buffer before expanding folds or trimming whitespace.
    let candidates = candidates::slash_matches(draft.editor().text(), skills, effort);
    if !candidates.is_empty() {
        let hint = &candidates[draft.selected().min(candidates.len() - 1)];
        let name = hint.name.clone();
        draft.edit().clear();
        draft.reset_selection();
        return if hint.skill {
            Intent::SkillCandidate(name)
        } else {
            Intent::Command(name)
        };
    }
    // Unlike candidate selection, submit records the untrimmed editor history.
    let (line, kept) = draft.submit();
    let text = line.trim().to_string();
    if text.is_empty() && kept.is_empty() {
        return Intent::Empty;
    }
    let names_a_skill = text.strip_prefix('/').is_some_and(|rest| {
        let token = rest.split_whitespace().next().unwrap_or(rest);
        skills.iter().any(|(name, _)| name == token)
    });
    if names_a_command(&text) && !names_a_skill {
        Intent::Command(text)
    } else {
        Intent::Message { text, kept }
    }
}

/// A command's name is one word with no internal slash, independent of the disk.
/// Thus `/tmp` is command-shaped but an absolute multi-part path is a message.
fn names_a_command(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('/') else {
        return false;
    };
    let token = rest.split_whitespace().next().unwrap_or(rest);
    !token.is_empty() && !token.contains('/')
}

#[cfg(test)]
#[path = "submission/tests.rs"]
mod tests;
