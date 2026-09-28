//! Execute model-related user operations. Key priority stays in the coordinator;
//! historical state and local controls keep their existing owners. Receipts and
//! requested activity are returned as data, never rendered through a callback.

use crate::terminal_host::model_catalog;
use crate::terminal_host::model_controls::ModelControls;
use crate::terminal_host::model_state::ModelState;
use crate::terminal_host::panels::navigation::PanelNavigation;
use lattice::{components::model_common::Effort, models, view::ModelRow, CatalogNote, Session};
use serde_json::json;

pub(super) enum Action<'a> {
    Model(&'a str),
    Effort(&'a str),
    CommitPicker,
    CommitDial,
    OpenForm,
    SubmitForm,
    AskDelete,
    ConfirmDelete,
    CancelDelete,
    SwitchSelected,
}

pub(super) enum Outcome {
    Quiet,
    Message(String),
    ModelRequested,
}

pub(super) fn execute(
    action: Action<'_>,
    model: &mut ModelState,
    controls: &mut ModelControls,
    panel: &mut PanelNavigation,
    session: Option<&Session>,
) -> Outcome {
    let mut operations = Operations {
        model,
        controls,
        panel,
        session,
    };
    match action {
        Action::Model(id) => operations.model(id),
        Action::Effort(value) => operations.effort(value),
        Action::CommitPicker => operations.commit_picker(),
        Action::CommitDial => operations.commit_dial(),
        Action::OpenForm => {
            operations.controls.open_form();
            operations.panel.collapse_details();
            Outcome::Quiet
        }
        Action::SubmitForm => operations.submit_form(),
        Action::AskDelete => operations.ask_delete(),
        Action::ConfirmDelete => operations.confirm_delete(),
        Action::CancelDelete => {
            operations.controls.cancel_deletion();
            Outcome::Message("kept".into())
        }
        Action::SwitchSelected => operations.switch_selected(),
    }
}

struct Operations<'a> {
    model: &'a mut ModelState,
    controls: &'a mut ModelControls,
    panel: &'a mut PanelNavigation,
    session: Option<&'a Session>,
}

impl Operations<'_> {
    fn model(&mut self, id: &str) -> Outcome {
        // Picker and panel choices previously went through the slash parser.
        let id = id.trim();
        if id.is_empty() {
            self.panel.show_models(self.model.catalog().now);
            return Outcome::Quiet;
        }
        match self.session {
            Some(session) => {
                session.set_model(id);
                // This is only a request. The marker and running target move
                // when the ledger confirms the swap, which can be refused.
                Outcome::ModelRequested
            }
            None => Outcome::Message("no session to change the model on".into()),
        }
    }

    fn effort(&mut self, word: &str) -> Outcome {
        let value = match word {
            "" => None,
            "off" | "false" => Some(json!(false)),
            word => Effort::parse(word).map(|rung| json!(rung.name())),
        };
        match (value, self.session) {
            (Some(value), Some(session)) => {
                let word = value.as_str().unwrap_or("off").to_string();
                session.set_effort(value);
                // Effort, unlike the model target, acknowledges locally before
                // a receipt arrives so its current marker moves immediately.
                self.model.sent_effort(word);
                Outcome::Quiet
            }
            (Some(_), None) => Outcome::Message("no session to set the effort on".into()),
            (None, _) => {
                self.controls.open_dial(self.model.effort());
                Outcome::Quiet
            }
        }
    }

    fn commit_picker(&mut self) -> Outcome {
        let choice = self
            .controls
            .take_picker()
            .and_then(|at| self.model.catalog().rows.get(at).map(|row| row.id.clone()));
        let Some(id) = choice else {
            return Outcome::Quiet;
        };
        // Enter on the current position must still answer, without saving the
        // same preference or paying for a swap nobody requested.
        if self
            .model
            .catalog()
            .current()
            .is_some_and(|now| now.id == id)
        {
            return Outcome::Message(format!("model unchanged — still {id}"));
        }
        self.model(&id)
    }

    fn commit_dial(&mut self) -> Outcome {
        // Whatever the result, choosing gives the input box back.
        let word = self.controls.take_dial_word();
        if self.model.effort().now.as_deref() == Some(word) {
            Outcome::Message(format!("effort unchanged — still {word}"))
        } else {
            self.effort(word)
        }
    }

    fn switch_selected(&mut self) -> Outcome {
        let chosen = self
            .model
            .catalog()
            .rows
            .get(self.panel.selected_row())
            .map(|row| row.id.clone());
        if let Some(id) = chosen {
            self.panel.close();
            self.model(&id)
        } else {
            Outcome::Quiet
        }
    }

    fn refresh(&mut self) {
        // Read after writing: the display follows disk and the current target,
        // not the model this process happened to start with.
        self.model
            .install_catalog(model_catalog::load(self.model.running()));
    }

    fn submit_form(&mut self) -> Outcome {
        let form = self.controls.form().cloned().unwrap_or_default();
        let added = form.entry().and_then(|(id, spec)| {
            models::add(&id, spec)?;
            Ok(id)
        });
        match added {
            Err(problem) => {
                self.controls.form_problem(problem);
                Outcome::Quiet
            }
            Ok(id) => {
                self.controls.close_form();
                self.refresh();
                self.panel.select_row(
                    self.model
                        .catalog()
                        .rows
                        .iter()
                        .position(|row| row.id == id)
                        .unwrap_or(0),
                );
                if let (Some(session), Some(row)) = (
                    self.session,
                    self.model.catalog().rows.get(self.panel.selected_row()),
                ) {
                    session.note_catalog_change(catalog_note("added", row));
                }
                Outcome::Message(format!("added {id} — s to switch to it"))
            }
        }
    }

    fn ask_delete(&mut self) -> Outcome {
        let row = self
            .model
            .catalog()
            .rows
            .get(self.panel.selected_row())
            .cloned();
        match row {
            None => Outcome::Quiet,
            // The running row would outlive its file entry, leaving the panel
            // describing a model that no longer exists anywhere else.
            Some(row) if Some(self.panel.selected_row()) == self.model.catalog().now => {
                Outcome::Message(format!(
                    "{} is the model this conversation is using — switch away first",
                    row.id
                ))
            }
            Some(row) => {
                self.controls.ask_delete(row.id);
                Outcome::Quiet
            }
        }
    }

    fn confirm_delete(&mut self) -> Outcome {
        let id = self.controls.take_deletion().unwrap_or_default();
        // Capture before removal: no backup is kept, so the post-write catalog
        // can no longer explain which endpoint and key name were deleted.
        let note = self
            .model
            .catalog()
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| catalog_note("removed", row));
        match models::remove(&id) {
            Ok(()) => {
                self.refresh();
                self.panel.clamp_selection(self.model.catalog().rows.len());
                if let (Some(session), Some(note)) = (self.session, note) {
                    session.note_catalog_change(note);
                }
                Outcome::Message(format!("deleted {id}"))
            }
            Err(problem) => Outcome::Message(problem),
        }
    }
}

/// Build audit data from the display row, never the file entry: rows contain
/// only the NAME of a key variable and cannot carry a literal key value.
fn catalog_note(action: &str, row: &ModelRow) -> CatalogNote {
    CatalogNote {
        action: action.to_string(),
        id: row.id.clone(),
        model: row.model.clone(),
        adapter: row.dialect.clone(),
        endpoint: row.endpoint.clone(),
        key_env: row.key_env.clone(),
    }
}
