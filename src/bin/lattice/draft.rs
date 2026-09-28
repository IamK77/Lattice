//! Unsent input owns its editor, image references and completion cursor.
//! Files are stored by the caller before attachment; sending is also the
//! caller's responsibility. None of this state belongs to a checkpoint.
use lattice::{contracts::document::DocRef, Editor};
use serde_json::{json, Value};

#[cfg(test)]
#[path = "draft/tests.rs"]
mod tests;

pub(super) struct Draft {
    editor: Editor,
    images: Vec<Value>,
    selected: usize,
}

impl Draft {
    pub fn new() -> Self {
        Self {
            editor: Editor::new(),
            images: Vec::new(),
            selected: 0,
        }
    }
    pub fn editor(&self) -> &Editor {
        &self.editor
    }
    /// Editor owns placeholder identity and all cursor/editing operations.
    /// Raw keystrokes do not reset selection implicitly: callers retain the
    /// existing distinction between typing, backspace and other deletion.
    /// Production image insertion and submission go through Draft's methods,
    /// not Editor::attach/submit; this borrowed editor is not a restricted type.
    pub fn edit(&mut self) -> &mut Editor {
        &mut self.editor
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn reset_selection(&mut self) {
        self.selected = 0;
    }
    pub fn previous_hint(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    /// The caller has already established that the candidate list is nonempty.
    pub fn next_hint(&mut self, count: usize) {
        self.selected = (self.selected + 1).min(count - 1);
    }

    pub fn attach(&mut self, stored: DocRef, media: &str, label: &str) -> bool {
        let clashes = self.images.iter().any(|reference| {
            reference["name"] == label && reference["file"] != stored.file.as_str()
        });
        let label = if clashes {
            let tail: String = stored.file.chars().take(4).collect();
            format!("{label} ·{tail}")
        } else {
            label.to_string()
        };
        let id = self.images.len();
        // Preserve the ordering: even an exhausted editor has recorded the
        // reference before reporting that its placeholder could not be added.
        self.images
            .push(json!({"file":stored.file,"mediaType":media,"bytes":stored.bytes,"name":label}));
        self.editor.attach(&label, id)
    }

    /// Capture live image identities before submit empties the editor.
    pub fn submit(&mut self) -> (String, Vec<usize>) {
        let kept = self.editor.images();
        (self.editor.submit(), kept)
    }

    /// Only called when an ordinary message is actually handed to a session.
    /// Local commands, candidate execution and a missing session retain refs.
    pub fn take_images(&mut self, kept: &[usize]) -> Vec<Value> {
        let images = kept
            .iter()
            .filter_map(|id| self.images.get(*id))
            .cloned()
            .collect();
        self.images.clear();
        images
    }

    #[cfg(test)]
    pub fn references(&self) -> &[Value] {
        &self.images
    }
}
