//! Persist initial settings, then apply completed configuration changes from
//! the ledger before starting a restored session. Never consult parent UI to
//! decide the target of an already established conversation.
use super::*;
use std::io::BufRead;
#[cfg(test)]
#[path = "settings/tests.rs"]
mod tests;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Settings {
    entry: lattice::models::Entry,
    thinking: Option<Value>,
    context_window: u64,
    usage_input_field: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    initial: Value,
    settings: Settings,
}

impl Settings {
    pub fn capture(cfg: &PresetConfig) -> Self {
        Self {
            entry: preset::running_entry(cfg),
            thinking: cfg.thinking.clone(),
            context_window: preset::running_entry(cfg)
                .context_window()
                .unwrap_or(cfg.context_window),
            usage_input_field: cfg.usage_input_field.clone(),
        }
    }

    fn apply(&self, cfg: &mut PresetConfig) {
        cfg.adapter = self.entry.adapter.clone();
        cfg.model = self.entry.model.clone();
        cfg.base_url = self.entry.base_url.clone();
        cfg.key_env = self.entry.key_env.clone();
        cfg.profile = self.entry.profile.clone();
        cfg.thinking = self.thinking.clone();
        cfg.context_window = self.context_window;
        cfg.usage_input_field = self.usage_input_field.clone();
    }

    pub fn restore(
        initial: Option<&Self>,
        path: &Path,
        cfg: &mut PresetConfig,
    ) -> Result<Option<String>, String> {
        let reader = path
            .is_dir()
            .then(|| lattice::kernel::log::LogReader::segmented_snapshot(path))
            .transpose()
            .map_err(|error| error.to_string())?;
        Self::restore_using(initial, path, cfg, reader.as_ref())
    }

    fn restore_using(
        initial: Option<&Self>,
        path: &Path,
        cfg: &mut PresetConfig,
        reader: Option<&lattice::kernel::log::LogReader>,
    ) -> Result<Option<String>, String> {
        let initial_key = serde_json::to_value(initial).map_err(|error| error.to_string())?;
        let mut settings = initial.cloned();
        let mut legacy = false;
        let mut through = 0;
        if let (Some(reader), Some(_)) = (&reader, initial) {
            let saved = reader
                .load_checkpoint::<Saved>("terminal-tab-settings", 1, reader.snapshot_end())
                .map_err(|error| error.to_string())?;
            if let Some(state) = saved.state.filter(|state| state.initial == initial_key) {
                settings = Some(state.settings);
                through = saved.through;
            } else {
                eprintln!(
                    "warning: {} cold-restoring side settings: {}",
                    path.display(),
                    saved
                        .cold_reason
                        .as_deref()
                        .unwrap_or("initial configuration changed")
                );
            }
        }
        let mut consume = |event: &EventEnvelope| -> Result<(), String> {
            if settings.is_none() {
                if event.event_type != core_events::STREAM_OPENED {
                    return Err("Legacy side ledger has no startup identity".into());
                }
                let model = event.payload["model"]
                    .as_str()
                    .ok_or("Legacy side ledger has no model")?;
                let adapter = event.payload["adapter"]
                    .as_str()
                    .ok_or("Legacy side ledger has no adapter")?;
                let mut candidates = lattice::models::load();
                let parent = preset::running_entry(cfg);
                if !candidates.iter().any(|entry| {
                    entry.model == parent.model
                        && entry.adapter == parent.adapter
                        && entry.base_url == parent.base_url
                        && entry.key_env == parent.key_env
                }) {
                    candidates.push(parent);
                }
                candidates.retain(|entry| entry.model == model && entry.adapter == adapter);
                if candidates.len() != 1 {
                    return Err("Legacy side model is missing or ambiguous; its endpoint will not be guessed".into());
                }
                let mut recovered = Self::capture(cfg);
                recovered.entry = candidates.remove(0);
                recovered.context_window = recovered
                    .entry
                    .context_window()
                    .unwrap_or(recovered.context_window);
                // An omitted historical setting is not permission to inherit
                // a different conversation's current dial.
                recovered.thinking = None;
                settings = Some(recovered);
                legacy = true;
            }
            let current = settings.as_mut().unwrap();
            if event.event_type == core_events::COMPONENT_REPLACED
                && event.payload["instance"] == preset::MAIN_MODEL
            {
                let config = &event.payload["config"];
                current.entry = lattice::models::Entry {
                    id: config["entryId"]
                        .as_str()
                        .or(config["model"].as_str())
                        .unwrap_or_default()
                        .into(),
                    adapter: ["scripted", "anthropic", "openai", "responses"]
                        .into_iter()
                        .find(|adapter| {
                            Some(preset::brain_name(adapter)) == event.payload["to"].as_str()
                        })
                        .ok_or("Recorded side adapter is not supported")?
                        .into(),
                    model: config["model"]
                        .as_str()
                        .ok_or("Recorded model has no name")?
                        .into(),
                    base_url: config["baseUrl"].as_str().unwrap_or_default().into(),
                    key_env: config["apiKeyEnv"].as_str().unwrap_or_default().into(),
                    profile: config.get("profile").filter(|v| !v.is_null()).cloned(),
                };
                current.context_window = current
                    .entry
                    .context_window()
                    .unwrap_or(current.context_window);
            }
            if event.event_type == core_events::EXTERNAL_INPUT {
                if event.payload["channel"] == lattice::components::context_gate::EFFORT_CHANNEL {
                    current.thinking = event.payload.get("value").filter(|v| !v.is_null()).cloned();
                }
                if event.payload["channel"] == lattice::components::context_gate::MODEL_CHANNEL {
                    if let Some(window) = event.payload["contextWindow"].as_u64() {
                        current.context_window = window;
                    }
                }
            }
            Ok(())
        };
        if let Some(reader) = &reader {
            reader
                .visit_range(through.saturating_add(1), reader.snapshot_end(), |events| {
                    for event in events {
                        consume(event).map_err(std::io::Error::other)?;
                    }
                    Ok(())
                })
                .map_err(|error| error.to_string())?;
        } else {
            for line in std::io::BufReader::new(
                std::fs::File::open(path).map_err(|error| error.to_string())?,
            )
            .lines()
            {
                let line = line.map_err(|error| error.to_string())?;
                if line.trim().is_empty() {
                    continue;
                }
                let event = serde_json::from_str(&line)
                    .map_err(|error| format!("Unreadable side ledger: {error}"))?;
                consume(&event)?;
            }
        }
        let settings = settings.ok_or("Side ledger has no recorded settings")?;
        if let (Some(reader), Some(_)) = (&reader, initial) {
            reader
                .save_checkpoint(
                    "terminal-tab-settings",
                    1,
                    reader.snapshot_end(),
                    &Saved {
                        initial: initial_key,
                        settings: settings.clone(),
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        // Loading the catalog also stages literal keys under their stable
        // environment names. Never store or pass a key value in this index.
        if settings.entry.adapter != "scripted" {
            let _ = lattice::models::load_reported();
            if !settings.entry.key_present() {
                return Err(format!(
                    "Side model key is unavailable: {}",
                    settings.entry.key_env
                ));
            }
        }
        settings.apply(cfg);
        Ok(legacy.then(|| "Legacy side settings recovered from the current catalog; the original endpoint and initial effort were not recorded. Verify /model and /effort before sending.".into()))
    }
}
