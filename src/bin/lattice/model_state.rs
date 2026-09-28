//! The model target and its displayed measurements. Event folding never reads
//! the catalog: callers supply already-read rows and initial configuration.
//! A live catalog or sent effort can differ from the pure recovery projection.

use lattice::{
    components::context_gate,
    core_events as ce,
    models::Entry,
    preset,
    view::{EffortView, ModelView},
    EventEnvelope,
};

#[cfg(test)]
#[path = "model_state/tests.rs"]
mod tests;

pub(super) struct ModelState {
    catalog: ModelView,
    running: Entry,
    effort: EffortView,
    effective_window: Option<u64>,
}

impl Default for ModelState {
    fn default() -> Self {
        Self::new(
            ModelView::default(),
            Entry {
                id: String::new(),
                adapter: String::new(),
                model: String::new(),
                base_url: String::new(),
                key_env: String::new(),
                profile: None,
            },
            EffortView::default(),
            None,
        )
    }
}

impl ModelState {
    pub fn new(
        catalog: ModelView,
        running: Entry,
        effort: EffortView,
        effective_window: Option<u64>,
    ) -> Self {
        Self {
            catalog,
            running,
            effort,
            effective_window,
        }
    }
    pub fn catalog(&self) -> &ModelView {
        &self.catalog
    }
    pub fn running(&self) -> &Entry {
        &self.running
    }
    pub fn effort(&self) -> &EffortView {
        &self.effort
    }
    pub fn effective_window(&self) -> Option<u64> {
        self.effective_window
    }
    pub fn install_catalog(&mut self, catalog: ModelView) {
        self.catalog = catalog;
    }
    /// Sending effort has always acknowledged its display before any receipt.
    pub fn sent_effort(&mut self, word: String) {
        self.effort.now = Some(word);
    }
    pub fn observe_window(&mut self, event: &EventEnvelope) {
        if event.event_type == ce::EXTERNAL_INPUT
            && event.payload["channel"] == context_gate::MODEL_CHANNEL
        {
            if let Some(window) = event.payload["contextWindow"].as_u64() {
                self.effective_window = Some(window);
            }
        }
    }

    /// Only an actual main-model replacement moves the seat. The returned name
    /// lets the coordinator update the title and invalidate context measurement.
    pub fn observe_swap<'a>(&mut self, event: &'a EventEnvelope) -> Option<&'a str> {
        if event.event_type != ce::COMPONENT_REPLACED
            || event.payload["instance"] != preset::MAIN_MODEL
        {
            return None;
        }
        let config = &event.payload["config"];
        let model = config["model"].as_str()?;
        let host = config["baseUrl"]
            .as_str()
            .map(endpoint_host)
            .unwrap_or_default();
        let key_env = config["apiKeyEnv"].as_str().unwrap_or_default();
        // Historical swap matching deliberately differs from catalog refresh:
        // an omitted endpoint or key does not restrict the match here.
        self.catalog.now = self.catalog.rows.iter().position(|row| {
            row.model == model
                && (host.is_empty() || row.endpoint == host)
                && (key_env.is_empty() || row.key_env == key_env)
        });
        let component = event.payload["to"].as_str().unwrap_or_default();
        if let Some(dialect) = ["anthropic", "scripted", "openai", "responses"]
            .into_iter()
            .find(|adapter| preset::brain_name(adapter) == component)
        {
            self.running.adapter = dialect.to_string();
        }
        self.running.id = config["entryId"].as_str().unwrap_or(model).to_string();
        self.running.model = model.to_string();
        self.running.profile = config
            .get("profile")
            .filter(|value| !value.is_null())
            .cloned();
        if let Some(base_url) = config["baseUrl"].as_str() {
            self.running.base_url = base_url.to_string();
        }
        if let Some(name) = config["apiKeyEnv"].as_str() {
            self.running.key_env = name.to_string();
        }
        // Absent profile and explicit null have different legacy fallbacks.
        self.effort.rungs = if config.get("profile").is_some() {
            self.running.effort_rungs()
        } else {
            self.catalog
                .current()
                .map(|row| row.rungs.clone())
                .unwrap_or_else(|| self.running.effort_rungs())
        };
        self.effective_window = self.running.context_window().or(self.effective_window);
        Some(model)
    }

    #[cfg(test)]
    pub fn fixture_catalog(&mut self) -> &mut ModelView {
        &mut self.catalog
    }
    #[cfg(test)]
    pub fn fixture_running(&mut self) -> &mut Entry {
        &mut self.running
    }
    #[cfg(test)]
    pub fn fixture_effort(&mut self) -> &mut EffortView {
        &mut self.effort
    }
    #[cfg(test)]
    pub fn fixture_window(&mut self) -> &mut Option<u64> {
        &mut self.effective_window
    }
}

/// Preserve the existing display label: remove scheme and trailing slashes,
/// but not the path. Swap matching and catalog row labels use the same rule.
pub(super) fn endpoint_host(base_url: &str) -> String {
    base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .trim_end_matches('/')
        .to_string()
}
