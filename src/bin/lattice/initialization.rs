//! Terminal-host startup ordering. Main and debug deliberately install differently.
use super::*;

pub(super) struct Main {
    pub title: String,
    pub workspace: String,
    pub effort: EffortView,
    pub models: ModelView,
    pub running: lattice::models::Entry,
    pub history_end: u64,
    pub documents: Option<std::path::PathBuf>,
    pub parts: Vec<lattice::Assembled>,
    pub expert_dir: Option<std::path::PathBuf>,
    pub stream_id: String,
    pub tab_config: PresetConfig,
    pub main_ledger: std::path::PathBuf,
}

impl Main {
    pub fn install(
        self,
        session: &Session,
        startup: &mut Option<StartupTrace>,
    ) -> std::io::Result<(Ui, PresetConfig, std::path::PathBuf)> {
        let effective_window = Some(
            preset::running_entry(&self.tab_config)
                .context_window()
                .unwrap_or(self.tab_config.context_window),
        );
        let mut ui = Ui {
            domain: domain_state::Live {
                title: self.title,
                expert_dir: self.expert_dir,
                stream_id: self.stream_id,
                model: ModelState::new(self.models, self.running, self.effort, effective_window),
                ..domain_state::Live::default()
            },
            workspace: self.workspace,
            bar: configured_bar(),
            entries: Vec::new(),
            background_view: None,
            links: link_actions::LinkOpener::default(),
            event_facts: None,
            cards: None,
            transcript_cache: Default::default(),
            interface_permission: false,
            navigation: None,
            tab_line: String::new(),
            replayed_through: 0,
            input_latency: Default::default(),
            initial_origin: None,
            draft: Draft::new(),
            live_output: live_output::LiveOutput::default(),
            tick: 0,
            browsing: browsing::Browsing::default(),
            flash: None,
            recovery: None,
            parts: self.parts,
            panel: panels::navigation::PanelNavigation::default(),
            documents: self.documents,
            controls: ModelControls::default(),
            experts: expert_controls::ExpertControls::default(),
        };
        if let Some(trace) = startup.as_mut() {
            ui.replay_prefix_traced(
                &session.log_reader(),
                self.history_end,
                trace.replay_memory(),
            )?;
        } else {
            ui.replay_prefix(&session.log_reader(), self.history_end)?;
        }
        ui.tick = SETTLED_TICK * 2;
        ui.browsing.pin();
        if let Some(trace) = startup.as_mut() {
            trace.checkpoint("ui_rebuild");
        }
        ui.parts = session.initial_parts().to_vec();
        Ok((ui, self.tab_config, self.main_ledger))
    }
}

pub(super) struct Debug {
    pub models: ModelView,
    pub config: PresetConfig,
    pub parts: Vec<lattice::Assembled>,
    pub workspace: std::path::PathBuf,
    pub documents: std::path::PathBuf,
    pub vision: bool,
}
impl Debug {
    pub fn install(mut self, past: &[EventEnvelope]) -> Ui {
        let mut ui = Ui::replayed(&[]);
        ui.domain.model = ModelState::new(
            ui.domain.model.catalog().clone(),
            ui.domain.model.running().clone(),
            ui.domain.model.effort().clone(),
            Some(
                preset::running_entry(&self.config)
                    .context_window()
                    .unwrap_or(self.config.context_window),
            ),
        );
        ui.replay_history(past);
        if !past.is_empty() {
            ui.tick = SETTLED_TICK * 2;
        }
        ui.parts = self.parts;
        ui.domain.title = "debug-tui".to_string();
        ui.workspace = self.workspace.display().to_string();
        ui.bar = configured_bar();
        ui.documents = Some(self.documents);
        if self.vision {
            for row in &mut self.models.rows {
                row.accepts_images = true;
            }
        }
        for row in &mut self.models.rows {
            row.window = row.window.or(Some(self.config.context_window));
        }
        ui.domain.model = ModelState::new(
            self.models,
            preset::running_entry(&self.config),
            ui.domain.model.effort().clone(),
            ui.domain.model.effective_window(),
        );
        ui
    }
}

pub(super) fn draw_observed<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    ui: &mut Ui,
    session: &Session,
    startup: &mut Option<StartupTrace>,
    cost: &mut RenderCost,
) -> std::io::Result<Hit>
where
    B::Error: super::backend_error::IntoIoError,
{
    if let Some(trace) = startup.as_mut() {
        trace.checkpoint("pre_draw");
    }
    let frame_began = lattice::input_latency::clock_ns();
    let began = std::time::Instant::now();
    let hit = draw_ui(term, ui)?;
    let frame_ended = lattice::input_latency::clock_ns();
    cost.frame(began.elapsed());
    for note in ui.input_latency.drawn(frame_began, frame_ended) {
        session.note_input_cost(note);
    }
    if let Some(mut note) = StartupTrace::first_frame(startup, session.startup_cost()) {
        note.ui_counts = serde_json::json!({"entries": ui.entry_count(), "calls": ui.domain.event_inputs.counts()[0], "sizes": ui.domain.event_inputs.counts()[1], "started": ui.domain.event_inputs.counts()[2]});
        session.note_startup(note);
    }
    Ok(hit)
}

#[cfg(test)]
#[path = "initialization/tests.rs"]
mod tests;
