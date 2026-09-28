//! Local model controls, never historical state. The coordinator owns key
//! priority; model_actions executes catalog/session effects. This owner only
//! edits the controls.

use super::effort_dial::{dial_home, dial_positions};
use lattice::view::{EffortView, ModelForm};

#[cfg(test)]
#[path = "model_controls/tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct ModelControls {
    dial: Option<usize>,
    picker: Option<usize>,
    form: Option<ModelForm>,
    confirm_delete: Option<String>,
}

impl ModelControls {
    pub fn dial(&self) -> Option<usize> {
        self.dial
    }
    pub fn picker(&self) -> Option<usize> {
        self.picker
    }
    pub fn form(&self) -> Option<&ModelForm> {
        self.form.as_ref()
    }
    pub fn deletion(&self) -> Option<&str> {
        self.confirm_delete.as_deref()
    }
    pub fn blocks_paste(&self) -> bool {
        self.dial.is_some() || self.picker.is_some() || self.confirm_delete.is_some()
    }
    pub fn open_dial(&mut self, effort: &EffortView) {
        self.dial = Some(dial_home(effort));
    }
    pub fn close_dial(&mut self) {
        self.dial = None;
    }
    pub fn previous_dial(&mut self) {
        self.dial = self.dial.map(|at| at.saturating_sub(1));
    }
    pub fn next_dial(&mut self) {
        self.dial = self.dial.map(|at| (at + 1).min(dial_positions().len() - 1));
    }
    pub fn take_dial_word(&mut self) -> &'static str {
        let positions = dial_positions();
        positions[self.dial.take().unwrap_or(0).min(positions.len() - 1)]
    }
    #[cfg(test)]
    pub fn seed_picker(&mut self, at: usize) {
        self.picker = Some(at);
    }
    pub fn take_picker(&mut self) -> Option<usize> {
        self.picker.take()
    }
    pub fn close_picker(&mut self) {
        self.picker = None;
    }
    pub fn previous_picker(&mut self) {
        self.picker = self.picker.map(|at| at.saturating_sub(1));
    }
    pub fn next_picker(&mut self, rows: usize) {
        let last = rows.saturating_sub(1);
        self.picker = self.picker.map(|at| (at + 1).min(last));
    }
    pub fn open_form(&mut self) {
        self.form = Some(ModelForm::default());
    }
    pub fn close_form(&mut self) {
        self.form = None;
    }
    pub fn form_problem(&mut self, problem: String) {
        if let Some(form) = self.form.as_mut() {
            form.problem = Some(problem);
        }
    }
    pub fn next_field(&mut self) {
        if let Some(form) = self.form.as_mut() {
            form.at = (form.at + 1) % ModelForm::FIELDS.len();
        }
    }
    pub fn previous_field(&mut self) {
        if let Some(form) = self.form.as_mut() {
            form.at = (form.at + ModelForm::FIELDS.len() - 1) % ModelForm::FIELDS.len();
        }
    }
    pub fn backspace(&mut self) {
        if let Some(form) = self.form.as_mut() {
            form.values[form.at].pop();
        }
    }
    pub fn type_character(&mut self, character: char) {
        if let Some(form) = self.form.as_mut() {
            form.values[form.at].push(character);
            form.problem = None;
        }
    }
    /// True means the form consumed the paste, including a rejected paste.
    pub fn paste_form(&mut self, text: &str) -> bool {
        let Some(form) = self.form.as_mut() else {
            return false;
        };
        if text.chars().any(char::is_control) {
            form.problem = Some("Paste a single value without control characters".into());
        } else {
            form.values[form.at].push_str(text);
            form.problem = None;
        }
        true
    }
    pub fn ask_delete(&mut self, id: String) {
        self.confirm_delete = Some(id);
    }
    pub fn take_deletion(&mut self) -> Option<String> {
        self.confirm_delete.take()
    }
    pub fn cancel_deletion(&mut self) {
        self.confirm_delete = None;
    }
}
