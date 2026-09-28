//! Read the current catalog into display data. Startup and runtime refresh use
//! the same target-matching rule; this module never installs the rows into a UI.

use crate::terminal_host::model_state::endpoint_host;
use lattice::{
    models, preset,
    view::{ModelRow, ModelView},
};

pub(super) fn from_config(cfg: &preset::PresetConfig) -> ModelView {
    load(&preset::running_entry(cfg))
}

/// Read again after catalog edits, rather than treating the startup list as live.
pub(super) fn load(running: &models::Entry) -> ModelView {
    from_entries(models::listing(running), running)
}

fn from_entries(entries: Vec<models::Entry>, running: &models::Entry) -> ModelView {
    // The delete guard protects this row: matching only a model name would
    // protect a spare endpoint and leave the actual running target deletable.
    let now = entries.iter().position(|entry| entry.same_target(running));
    let rows: Vec<ModelRow> = entries
        .into_iter()
        .map(|entry| ModelRow {
            endpoint: endpoint_host(&entry.base_url),
            dialect: entry.adapter.clone(),
            window: entry.context_window(),
            rungs: entry.effort_rungs(),
            accepts_images: entry.accepts_images(),
            key_present: entry.key_present(),
            key_env: entry.key_env,
            id: entry.id,
            model: entry.model,
        })
        .collect();
    ModelView { rows, now }
}

#[cfg(test)]
#[path = "model_catalog/tests.rs"]
mod tests;
