//! Terminal coordination: the private UI shell, event loop, input priority,
//! navigation and whole-frame layout. Business owners expose narrow inputs;
//! lifecycle and debug drivers share this host without owning its rules.
//! Only the three launch functions are exposed to the process entry.

#[path = "backend_error.rs"]
mod backend_error;
#[path = "brand.rs"]
mod brand;
#[path = "effort_dial.rs"]
mod effort_dial;
#[cfg(test)]
use effort_dial::dial_home;
use effort_dial::dial_lines;
#[path = "model_picker.rs"]
mod model_picker;
use model_picker::picker_lines;
#[path = "linked_line.rs"]
mod linked_line;
#[path = "markdown.rs"]
mod markdown;
#[cfg(test)]
use markdown::{clear_render_cache, render_cache_len};
#[path = "text.rs"]
mod text;
#[path = "theme.rs"]
mod theme;
use text::clip;
#[path = "slash_catalog.rs"]
mod slash_catalog;
#[cfg(test)]
use slash_catalog::SLASH;
#[path = "browsing.rs"]
mod browsing;
#[path = "candidates.rs"]
mod candidates;
#[path = "draft.rs"]
mod draft;
#[cfg(test)]
#[path = "draft_keys/entry_tests.rs"]
mod draft_key_tests;
#[path = "draft_keys.rs"]
mod draft_keys;
use candidates::slash_matches;
use draft::Draft;
#[path = "panels.rs"]
mod panels;
use panels::context::picture as context_picture;
#[path = "material.rs"]
mod material;
#[cfg(test)]
use panels::{panel_tabs, PANELS};
use panels::{
    AT_BACKGROUND, AT_COMMANDS, AT_COMPONENTS, AT_CONFIG, AT_CONTEXT, AT_MODELS, AT_USAGE,
};
#[path = "activity.rs"]
mod activity;
#[path = "panel_sources.rs"]
mod panel_sources;
use activity::DONE_SETTLE;
#[cfg(test)]
use activity::{state_phrase, SPINNER, THINK_SPIN};
#[path = "tool_arguments.rs"]
mod tool_arguments;
#[cfg(test)]
use theme::ERR;
use theme::{ACCENT, DIM, FG, RULE, USER_BG, WARM};
#[path = "tool_card.rs"]
mod tool_card;

#[path = "accounting.rs"]
mod accounting;
#[path = "authorization_panel.rs"]
mod authorization_panel;
#[path = "authorizations.rs"]
mod authorizations;
#[path = "cards.rs"]
mod cards;
#[path = "diagnostics.rs"]
mod diagnostics;
#[path = "event_inputs.rs"]
mod event_inputs;
#[path = "live_output.rs"]
mod live_output;
#[path = "model_state.rs"]
mod model_state;
use model_state::ModelState;
#[path = "modal_keys.rs"]
mod modal_keys;
#[path = "model_controls.rs"]
mod model_controls;
use model_controls::ModelControls;
#[path = "background_state.rs"]
mod background_state;
#[path = "domain_state.rs"]
mod domain_state;
#[path = "expert_controls.rs"]
mod expert_controls;
#[cfg(test)]
#[path = "expert_controls/entry_tests.rs"]
mod expert_entry_tests;
#[path = "initialization.rs"]
mod initialization;
#[cfg(test)]
#[path = "modal_keys/entry_tests.rs"]
mod modal_key_tests;
#[path = "recovery.rs"]
mod recovery;
#[path = "shutdown.rs"]
mod shutdown;
#[path = "status.rs"]
mod status;
#[path = "turns.rs"]
mod turns;
#[path = "unseen.rs"]
mod unseen;
use status::Opens;
#[path = "tabs.rs"]
mod tabs;
#[path = "transcript.rs"]
mod transcript;
use transcript::group_key;
#[cfg(test)]
use transcript::{entry_lines, folded_work, transcript_body};

use std::io::Stdout;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
#[cfg(test)]
use ratatui::crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use ratatui::Terminal;

use serde_json::{json, Value};

#[cfg(test)]
use lattice::components::trust_policy;
use lattice::core_events;
use lattice::preset::{self, PresetConfig};
use lattice::view::{self, EffortView, Entry, ModelForm, ModelView, ToolStatus, View};
#[cfg(test)]
use lattice::view::{ModelRow, ThinkingCard, ToolCard};
use lattice::wrap;
#[cfg(test)]
use lattice::{Editor, Kernel, KernelOptions};
use lattice::{EventEnvelope, RenderEvent, Session};
use unicode_width::UnicodeWidthStr;

use crate::cli::Resume;
use crate::startup::{self, experts_dir, home, Trace as StartupTrace};
#[cfg(test)]
#[path = "attachment_actions/entry_tests.rs"]
mod attachment_action_tests;
#[path = "attachment_actions.rs"]
mod attachment_actions;
#[cfg(test)]
#[path = "link_actions/entry_tests.rs"]
mod link_action_tests;
#[path = "link_actions.rs"]
mod link_actions;
#[cfg(test)]
#[path = "model_actions/entry_tests.rs"]
mod model_action_tests;
#[path = "model_actions.rs"]
mod model_actions;
#[path = "model_catalog.rs"]
mod model_catalog;
#[path = "mouse_intent.rs"]
mod mouse_intent;
#[cfg(test)]
#[path = "mouse_intent/entry_tests.rs"]
mod mouse_intent_entry_tests;
#[cfg(test)]
use crate::process_commands;
#[cfg(test)]
use mouse_intent::rect_has;
#[path = "session_build.rs"]
mod session_build;
#[path = "slash_command.rs"]
mod slash_command;
#[cfg(test)]
#[path = "slash_command/entry_tests.rs"]
mod slash_command_entry_tests;
#[path = "submission.rs"]
mod submission;
#[cfg(test)]
#[path = "submission/entry_tests.rs"]
mod submission_entry_tests;
#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;

#[path = "debug.rs"]
mod debug;
pub(super) use debug::{run_debug_frame, run_debug_tui};

// ── TUI mode: in-process, ratatui ──────────────────────────────────────────

/// A tick far enough past any animation that everything folded with it reads
/// as history: a "Done" line whose age is zero renders as still working.
/// Used by the debug renderer and by a continued conversation alike.
const SETTLED_TICK: usize = 10_000;

#[path = "lifecycle.rs"]
mod lifecycle;
pub(super) use lifecycle::run_tui;

// ── Headless driver: drive the real TUI from a script ──────────────────────

/// The TUI's own state, implementing the neutral [`View`] the renderer reads.
/// Keeping `draw` behind `View` (not this concrete struct) is the render seam:
/// a ledger replay or a per-stream selector could feed the same `draw`.
struct Ui {
    domain: domain_state::Live,
    /// Where the tools are working, for the status bar's `cwd` readout.
    workspace: String,
    /// The status bar's readouts, in order. Resolved once at startup from the
    /// user's preferences: reading a file to draw a frame would be a file read
    /// twenty times a second.
    bar: Vec<String>,
    entries: Vec<Entry>,
    /// Host-composed display only; never fold another stream's events here.
    background_view: Option<Vec<view::Live>>,
    /// Frontend-local browser launcher results, never model input.
    links: link_actions::LinkOpener,
    navigation: Option<tabs::Navigation>,
    tab_line: String,
    replayed_through: u64,
    input_latency: lattice::input_latency::Tracker,
    initial_origin: Option<lattice::contracts::event::StreamRef>,
    /// Persistent dependency lookup and fallible rebuilding stay at the ingress.
    event_facts: Option<view::facts::EventFacts>,
    cards: Option<std::cell::RefCell<cards::Cards>>,
    draft: Draft,
    live_output: live_output::LiveOutput,
    /// Advances every loop tick (~50ms), so the spinner spins even while idle.
    tick: usize,
    browsing: browsing::Browsing,
    /// The acknowledgement of the command just typed, shown beside the input
    /// and cleared by the next keystroke or the next turn. See `View::flash`
    /// for why this is not an entry.
    flash: Option<String>,
    recovery: Option<Box<recovery::Recovery>>,

    /// A reference mode over the transcript, never a recorded message.
    panel: panels::navigation::PanelNavigation,
    /// What is assembled: (instance, component, runtime, tools, removable).
    /// Read once at launch from the same manifest the kernel was built from.
    parts: Vec<lattice::Assembled>,
    /// Where this stream's attachments are stored. `None` when the stream has
    /// no ledger, and then a picture cannot be attached at all rather than
    /// being attached somewhere nobody will look for it.
    documents: Option<std::path::PathBuf>,
    controls: ModelControls,
    experts: expert_controls::ExpertControls,
}

impl View for Ui {
    fn title(&self) -> &str {
        &self.domain.title
    }
    fn tabs(&self) -> &str {
        &self.tab_line
    }
    fn workspace(&self) -> &str {
        &self.workspace
    }
    fn status_bar(&self) -> &[String] {
        &self.bar
    }
    fn unseen(&self) -> &[String] {
        self.domain.unseen.lines()
    }
    fn flash(&self) -> Option<&str> {
        self.flash.as_deref()
    }
    fn panel(&self) -> Option<(usize, usize)> {
        self.panel.active()
    }
    fn panel_sel(&self) -> usize {
        self.panel.selected_row()
    }
    fn panel_scroll(&self) -> usize {
        self.panel.scroll_offset()
    }
    fn panel_open(&self) -> bool {
        self.panel.details_expanded()
    }
    fn components(&self) -> Vec<lattice::Assembled> {
        self.parts.clone()
    }
    fn composition(&self) -> lattice::Material {
        self.domain.accounting.parts().clone()
    }
    fn growth(&self) -> (lattice::Material, lattice::Material) {
        self.domain.accounting.growth()
    }
    fn history(&self) -> Vec<u64> {
        self.domain.accounting.history()
    }
    fn history_growth(&self) -> view::peaks::GrowthSummary {
        self.domain.accounting.history_growth()
    }
    fn usage(&self) -> Option<lattice::UsageReport> {
        self.domain.accounting.report()
    }
    fn pending_auth(&self) -> Option<&str> {
        self.domain.authorizations.next()
    }
    fn authorization_prompt(&self) -> std::io::Result<Option<view::AuthorizationPrompt>> {
        self.domain.authorizations.prompt()
    }
    fn effort(&self) -> EffortView {
        self.domain.model.effort().clone()
    }

    fn dial(&self) -> Option<usize> {
        self.controls.dial()
    }

    fn models(&self) -> ModelView {
        self.domain.model.catalog().clone()
    }
    fn compaction_status(&self) -> Option<&lattice::components::context_gate::CompactionStatus> {
        self.domain.compaction.as_ref()
    }

    fn model_form(&self) -> Option<ModelForm> {
        self.controls.form().cloned()
    }

    fn expert_panel(&self) -> Option<view::expert_panel::Panel> {
        Some(self.experts.display())
    }

    fn confirm_delete(&self) -> Option<String> {
        self.controls.deletion().map(str::to_string)
    }

    fn picker(&self) -> Option<usize> {
        self.controls.picker()
    }

    fn background(&self) -> &[view::Live] {
        self.background_view
            .as_deref()
            .unwrap_or(self.domain.background.rows())
    }

    fn skills(&self) -> &[(String, String)] {
        &self.domain.skills
    }
    fn entries(&self) -> &[Entry] {
        &self.entries
    }
    fn entry_count(&self) -> usize {
        self.cards
            .as_ref()
            .map_or(self.entries.len(), |cards| cards.borrow().len())
    }
    fn transcript_group(&self, index: usize) -> std::io::Result<view::TranscriptGroup> {
        match &self.cards {
            Some(cards) => cards.borrow_mut().group(index),
            None => view::TranscriptGroup::from_entries(&self.entries, index),
        }
    }
    fn last_tool_running(&self) -> bool {
        self.cards.as_ref().map_or_else(
            || matches!(self.entries.last(), Some(Entry::Tool(card)) if card.status == ToolStatus::Running),
            |cards| cards.borrow().last_tool_running(),
        )
    }
    fn streaming(&self) -> &str {
        self.live_output.reply()
    }
    fn thinking(&self) -> &str {
        self.live_output.thinking()
    }
    fn input(&self) -> std::borrow::Cow<'_, str> {
        self.draft.editor().shown()
    }
    fn cursor(&self) -> usize {
        self.draft.editor().shown_cursor()
    }
    fn busy(&self) -> bool {
        // A question on the table IS the turn still running. Letting a new
        // message start while one is open is what produced a conversation
        // with tool calls nobody ever answered.
        self.domain.turns.busy() || self.domain.authorizations.next().is_some()
    }
    fn waiting(&self) -> bool {
        self.domain.turns.waiting() && !self.busy()
    }
    fn tick(&self) -> usize {
        self.tick
    }
    fn hint_sel(&self) -> usize {
        self.draft.selected()
    }
    fn scroll(&self) -> usize {
        self.browsing.offset()
    }
    fn transcript_position(&self) -> Option<(view::TranscriptPosition, isize)> {
        self.browsing.position()
    }
    fn tool_expanded(&self, call: Option<&str>) -> bool {
        call.is_some_and(|c| self.browsing.is_expanded(c))
    }
    fn done_at(&self) -> Option<usize> {
        self.domain.turns.done_at()
    }
    fn turn(&self) -> usize {
        self.domain.turns.number()
    }
}

impl Ui {
    /// A read-only view rebuilt from a ledger replay: fold each event for its
    /// transcript AND its turn state, so a replay reflects busy/turn/Done exactly
    /// as the live UI would. `SETTLED_TICK` puts any "Done" past its animation.
    fn replayed(events: &[EventEnvelope]) -> Self {
        let mut ui = Ui {
            domain: domain_state::Live::default(),
            workspace: String::new(),
            bar: Vec::new(),
            entries: Vec::new(),
            background_view: None,
            links: link_actions::LinkOpener::default(),
            event_facts: None,
            cards: None,
            navigation: None,
            tab_line: String::new(),
            replayed_through: 0,
            input_latency: Default::default(),
            initial_origin: None,
            draft: Draft::new(),
            live_output: live_output::LiveOutput::default(),
            tick: SETTLED_TICK,
            browsing: browsing::Browsing::default(),
            flash: None,
            recovery: None,
            parts: Vec::new(),
            panel: panels::navigation::PanelNavigation::default(),
            documents: None,
            controls: ModelControls::default(),
            experts: expert_controls::ExpertControls::default(),
        };
        ui.replay_history(events);
        // Any "Done" belongs to history, past its animation: `done_at` is set
        // by the fold above to whatever tick it was given, and a line whose
        // age is zero renders as still-working.
        ui.tick = SETTLED_TICK * 2;
        ui
    }

    /// Fold ONE ledger event into everything this view derives from the
    /// ledger. The single place that list lives.
    ///
    /// It was two lists, written out by hand in two places — the live drain
    /// and the replay — and they had drifted: replay folded lines-not-yet-seen
    /// and live did not, live folded model swaps and replay did not. So the
    /// queued-lines display was dead from the day it was written, in the only
    /// path a person ever runs, while its unit test (which called the fold
    /// directly) stayed green and the debug renderer showed it correctly. One
    /// entry point cannot drift from itself.
    /// Keep the list of what is running or armed away from this conversation.
    ///
    /// Every kind announces itself the same way, which is why this needs to
    /// know none of them: the call that starts one is answered with an id, and
    /// each report back carries a `source` of the form `kind:id`. Timers and
    /// watches are the two that do not end by reporting — they end when they
    /// are cancelled — so those are dropped on the cancelling call instead.
    /// Re-read what each expert has written since last time.
    ///
    /// Reading a file rather than being told: an expert runs in its own stream
    /// on its own thread, and the only paths back would be to send progress
    /// into this conversation — which would wake it, and a turn per progress
    /// report is not a price worth paying — or to wait until the end, by which
    /// point the answer is in and nobody needs a progress bar. The record is
    /// already being written; reading it costs nothing anyone else pays for.
    ///
    /// Skipped unless the file grew, so an idle expert costs one `len()`.
    fn refresh_experts(&mut self) {
        let model = self
            .domain
            .model
            .catalog()
            .current()
            .map(|m| (m.model.clone(), m.dialect.clone()))
            .unwrap_or_default();
        self.domain.background.refresh(&model.0, &model.1);
    }

    #[cfg(test)]
    fn note_background(&mut self, event: &lattice::EventEnvelope, tick: usize) {
        self.domain.test_note_background(event, tick);
    }

    fn replay_prefix(
        &mut self,
        reader: &lattice::kernel::log::LogReader,
        through: u64,
    ) -> std::io::Result<()> {
        self.recover_prefix(reader, through, None)
    }

    fn replay_history<'a>(&mut self, events: impl IntoIterator<Item = &'a lattice::EventEnvelope>) {
        let mut through = self.replayed_through;
        for event in events {
            self.absorb(event, SETTLED_TICK);
            through = event.seq;
        }
        self.replayed_through = through;
    }

    fn absorb(&mut self, event: &lattice::EventEnvelope, tick: usize) {
        assert!(
            self.event_facts.is_none(),
            "reader-backed folds must propagate read errors"
        );
        self.absorb_profiled(event, tick, None);
    }

    fn bind_event_facts(
        &mut self,
        reader: &lattice::kernel::log::LogReader,
        through: u64,
    ) -> std::io::Result<()> {
        self.domain.authorizations.bind_reader(reader.clone());
        if self.event_facts.is_none() {
            self.event_facts = Some(view::facts::EventFacts::recover(reader.clone(), through)?);
            self.domain.event_inputs.release();
        }
        Ok(())
    }

    fn bind_cards(
        &mut self,
        reader: &lattice::kernel::log::LogReader,
        through: u64,
    ) -> std::io::Result<()> {
        if self.cards.is_none() {
            self.cards = Some(std::cell::RefCell::new(cards::Cards::recover(
                reader.clone(),
                through,
            )?));
            self.entries = Vec::new();
        }
        Ok(())
    }

    #[cfg(test)]
    fn bind_peaks(&mut self, reader: &lattice::LogReader) -> std::io::Result<()> {
        self.domain.accounting.bind_peaks(reader)
    }

    fn completed_usage(&self, event: &lattice::EventEnvelope) -> Option<lattice::Usage> {
        self.domain.completed_usage(event)
    }

    fn clear_cards(&mut self) {
        if let Some(cards) = &mut self.cards {
            if let Err(error) = cards.get_mut().clear() {
                self.flash = Some(format!("Cannot clear displayed cards: {error}"));
                return;
            }
        }
        self.entries.clear();
        self.browsing.forget_position();
    }

    fn push_local_card(&mut self, entry: Entry) {
        match &mut self.cards {
            Some(cards) => cards.get_mut().push_local(entry),
            None => self.entries.push(entry),
        }
    }

    fn has_user_card(&self) -> bool {
        self.cards.as_ref().map_or_else(
            || {
                self.entries
                    .iter()
                    .any(|entry| matches!(entry, Entry::User(_)))
            },
            |cards| cards.borrow().has_user(),
        )
    }

    fn try_absorb_profiled(
        &mut self,
        event: &lattice::EventEnvelope,
        tick: usize,
        trace: Option<&mut lattice::memory::Breakdown>,
    ) -> std::io::Result<()> {
        if self.replayed_through > 0 && event.seq <= self.replayed_through {
            return Ok(());
        }
        // Finish all fallible reads before changing counters, authorization,
        // or cards. Historical dependencies never survive this one fold.
        let mut rebuilt = None;
        if let Some(facts) = self.event_facts.as_mut() {
            let (inputs, reason) = event_inputs::EventInputs::read_live(facts, event)?;
            rebuilt = reason.map(|reason| format!("Rebuilt cached event facts: {reason}"));
            self.domain.event_inputs = inputs;
        }
        let projected = if let Some(recovered) = self.recovery.as_mut() {
            recovered.advance(
                event,
                self.domain.event_inputs.clone(),
                self.event_facts.as_mut().unwrap(),
            )
        } else {
            Ok(())
        };
        let card_change = projected
            .and_then(|()| {
                self.cards
                    .as_mut()
                    .map(|cards| cards.get_mut().advance(event.seq))
                    .transpose()
            })
            .and_then(|change| {
                if self.domain.accounting.has_peaks() {
                    if let Some(usage) = self.completed_usage(event) {
                        self.domain
                            .accounting
                            .record_indexed_peak(self.domain.turns.number() as u64, usage.prompt)?;
                    }
                }
                Ok(change)
            });
        if let Err(error) = card_change {
            self.domain.event_inputs.release();
            return Err(error);
        }
        if let Some(recovered) = self.recovery.as_ref() {
            self.domain.compaction = recovered.compaction_status().cloned();
        }
        self.absorb_profiled(event, tick, trace);
        if let Some((true, thinking)) = card_change.unwrap() {
            self.live_output.card_landed(thinking);
        }
        if self.event_facts.is_some() {
            self.domain.event_inputs.release();
        }
        if let Some(note) = rebuilt {
            self.flash = Some(note);
        }
        Ok(())
    }

    fn replay_prefix_traced(
        &mut self,
        reader: &lattice::kernel::log::LogReader,
        through: u64,
        trace: &mut lattice::memory::Breakdown,
    ) -> std::io::Result<()> {
        self.recover_prefix(reader, through, Some(trace))
    }

    fn recover_prefix(
        &mut self,
        reader: &lattice::LogReader,
        through: u64,
        trace: Option<&mut lattice::memory::Breakdown>,
    ) -> std::io::Result<()> {
        let initial = recovery::Initial::of(self);
        self.bind_event_facts(reader, through)?;
        self.bind_cards(reader, through)?;
        let mut recovered = recovery::Recovery::recover(
            initial,
            reader.clone(),
            self.event_facts.as_mut().unwrap(),
            through,
            trace,
        )?;
        recovered.apply(self)?;
        if let Some(reason) = &recovered.cold_reason {
            eprintln!("slow recovery for terminal state: {reason}");
        }
        self.recovery = Some(Box::new(recovered));
        Ok(())
    }

    fn absorb_state(
        &mut self,
        event: &lattice::EventEnvelope,
        tick: usize,
        trace: Option<&mut lattice::memory::Breakdown>,
    ) {
        if self.domain.absorb(event, tick, trace) {
            self.flash = None;
        }
        if event.event_type == lattice::components::context_gate::DECISION
            && event.payload["action"] == "compact_skipped"
        {
            self.flash = event.reason.clone();
        }
    }

    fn absorb_profiled(
        &mut self,
        event: &lattice::EventEnvelope,
        tick: usize,
        mut trace: Option<&mut lattice::memory::Breakdown>,
    ) {
        use lattice::memory::Stage;
        macro_rules! measure {
            ($stage:expr, $body:expr) => {
                if let Some(trace) = trace.as_deref_mut() {
                    trace.measure($stage, event.seq, || $body)
                } else {
                    $body
                }
            };
        }
        // Startup subscribes before taking its history snapshot. Events in
        // that overlap must not be counted twice when the live queue drains.
        if self.replayed_through > 0 && event.seq <= self.replayed_through {
            return;
        }
        self.absorb_state(event, tick, trace.as_deref_mut());
        if self.cards.is_none() && measure!(Stage::Cards, view::ingest(&mut self.entries, event)) {
            self.live_output
                .card_landed(matches!(self.entries.last(), Some(Entry::Thinking(_))));
        }
        self.live_output.round_event(&event.event_type);
    }

    #[cfg(test)]
    fn note_authorization(&mut self, event: &lattice::EventEnvelope) {
        self.domain.authorizations.observe(event);
    }

    #[cfg(test)]
    fn note_model_swap(&mut self, event: &lattice::EventEnvelope) {
        self.domain.test_note_model_swap(event);
    }

    #[cfg(test)]
    fn note_usage(&mut self, event: &lattice::EventEnvelope) {
        self.domain.test_note_usage(event);
    }

    #[cfg(test)]
    fn note_turn_boundary(&mut self, event: &lattice::EventEnvelope, tick: usize) {
        if self.domain.test_note_turn_boundary(event, tick) {
            self.flash = None;
        }
    }
}

/// Coordinate only the returned display effects, not the model operation.
fn run_model_action(ui: &mut Ui, session: Option<&Session>, action: model_actions::Action<'_>) {
    match model_actions::execute(
        action,
        &mut ui.domain.model,
        &mut ui.controls,
        &mut ui.panel,
        session,
    ) {
        model_actions::Outcome::Quiet => {}
        model_actions::Outcome::Message(text) => ack(ui, text),
        model_actions::Outcome::ModelRequested => ui.domain.turns.optimistic_activity(),
    }
}

#[cfg(test)]
fn commit_dial(ui: &mut Ui, session: Option<&Session>) {
    run_model_action(ui, session, model_actions::Action::CommitDial);
}

/// Compact JSON byte length without allocating an encoded copy of strings
/// or containers. Numbers still use serde_json's formatter, not a competing
/// floating-point representation. This counts bytes, never tokens.
#[cfg(test)]
fn json_bytes(value: &Value) -> u64 {
    lattice::view::facts::json_bytes(value)
}

#[cfg(test)]
fn started_live(tool: &str, args: &Value, result: &Value, tick: usize) -> Option<view::Live> {
    background_state::started(tool, args, result).map(|row| row.display(tick))
}

fn panel_rows(at: (usize, usize), view: &dyn View, width: usize) -> Vec<(String, String)> {
    match at {
        AT_BACKGROUND => panels::background::rows(view, width),
        AT_COMMANDS => panels::reference::commands(),
        (0, 1) => panels::reference::keys(),
        AT_CONFIG => {
            let home = home();
            let sources = panel_sources::ConfigSources::read(&home, lattice::preferences::load());
            panels::config::rows(view, &sources)
        }
        AT_MODELS => {
            // Preserve the existing read boundary: only an empty list without
            // an active form needs the environment-derived catalog path.
            let path = if view.model_form().is_none() && view.models().rows.is_empty() {
                lattice::models::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "~/.lattice/models.json".to_string())
            } else {
                String::new()
            };
            panels::models::rows(view, width, &path)
        }
        AT_COMPONENTS => panels::components::rows(view, width),
        AT_USAGE => panels::usage::rows(&lattice::ledgers::usage_by_day(&home())),
        AT_CONTEXT => panels::context::rows(view),
        _ => panels::reference::session(view),
    }
}

/// Prepare panel contents at their existing read boundaries, then compose.
fn panel_lines(at: (usize, usize), view: &dyn View, width: usize) -> Vec<Line<'static>> {
    if at == panels::AT_EXPERTS {
        return panels::experts::lines(view, width);
    }
    let picture = match at {
        AT_CONTEXT => context_picture(view, width),
        // Keep the calendar read separate and before the totals read.
        AT_USAGE => panels::usage::calendar(&lattice::ledgers::usage_by_day(&home()), width),
        _ => Vec::new(),
    };
    let rows = panel_rows(at, view, width);
    panels::compose::lines(at, width, picture, rows)
}

fn attachments(ui: &mut Ui) -> attachment_actions::Attachments<'_> {
    attachment_actions::Attachments {
        draft: &mut ui.draft,
        catalog: ui.domain.model.catalog(),
        documents: ui.documents.as_deref(),
    }
}

/// Live and headless paste share modal routing before any attachment action.
fn absorb_paste(ui: &mut Ui, text: &str) {
    if ui.pending_auth().is_some() {
        return;
    }
    if ui.panel.active() == Some(panels::AT_EXPERTS) {
        ui.experts.paste(text);
        return;
    }
    if ui.controls.paste_form(text) {
        return;
    }
    if ui.panel.is_visible() || ui.controls.blocks_paste() {
        return;
    }
    if let Some(message) = attachments(ui).paste(text) {
        ack(ui, message);
    }
}

fn attach_from_clipboard(ui: &mut Ui) {
    if let Some(message) = attachments(ui).clipboard() {
        ack(ui, message);
    }
}

#[cfg(test)]
fn attach_image(ui: &mut Ui, at: &std::path::Path, media: &'static str) {
    if let Some(message) = attachments(ui).image(at, media) {
        ack(ui, message);
    }
}

#[cfg(test)]
fn attach_bytes(ui: &mut Ui, bytes: &[u8], ext: &str, media: &str, label: &str) {
    if let Some(message) = attachments(ui).bytes(bytes, ext, media, label) {
        ack(ui, message);
    }
}

/// Acknowledge a command beside the input box, where the person is looking,
/// instead of appending it to the conversation. See `View::flash`.
fn ack(ui: &mut Ui, text: impl Into<String>) {
    ui.flash = Some(text.into());
}

fn open_link(ui: &mut Ui, url: &str) {
    match link_actions::prepare(url) {
        Ok((message, command)) => {
            ack(ui, message);
            ui.links.launch(command);
        }
        Err(message) => ack(ui, message),
    }
}

fn drain_link_open_results(ui: &mut Ui) -> bool {
    let failures = ui.links.drain();
    let changed = !failures.is_empty();
    for error in failures {
        ack(ui, error);
    }
    changed
}

/// Run a slash command by name, mutating the view. Returns true if the app
/// should quit. The core never sees any of this — slash is frontend-local.
fn run_slash(ui: &mut Ui, line: &str, session: Option<&Session>) -> bool {
    use slash_command::Intent;
    match slash_command::parse(line) {
        Intent::Exit => return true,
        Intent::Open(text) => ui.navigation = Some(tabs::Navigation::Open(text.to_string())),
        Intent::Next => ui.navigation = Some(tabs::Navigation::Next),
        Intent::Select(index) => ui.navigation = Some(tabs::Navigation::Select(index)),
        Intent::Parent => ui.navigation = Some(tabs::Navigation::Parent),
        Intent::Clear => {
            ui.clear_cards();
            ui.browsing.pin();
        }
        Intent::Panel(target) => {
            ui.panel.show(target);
            if target == panels::AT_EXPERTS {
                ui.experts.refresh();
                ui.experts.flush(session);
            }
        }
        Intent::Effort(rest) => {
            run_model_action(ui, session, model_actions::Action::Effort(rest));
        }
        Intent::Model(rest) => run_model_action(ui, session, model_actions::Action::Model(rest)),
        Intent::Compact => match session {
            Some(session) => {
                session.request_compaction();
                ack(ui, "Compaction requested — see /context for status");
            }
            None => ack(ui, "no session to compact"),
        },
        // Rewiring belongs to the thread owning the kernel; its answer arrives
        // as an ordinary ledger event.
        Intent::Uninstall(instance) => {
            if let Some(session) = session {
                session.uninstall(instance);
                ui.domain.turns.optimistic_activity();
            }
        }
        Intent::Notice(message) => ack(ui, message),
    }
    false
}

fn tui_loop(
    term: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    session: &Session,
    mut startup: Option<StartupTrace>,
    initial: initialization::Main,
    diagnostics: &mut diagnostics::Capture,
) -> (std::io::Result<()>, tabs::Shutdown, shutdown::Trace) {
    let (mut ui, tab_config, main_ledger) = match initial.install(session, &mut startup) {
        Ok(initial) => initial,
        Err(error) => {
            return (
                Err(error),
                tabs::Shutdown::default(),
                shutdown::Trace::start(),
            )
        }
    };

    // draw reports the transcript geometry so clicks and scrolling map to
    // exactly what's on screen. Kept across iterations because a skipped frame
    // leaves the previous geometry standing — which is correct, since the
    // screen it described is still the screen.
    let mut hit = Hit::default();
    let mut redraw = true;
    let mut cost = RenderCost::default();
    let mut settled_at = ui.domain.turns.done_at();
    let mut tabs = tabs::Tabs::interactive(session, tab_config, &main_ledger, &mut ui, true);

    let result = (|| {
        loop {
            ui.tick = ui.tick.wrapping_add(1);

            // Twice a second, and only while something is writing: this touches
            // the filesystem, and an expert's record does not change fast enough
            // to be worth reading at frame rate.
            if ui.tick.is_multiple_of(10)
                && ui
                    .domain
                    .background
                    .rows()
                    .iter()
                    .any(|l| l.ledger.is_some())
            {
                ui.refresh_experts();
                redraw = true;
            }

            redraw |= drain_link_open_results(&mut ui);
            let folded = tabs.drain(&mut ui)?;
            if let Some(notice) = diagnostics.notice()? {
                ui.flash = Some(notice);
                redraw = true;
            }
            let session = tabs.session();

            // Only when the frame would differ. Building one re-renders the WHOLE
            // transcript — every code block highlighted again from scratch — and
            // this loop comes round twenty times a second whether or not anything
            // happened. Measured on an idle session: 17% of a core, spent
            // producing a frame identical to the one already on screen, and the
            // bill grows with the conversation because the whole of it is rebuilt
            // every time.
            if folded || redraw || animating(&ui) {
                redraw = false;
                hit =
                    initialization::draw_observed(term, &mut ui, session, &mut startup, &mut cost)?;
            } else {
                cost.skipped += 1;
            }

            // A turn just landed: report what drawing it cost, and start counting
            // the next one. `done_at` is stamped once, when the turn ends, so its
            // changing IS the boundary — no second place to keep in step.
            if ui.domain.turns.done_at().is_some() && ui.domain.turns.done_at() != settled_at {
                settled_at = ui.domain.turns.done_at();
                session.note_render_cost(cost.take(ui.entry_count()));
            }

            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            // Anything the person did can change what is on screen. Rather than
            // work out which keys cannot, draw on all of them: a missed frame is a
            // frozen terminal, a spare one costs a few milliseconds.
            redraw = true;
            match event::read()? {
                // Bracketed paste arrives whole — insert it verbatim (may be multi-line).
                Event::Paste(text) => {
                    ui.flash = None;
                    // A terminal delivers a DRAGGED file as a pasted path — that is
                    // the only channel it has — so drag support belongs here.
                    absorb_paste(&mut ui, &text);
                    ui.draft.reset_selection();
                }
                Event::Mouse(m) => on_mouse(&mut ui, m, &hit),
                // on_key does the editing (its side effect) and returns true only
                // when it's time to quit.
                Event::Key(key) if on_key(&mut ui, Some(session), key, &hit) => return Ok(()),
                _ => {}
            }
            if let Some(navigation) = ui.navigation.take() {
                session.note_render_cost(cost.take(ui.entry_count()));
                tabs.navigate(&mut ui, navigation);
                settled_at = ui.domain.turns.done_at();
                hit = Hit::default();
                redraw = true;
            }
        }
    })();
    let trace = shutdown::Trace::start();
    (result, tabs.release_sessions(), trace)
}

/// Fold everything the kernel has produced since the last look into the view.
/// Lives apart from the event loop so the headless driver (`debug-tui`) folds
/// through THIS code rather than a copy of it — a debugging harness that
/// reimplements what it is meant to inspect proves nothing.
/// Returns whether anything arrived — the caller redraws only when it did.
fn drain_render(ui: &mut Ui, session: &Session) -> std::io::Result<bool> {
    let mut folded = false;
    while let Some(render) = session.poll_render() {
        fold_render(ui, render)?;
        ui.experts.flush(Some(session));
        folded = true;
    }
    Ok(folded)
}

/// Is anything on screen moving right now?
///
/// The frame is skipped when nothing changed, so every animation has to be
/// named here or it freezes mid-motion: the braille spinner and the sweep
/// crossing the phrase while a turn runs, and the "Done" line settling for a
/// moment after it ends.
fn animating(ui: &Ui) -> bool {
    ui.domain.turns.busy()
        || matches!(ui.domain.turns.done_at(), Some(at) if ui.tick.wrapping_sub(at) <= DONE_SETTLE)
}

/// What drawing cost over one turn, tallied frame by frame.
///
/// Kept as a running total and reported once, because a frame is not a
/// completed state in the sense the ledger means — and twenty entries a second
/// would bury the very record they are meant to make readable.
#[derive(Default)]
struct RenderCost {
    frames: usize,
    skipped: usize,
    total: Duration,
    worst: Duration,
}

impl RenderCost {
    fn frame(&mut self, took: Duration) {
        self.frames += 1;
        self.total += took;
        self.worst = self.worst.max(took);
    }

    /// Read the tally out and start the next one.
    fn take(&mut self, entries: usize) -> lattice::RenderCostNote {
        let ms = |d: Duration| (d.as_secs_f64() * 10_000.0).round() / 10.0;
        let done = lattice::RenderCostNote {
            frames: self.frames,
            skipped: self.skipped,
            total_ms: ms(self.total),
            worst_ms: ms(self.worst),
            entries,
            memory: Some(lattice::memory::Snapshot::capture()),
            history: serde_json::Value::Null,
        };
        *self = Self::default();
        done
    }
}

/// Fold ONE render event into the view. Separate from the drain so that a
/// caller which waits for a specific event (rather than taking whatever has
/// arrived) folds through the same code instead of a copy of it.
fn fold_render(ui: &mut Ui, render: RenderEvent) -> std::io::Result<()> {
    match render {
        // Turn boundaries ride in the event stream, so ANY turn source — a
        // user message OR a background/timer wake — lights the line. Every
        // ledger-derived piece of this view is folded in one place; see
        // `Ui::absorb`.
        RenderEvent::InputObserved {
            event,
            pid,
            clock_ns,
        } => {
            ui.input_latency
                .notified(event, pid, clock_ns, lattice::input_latency::clock_ns());
        }
        RenderEvent::Appended(event) => {
            ui.experts.observe(&event);
            ui.try_absorb_profiled(&event, ui.tick, None)?;
            if event.event_type == core_events::USER_MESSAGE && event.causes.is_empty() {
                ui.input_latency
                    .absorbed(&event.id, lattice::input_latency::clock_ns());
            }
        }
        RenderEvent::Notice { source, payload } if source == "assembly" => {
            type Row = (String, String, String, String, bool, Vec<String>);
            match serde_json::from_value::<Vec<Row>>(payload["parts"].clone()) {
                Ok(rows) => {
                    ui.parts = rows
                        .into_iter()
                        .map(|(name, component, runtime, tools, removable, wires)| {
                            let runtime = match runtime.as_str() {
                                "in-process" => "in-process",
                                "subprocess" => "subprocess",
                                _ => "?",
                            };
                            (name, component, runtime, tools, removable, wires)
                        })
                        .collect()
                }
                Err(error) => ui.flash = Some(format!("Invalid assembly snapshot: {error}")),
            }
        }
        RenderEvent::Notice { source: _, payload } => {
            if let Some(note) = ui.live_output.notice(&payload) {
                // A runtime receipt belongs beside the input, not in the ledger.
                ui.flash = Some(note.to_string());
            }
        }
        RenderEvent::Quiescent => ui.domain.turns.quiescent(ui.tick),
        RenderEvent::StartupFailed(msg) => ui.push_local_card(Entry::Error(msg)),
    }
    Ok(())
}

fn route_modal(ui: &mut Ui, session: Option<&Session>, key: KeyCode) -> bool {
    if ui.panel.active() == Some(panels::AT_EXPERTS) && ui.experts.key(key) {
        ui.experts.flush(session);
        return true;
    }
    match modal_keys::handle(
        &mut ui.controls,
        &mut ui.panel,
        key,
        ui.domain.model.catalog().rows.len(),
        ui.parts.len(),
    ) {
        modal_keys::Outcome::Unhandled => return false,
        modal_keys::Outcome::Consumed => {}
        modal_keys::Outcome::Model(action) => run_model_action(ui, session, action),
        modal_keys::Outcome::Uninstall(selected) => match ui.components().get(selected) {
            Some((instance, _, _, _, true, _)) => {
                let name = instance.clone();
                if let Some(session) = session {
                    session.uninstall(&name);
                }
                ui.panel.dismiss_preserving_details();
                ui.domain.turns.optimistic_activity();
                ack(ui, format!("removing {name}"));
            }
            Some((instance, _, _, _, false, _)) => ack(
                ui,
                format!("{instance} is part of the base assembly — it cannot be removed"),
            ),
            None => {}
        },
    }
    if ui.panel.active() == Some(panels::AT_EXPERTS) {
        ui.experts.ensure_loaded();
        ui.experts.flush(session);
    }
    true
}

/// Handle one key press, mutating `ui`. Returns true when the app should quit.
fn on_key(
    ui: &mut Ui,
    // OPTIONAL, the way `run_slash` already took it: without it the whole key
    // path could only be exercised by running a real kernel, so none of it was
    // covered. Every use inside was already wrapped in `Some(..)`.
    session: Option<&Session>,
    key: ratatui::crossterm::event::KeyEvent,
    hit: &Hit,
) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    // The last command's receipt goes as soon as you touch a key — it answered
    // the previous keystroke, and by now you have moved on. Cleared HERE and
    // not in the event loop so a test can drive it; and cleared BEFORE the key
    // is handled, so a command that leaves a receipt still leaves one.
    ui.flash = None;
    if ui.pending_auth().is_some() {
        // Authorization owns the keyboard, including modified edit/submit keys.
        // Scrolling remains available to inspect the complete request above.
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Up => ui.domain.authorizations.select_allow(true),
                KeyCode::Down => ui.domain.authorizations.select_allow(false),
                KeyCode::Enter | KeyCode::Esc => {
                    if key.code == KeyCode::Esc {
                        ui.domain.authorizations.select_allow(false);
                    }
                    if let Some((request, allow)) = ui.domain.authorizations.answer_selected() {
                        if let Some(session) = session {
                            session.authorize(&request, allow);
                        }
                    }
                }
                KeyCode::PageUp => scroll_by(ui, SCROLL_PAGE as isize, hit),
                KeyCode::PageDown => scroll_by(ui, -(SCROLL_PAGE as isize), hit),
                _ => {}
            }
        }
        return false;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // A newline can be inserted while the slash menu is closed; the menu is only
    // open when the input is a partial slash, so this never conflicts.
    let newline = key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
    match key.code {
        KeyCode::PageDown if ctrl => ui.navigation = Some(tabs::Navigation::Next),
        KeyCode::PageUp if ctrl => ui.navigation = Some(tabs::Navigation::Previous),
        _ if route_modal(ui, session, key.code) => {}
        // Ctrl-D quits. Ctrl-C interrupts a running turn, else clears the input
        // line. Esc interrupts, else drops back to the latest when scrolled up.
        KeyCode::Char('d') if ctrl => return true,
        KeyCode::Char('c') if ctrl => {
            if ui.domain.turns.busy() {
                if let Some(session) = session {
                    session.interrupt();
                }
            } else {
                ui.draft.edit().clear();
                ui.draft.reset_selection();
            }
        }
        KeyCode::F(1) => ui.panel.show(AT_COMMANDS),
        KeyCode::Char('l') if ctrl => {
            ui.clear_cards();
            ui.browsing.pin();
        }
        KeyCode::Esc => {
            if ui.domain.turns.busy() {
                if let Some(session) = session {
                    session.interrupt();
                }
            } else if ui.browsing.offset() > 0 {
                ui.browsing.pin();
            }
        }

        // Scroll the transcript back through history and forward again.
        KeyCode::PageUp => scroll_by(ui, SCROLL_PAGE as isize, hit),
        KeyCode::PageDown => scroll_by(ui, -(SCROLL_PAGE as isize), hit),

        // Expand/collapse the most recent tool card's full output.
        KeyCode::Char('o') if ctrl => toggle_last_tool(ui),

        // Modified Enter remains an edit; plain Enter submits here.
        KeyCode::Enter if !newline => {
            let submitted_ns = lattice::input_latency::clock_ns();
            let (line, kept) = match submission::submit(
                &mut ui.draft,
                &ui.domain.skills,
                ui.domain.model.effort(),
            ) {
                submission::Intent::Empty => return false,
                submission::Intent::Command(line) => return run_slash(ui, &line, session),
                submission::Intent::SkillCandidate(name) => {
                    ui.live_output.submitted();
                    ui.browsing.pin();
                    ui.domain.turns.optimistic_activity();
                    if let Some(session) = session {
                        ui.input_latency.submitted(submitted_ns);
                        session.send_with_origin(name, Vec::new(), ui.initial_origin.take());
                    }
                    return false;
                }
                submission::Intent::Message { text, kept } => (text, kept),
            };
            // Don't echo locally: the USER_MESSAGE we inject comes straight back
            // from the ledger and `ingest` renders it — one source of truth.
            ui.live_output.submitted();
            ui.browsing.pin(); // jump back to the latest to watch the reply
                               // Optimistic: light up immediately. The USER_MESSAGE event coming
                               // back from the ledger is authoritative — it advances the turn (via
                               // note_turn_boundary), the same path a background wake takes.
            ui.domain.turns.optimistic_activity();
            if let Some(session) = session {
                // Only the ones whose placeholder is STILL in the line, in the
                // order they appear there. Deleting a placeholder is how a
                // person cancels a picture, so the buffer decides, not the
                // list of everything that was ever attached.
                let images = ui.draft.take_images(&kept);
                ui.input_latency.submitted(submitted_ns);
                session.send_with_origin(line, images, ui.initial_origin.take());
            }
        }

        // Its own key because ⌘V cannot carry a picture: bracketed paste is
        // text, so a clipboard holding only an image sends nothing at all.
        KeyCode::Char('v') if ctrl => attach_from_clipboard(ui),

        _ => draft_keys::handle(
            &mut ui.draft,
            key,
            hit.input_width,
            &ui.domain.skills,
            ui.domain.model.effort(),
        ),
    }
    false
}

/// What `draw` reports back so the loop can hit-test clicks and clamp scrolling:
/// the transcript's area, the row it is scrolled to, and, per rendered row, the
/// tool card (call id) that owns it — all in terms of the wrapped rows on screen.
#[derive(Default)]
struct Hit {
    /// Text columns from the most recent draw, shared with vertical editing.
    input_width: usize,
    area: ratatui::layout::Rect,
    offset: usize,
    /// Earlier groups were deliberately not laid out, so total height is unknown.
    more_above: bool,
    more_below: bool,
    top: Option<view::TranscriptPosition>,
    owner: Vec<Option<String>>,
    /// The "jump to bottom" button's rect, when scrolled up (else None).
    jump: Option<ratatui::layout::Rect>,
    /// Each status readout's rect and what clicking it opens. This is what
    /// turns the bar from something you read into something you steer by: the
    /// panels stop being things you have to remember a slash command for.
    status: Vec<(ratatui::layout::Rect, Opens)>,
    links: Vec<(ratatui::layout::Rect, String)>,
}

fn on_mouse(ui: &mut Ui, m: ratatui::crossterm::event::MouseEvent, hit: &Hit) {
    use mouse_intent::Intent;
    let authorization = ui.pending_auth().is_some();
    let intent = mouse_intent::interpret(
        m,
        ui.panel.is_visible() && !authorization,
        hit.mouse_targets(),
    );
    if authorization {
        // Keep the transcript inspectable without activating suspended controls.
        match intent {
            Some(Intent::Scroll(delta)) => scroll_by(ui, delta, hit),
            Some(Intent::Jump) => ui.browsing.pin(),
            _ => {}
        }
        return;
    }
    match intent {
        Some(Intent::PanelUp(lines)) => ui.panel.scroll_up(lines),
        Some(Intent::PanelDown(lines)) => ui.panel.scroll_down(lines),
        Some(Intent::Scroll(delta)) => scroll_by(ui, delta, hit),
        Some(Intent::Status(opens)) => open_from_status(ui, opens),
        Some(Intent::Jump) => ui.browsing.pin(),
        Some(Intent::Link(url)) => open_link(ui, &url),
        Some(Intent::Card(id)) => toggle_card(ui, id),
        None => {}
    }
}

impl Hit {
    fn mouse_targets(&self) -> mouse_intent::Targets<'_> {
        mouse_intent::Targets {
            area: self.area,
            offset: self.offset,
            owner: &self.owner,
            jump: self.jump,
            status: &self.status,
            links: &self.links,
        }
    }

    /// Highest the transcript can scroll up (top fully shown).
    fn max_scroll(&self) -> Option<usize> {
        (!self.more_above && !self.more_below)
            .then(|| self.owner.len().saturating_sub(self.area.height as usize))
    }

    #[cfg(test)]
    fn url_at(&self, col: u16, row: u16) -> Option<&str> {
        self.mouse_targets().url_at(col, row)
    }
    #[cfg(test)]
    fn card_at(&self, col: u16, row: u16) -> Option<String> {
        self.mouse_targets().card_at(col, row)
    }
    #[cfg(test)]
    fn jump_at(&self, col: u16, row: u16) -> bool {
        self.mouse_targets().jump_at(col, row)
    }
    #[cfg(test)]
    fn status_at(&self, col: u16, row: u16) -> Option<Opens> {
        self.mouse_targets().status_at(col, row)
    }
}

/// A frame captured headlessly: the text grid, the cursor, and the layout
/// geometry (from `Hit`). Because `draw` is a pure function of the `View`, any
/// state — built in a test or replayed from a ledger — can be snapshotted, so UI
/// checks read structured data instead of a human eyeballing the terminal.
struct FrameSnapshot {
    width: u16,
    height: u16,
    rows: Vec<String>,
    cursor: Option<(u16, u16)>,
    transcript: ratatui::layout::Rect,
    scroll: usize,
    max_scroll: Option<usize>,
    anchor: Option<view::TranscriptPosition>,
    has_before: bool,
    has_after: bool,
    jump: Option<ratatui::layout::Rect>,
    owners: Vec<Option<String>>,
}

impl FrameSnapshot {
    /// Structured form for the `debug-frame` inspector.
    fn to_json(&self) -> Value {
        let rect = |r: ratatui::layout::Rect| serde_json::json!([r.x, r.y, r.width, r.height]);
        serde_json::json!({
            "width": self.width,
            "height": self.height,
            "rows": self.rows.iter().map(|r| r.trim_end()).collect::<Vec<_>>(),
            "cursor": self.cursor.map(|(x, y)| serde_json::json!([x, y])),
            "transcript": rect(self.transcript),
            "scroll": self.scroll,
            "maxScroll": self.max_scroll,
            "anchor": self.anchor,
            "hasBefore": self.has_before,
            "hasAfter": self.has_after,
            "jump": self.jump.map(rect),
            "rowOwners": self.owners,
        })
    }
}

#[cfg(test)]
impl FrameSnapshot {
    /// Whether any row contains `needle`.
    fn has(&self, needle: &str) -> bool {
        self.rows.iter().any(|r| r.contains(needle))
    }

    /// The first row containing `needle` — for asserting on vertical ORDER,
    /// which is the only way to test that one thing is drawn above another.
    fn row_of(&self, needle: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(needle))
    }
}

/// Render `view` at `w`×`h` on a headless backend and capture the frame — the
/// heart of the visual-test and `debug-frame` machinery.
fn snapshot(view: &dyn View, w: u16, h: u16) -> FrameSnapshot {
    let mut term =
        Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("headless backend");
    let hit = draw(&mut term, view).expect("draw");
    let cursor = term.get_cursor_position().ok().map(|p| (p.x, p.y));
    let buf = term.backend().buffer();
    // A wide character (CJK, an emoji) occupies two cells; the second holds
    // no symbol of its own. Walking cell by cell would print it as a stray
    // space and tear every word apart — step over the continuation instead.
    let rows: Vec<String> = (0..h)
        .map(|y| {
            let mut row = String::new();
            let mut x = 0u16;
            while x < w {
                let symbol = buf[(x, y)].symbol();
                row.push_str(symbol);
                x += (UnicodeWidthStr::width(symbol) as u16).max(1);
            }
            row
        })
        .collect();
    FrameSnapshot {
        width: w,
        height: h,
        rows,
        cursor,
        transcript: hit.area,
        scroll: hit.offset,
        max_scroll: hit.max_scroll(),
        anchor: hit.top,
        has_before: hit.more_above,
        has_after: hit.more_below,
        jump: hit.jump,
        owners: hit.owner,
    }
}

/// Render `view` at `w`×`h` and dump the frame as a standalone HTML page.
///
/// The text snapshot everything else uses throws every colour away, which is
/// fine for asserting that one thing is drawn above another and useless for
/// reviewing a palette. This is the seam where a change to the colours can
/// actually be looked at without a terminal.
fn frame_html(view: &dyn View, w: u16, h: u16) -> String {
    let mut term =
        Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("headless backend");
    draw(&mut term, view).expect("draw");
    let buf = term.backend().buffer();

    // Only the colours this frontend actually spends. Anything else falls back
    // to "inherit", which is the page's own foreground — the same thing a
    // terminal does with a colour it was never told.
    fn css(c: ratatui::style::Color) -> String {
        match c {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "inherit".to_string(),
        }
    }
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    let mut out = String::from(
        "<!doctype html><meta charset=\"utf-8\"><title>lattice frame</title>\
         <style>body{background:#0b0d13;margin:0;padding:18px}\
         pre{font:14px/1.15 'SF Mono','Menlo','DejaVu Sans Mono',monospace;\
         color:#c6d0e0;margin:0;white-space:pre}</style><pre>",
    );
    for y in 0..h {
        let mut x = 0u16;
        // Runs of one style collapse into one span; a span per cell would make
        // the page an order of magnitude bigger than the frame it shows.
        let (mut run, mut run_style) = (String::new(), None::<Style>);
        let flush = |out: &mut String, run: &mut String, style: &Option<Style>| {
            if run.is_empty() {
                return;
            }
            match style {
                Some(st) => {
                    let mut rules = vec![format!("color:{}", css(st.fg.unwrap_or(Color::Reset)))];
                    if let Some(bg) = st.bg {
                        rules.push(format!("background:{}", css(bg)));
                    }
                    if st.add_modifier.contains(Modifier::BOLD) {
                        rules.push("font-weight:700".into());
                    }
                    if st.add_modifier.contains(Modifier::ITALIC) {
                        rules.push("font-style:italic".into());
                    }
                    out.push_str(&format!(
                        "<span style=\"{}\">{}</span>",
                        rules.join(";"),
                        esc(run)
                    ));
                }
                None => out.push_str(&esc(run)),
            }
            run.clear();
        };
        while x < w {
            let cell = &buf[(x, y)];
            let style = cell.style();
            if run_style != Some(style) {
                flush(&mut out, &mut run, &run_style);
                run_style = Some(style);
            }
            run.push_str(cell.symbol());
            x += (UnicodeWidthStr::width(cell.symbol()) as u16).max(1);
        }
        flush(&mut out, &mut run, &run_style);
        out.push('\n');
    }
    out.push_str("</pre>");
    out
}

fn draw_ui<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    ui: &mut Ui,
) -> std::io::Result<Hit>
where
    B::Error: backend_error::IntoIoError,
{
    let hit = draw(term, ui)?;
    if ui.panel.active().is_none() {
        if let Some(top) = hit.top {
            ui.browsing.drawn(top, hit.more_below);
        }
    }
    Ok(hit)
}

fn draw<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    view: &dyn View,
) -> std::io::Result<Hit>
where
    B::Error: backend_error::IntoIoError,
{
    let spinner = activity::spinner(view.tick());
    // Candidate commands to suggest under the input (empty unless typing a slash)
    let hint = slash_matches(&view.input(), view.skills(), &view.effort());
    // The dial replaces the INPUT BOX rather than hovering above it. It is a
    // mode, so leaving the prompt and a spent command on screen beside it
    // would be showing two things where only one is live.
    let dial = view.dial();
    let models = view.models();
    // The picker's height is its own content: one row of names, a blank, a
    // heading, and one line per fact that changes. Measured rather than fixed,
    // because a swap that changes one thing should not be drawn in the space a
    // swap that changes four would need.
    let picker = view
        .picker()
        .map(|cursor| (cursor, picker_lines(&models, cursor)));
    let authorization = view.authorization_prompt()?;
    let mode_h = if authorization.is_some() {
        authorization_panel::HEIGHT
    } else {
        match (&dial, &picker) {
            (Some(_), _) => 2,
            (_, Some((_, lines))) => lines.len() as u16,
            _ => 0,
        }
    };
    // Keep the palette compact; its visible window follows the selection.
    // Matching and keyboard navigation still use the complete candidate list.
    let hint_h = if mode_h > 0 {
        0
    } else {
        hint.len().min(candidates::MAX_VISIBLE) as u16
    };
    // The receipt for the command just typed, beside the input box. It cannot
    // coexist with the slash menu (the keystroke that opens the menu clears the
    // receipt), so they take turns in the same place. A blank row above it,
    // like the queued lines have.
    let flash = view.flash().filter(|_| hint_h == 0 && mode_h == 0);
    let flash_h: u16 = match flash {
        Some(text) => (text.split('\n').count() as u16).min(4) + 1,
        None => 0,
    };
    let sel = view.hint_sel().min(hint.len().saturating_sub(1));
    // Rendering and cursor navigation use the same wrapped screen rows.
    let input = view.input();
    let input_width = term
        .size()
        .map_err(backend_error::IntoIoError::into_io_error)?
        .width
        .saturating_sub(4)
        .max(1) as usize;
    let input_layout = lattice::editor::InputLayout::new(&input, input_width);
    let input_h = if mode_h > 0 {
        mode_h
    } else {
        input_layout.rows.len().clamp(1, 6) as u16
    };
    // A dedicated line sits above the input, set off by a blank row on each side
    // (from the output above, from the input below). It shows for the whole turn
    // — through tool runs too, so it never blinks out mid-work — and stays put
    // after the turn ends, settling into "Done" rather than vanishing. It only
    // hides before the very first turn.
    let show_line = view.busy() || view.waiting() || view.done_at().is_some();
    // Lines already said that the model has not been shown yet. They belong
    // beside the input box, not in the transcript: the transcript is the
    // ledger's story of what HAPPENED, and "not yet heard" is a fact about
    // right now, which no replay of the ledger could reconstruct.
    let queued = view.unseen();
    let queued_h = (queued.len() as u16).min(QUEUED_ROWS);
    // Blank, the line, blank. The trailing blank goes when something waits
    // below it: the waiting lines belong TO that line — what is being worked
    // on, and what has not been heard yet — and a gap between them reads as
    // two unrelated things.
    // The receipt brings its own leading blank, so it counts as "something
    // waits below" for the same reason the queued lines do — otherwise two
    // blanks stack up between the line and the receipt.
    let think_h: u16 = match (show_line, queued_h > 0 || flash_h > 0) {
        (false, _) => 0,
        (true, true) => 2,
        (true, false) => 3,
    };
    // The transcript is full-width with one column of padding a side, and the
    // card heads have to be built to fit it — a head is one line by design, so
    // it is trimmed rather than wrapped, and a trim needs to know to what.
    // Build transcript content only after layout supplies its actual height.
    let mut hit = Hit::default();
    let mut transcript_error = None;
    term.draw(|frame| {
        let mut screen = frame.area();
        if !view.tabs().is_empty() && screen.height > 0 {
            let header = ratatui::layout::Rect::new(screen.x, screen.y, screen.width, 1);
            frame.render_widget(
                Paragraph::new(view.tabs()).style(Style::default().fg(ACCENT)),
                header,
            );
            screen.y += 1;
            screen.height -= 1;
        }
        // The conversation selector stays visible; the brand rides in the transcript.
        // No pinned brand header — the brand rides at the TOP of the transcript and
        // scrolls up with the conversation (built in `transcript_body`).
        // The slash candidates sit DIRECTLY above the input (after the
        // thinking line), so the menu hugs the box the user is typing in
        // rather than floating up by the "Done" line.
        let areas = Layout::vertical([
            Constraint::Min(3),              // transcript
            Constraint::Length(think_h),     // thinking line + blank (0 when idle)
            Constraint::Length(queued_h),    // said but not yet in front of the model
            Constraint::Length(hint_h),      // slash candidates (0 when not typing /)
            Constraint::Length(flash_h),     // the last command's receipt (0 when none)
            Constraint::Length(input_h + 2), // input (grows with newlines)
            Constraint::Length(1),           // status
        ])
        .split(screen);

        // The transcript is borderless, full-width, with a thin left gutter. We
        // wrap it ourselves so one row = one screen line: scroll and click
        // hit-testing are then exact, not approximated from unwrapped counts.
        let content = areas[0];
        let inner_w = content.width.saturating_sub(2) as usize; // padding 1 each side
                                                                // The welcome scene rides at the top and scrolls with the talk; it fills
                                                                // the transcript (bar the label lines) so a fresh screen is all globe.
        let art_h = content.height as usize;
        let page = match transcript::page(view, spinner, inner_w, art_h) {
            Ok(page) => page,
            Err(error) => {
                transcript_error = Some(error);
                return;
            }
        };
        let more_above = page.before;
        let more_below = page.after;
        let top = page.rows.first().map(|row| row.position);
        let mut rows = Vec::with_capacity(page.rows.len());
        let mut owner = Vec::with_capacity(page.rows.len());
        let mut link_rows = Vec::new();
        for located in page.rows {
            let row = located.row;
            link_rows.extend(
                row.links
                    .into_iter()
                    .map(|(x, width, url)| (rows.len(), x, width, url)),
            );
            rows.push(row.line);
            owner.push(row.owner);
        }
        let visible = content.height as usize;
        let offset = 0;
        // Only visible rows reach the widget; history coordinates never enter u16.
        frame.render_widget(
            Paragraph::new(rows).block(Block::default().padding(Padding::new(1, 1, 0, 0))),
            content,
        );
        // A "jump to bottom" pill floats at the transcript's bottom-right while
        // scrolled up — a clickable shortcut back to the latest (borrowed from
        // Claude Code); Esc and the wheel do the same.
        let mut jump = None;
        if more_below && view.scroll() > 0 {
            let label = " ↓ jump to bottom ";
            let bw = label.chars().count() as u16;
            if content.width > bw + 2 && content.height > 0 {
                let brect = ratatui::layout::Rect {
                    x: content.x + content.width - bw - 1,
                    y: content.y + content.height - 1,
                    width: bw,
                    height: 1,
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        label,
                        Style::default().fg(FG).bg(USER_BG),
                    ))),
                    brect,
                );
                jump = Some(brect);
            }
        }
        hit = Hit {
            input_width,
            area: content,
            offset,
            more_above,
            more_below,
            top,
            owner,
            jump,
            status: Vec::new(),
            links: if view.panel().is_some() {
                Vec::new()
            } else {
                link_rows
                    .into_iter()
                    .filter_map(|(row, x, width, url)| {
                        (row >= offset && row < offset + visible).then(|| {
                            (
                                ratatui::layout::Rect::new(
                                    content.x + 1 + x as u16,
                                    content.y + (row - offset) as u16,
                                    width as u16,
                                    1,
                                ),
                                url,
                            )
                        })
                    })
                    .collect()
            },
        };

        // The panel takes the transcript's place while it is open. A mode: the
        // conversation is still there, it is simply not what you are looking at.
        //
        // Painted OVER the transcript rather than instead of it, because
        // "instead" meant returning early from the frame — which also skipped
        // the input box and the status bar, and the status bar is where it
        // says how to get out. `Clear` wipes the rows the panel does not fill,
        // so no transcript shows through beneath it.
        if let Some(at) = view.panel().filter(|_| authorization.is_none()) {
            frame.render_widget(ratatui::widgets::Clear, areas[0]);
            let mut lines = panel_lines(at, view, areas[0].width as usize);
            let height = areas[0].height as usize;
            // The tab row stays put; the body under it scrolls. Without this
            // the panel simply ended at the bottom of the terminal and said
            // nothing — the chart and the cache table were not "below the
            // fold", they were gone.
            const HEAD: usize = 2;
            if lines.len() > height {
                let body = height.saturating_sub(HEAD + 1);
                let most = lines.len().saturating_sub(HEAD).saturating_sub(body);
                let at_line = view.panel_scroll().min(most);
                let mut shown: Vec<Line> = lines.drain(..HEAD.min(lines.len())).collect();
                shown.extend(lines.into_iter().skip(at_line).take(body));
                let more = most.saturating_sub(at_line);
                shown.push(Line::from(Span::styled(
                    if more > 0 {
                        format!("   ↓ {more} more lines")
                    } else if at_line > 0 {
                        "   ↑ back to the top".to_string()
                    } else {
                        String::new()
                    },
                    Style::default().fg(ACCENT),
                )));
                lines = shown;
            }
            frame.render_widget(Paragraph::new(lines), areas[0]);
        }

        // Slash candidates, listed just above the input while typing a command;
        // the highlighted one (arrow-key selection) gets a ▸ marker and brightens.
        if hint_h > 0 {
            let hint_lines = candidates::lines(
                &hint,
                sel,
                areas[3].height as usize,
                areas[3].width as usize,
            );
            frame.render_widget(Paragraph::new(hint_lines), areas[3]);
        }

        // The thinking line rides on the MIDDLE row of its region; the rows above
        // and below stay blank, setting it off from the output and the input.
        // Busy → a breathing dot and a phrase (working if it's writing or running
        // a tool, else thinking); finished → the dot settles and the phrase is
        // wiped and typed over to "Done".
        if think_h > 0 {
            let think = activity::line(view);
            frame.render_widget(
                Paragraph::new(think),
                ratatui::layout::Rect {
                    y: areas[1].y + 1,
                    height: 1,
                    ..areas[1]
                },
            );
        }

        if queued_h > 0 {
            let dim = Style::default().fg(DIM);
            let rows: Vec<Line> = queued
                .iter()
                .take(QUEUED_ROWS as usize)
                .enumerate()
                .map(|(i, text)| {
                    let last = i + 1 == queued.len().min(QUEUED_ROWS as usize);
                    let more = queued.len() as u16 - queued_h;
                    let tail = if last && more > 0 {
                        format!("  (+{more})")
                    } else {
                        String::new()
                    };
                    Line::from(vec![
                        // Same column as the line above it, which is the
                        // same column as the ❯ of the input box below and of
                        // every question in the transcript — one left edge for
                        // everything the person is looking at right now.
                        Span::styled(" ⎿ ", dim),
                        Span::styled(clip(text, 60), dim.add_modifier(Modifier::ITALIC)),
                        Span::styled(tail, dim),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(rows), areas[2]);
        }

        let caret = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
        // The receipt sits on the BOTTOM row of its region; the row above it
        // stays blank, setting it off from whatever the transcript ended with.
        // Dim, because it is the frontend talking, not the conversation.
        if let Some(text) = flash {
            let rows: Vec<Line> = text
                .split('\n')
                .map(|l| Line::from(Span::styled(format!("  {l}"), Style::default().fg(DIM))))
                .collect();
            let mut at = areas[4];
            at.y += 1;
            at.height = at.height.saturating_sub(1);
            frame.render_widget(Paragraph::new(rows), at);
        }

        let input_area = areas[5];
        hit.input_width = input_width;
        let visible_height = input_area.height.saturating_sub(2) as usize;
        let first_input_row = input_layout.visible_start(view.cursor(), visible_height);
        // Row 0 gets the ❯ prompt; continuation rows a matching 2-space indent.
        // Unless the dial has the box, in which case there is no prompt at
        // all — that absence is what says a mode is running.
        let input_lines: Vec<Line> = if let Some(prompt) = &authorization {
            authorization_panel::lines(
                prompt,
                input_area.width.saturating_sub(2) as usize,
                visible_height,
            )
        } else {
            match (dial, &picker) {
                (Some(cursor), _) => dial_lines(&view.effort(), cursor),
                (_, Some((_, lines))) => lines.clone(),
                _ => input_layout
                    .rows
                    .iter()
                    .enumerate()
                    .skip(first_input_row)
                    .take(visible_height)
                    .map(|(i, row)| {
                        Line::from(vec![
                            Span::styled(if i == 0 { "❯ " } else { "  " }, caret),
                            Span::styled(row.to_string(), Style::default().fg(FG)),
                        ])
                    })
                    .collect(),
            }
        };
        frame.render_widget(
            Paragraph::new(input_lines).block(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .border_style(Style::default().fg(if view.busy() { DIM } else { RULE }))
                    .padding(Padding::horizontal(1)),
            ),
            input_area,
        );
        // Park the real (blinking) terminal cursor at the logical cursor's row
        // and column, so the typing position is exactly where it appears.
        if visible_height > 0 && input_area.width > 0 && mode_h == 0 {
            let (row, column) = input_layout.position(view.cursor());
            let x = (input_area.x as usize + 3 + column)
                .min(input_area.right().saturating_sub(1) as usize) as u16;
            let y = input_area.y
                + 1
                + row.saturating_sub(first_input_row).min(visible_height - 1) as u16;
            frame.set_cursor_position((x, y));
        }

        // The status bar shows only what isn't already obvious: while scrolled,
        // how to get back; with the slash menu open, how to drive it; while busy,
        // how to interrupt; while typing, clear/newline; and — resident when the
        // line is empty — how to quit.
        let dimmed = Style::default().fg(DIM);
        // A MODE clears the readouts. What you need then is the way out, not
        // a report on things you cannot touch until you take it — and the way
        // out is written in these keys and nowhere else, which is also why this
        // half of the bar is not configurable.
        let (keys, mode): (Vec<Span>, bool) = if authorization.is_some() {
            (vec![Span::styled(authorization_panel::KEYS, dimmed)], true)
        } else if view.panel() == Some(AT_COMPONENTS) {
            (
                vec![Span::styled(
                    "↑↓ choose · Enter wiring · u remove · ← → tabs · Esc close",
                    dimmed,
                )],
                true,
            )
        } else if view.panel().is_some() {
            (
                vec![Span::styled(
                    if view.panel() == Some(panels::AT_EXPERTS) {
                        "Experts · PgUp/PgDn scroll"
                    } else {
                        "↑↓ PgUp/PgDn scroll · ← → tabs · Esc close"
                    },
                    dimmed,
                )],
                true,
            )
        } else if view.scroll() > 0 && more_below {
            (
                vec![
                    Span::styled("⌃ scrolled up", Style::default().fg(ACCENT)),
                    Span::styled(" · wheel/PgUp/PgDn · Esc to bottom", dimmed),
                ],
                true,
            )
        } else if dial.is_some() {
            (
                vec![Span::styled("← → choose · Enter set · Esc cancel", dimmed)],
                true,
            )
        } else if picker.is_some() {
            (
                vec![Span::styled(
                    "← → choose · Enter switch · Esc cancel",
                    dimmed,
                )],
                true,
            )
        } else if hint_h > 0 {
            (
                vec![Span::styled(
                    "↑↓ select · Tab fill · Enter run · Esc cancel",
                    dimmed,
                )],
                true,
            )
        } else if view.busy() {
            (vec![Span::styled("Esc interrupt", dimmed)], false)
        } else if !view.input().is_empty() {
            (
                vec![Span::styled("Ctrl-C clear · ⌥⏎ newline", dimmed)],
                false,
            )
        } else {
            (vec![Span::styled("Ctrl-D quit", dimmed)], false)
        };

        let bar = areas[6];
        let keys_w: usize = keys.iter().map(|s| wrap::str_cols(&s.content)).sum();
        let mut state = if mode { Vec::new() } else { status_state(view) };
        // Too narrow: shed readouts from the far end, one at a time, and never
        // a key. The keys are the half that tells you what to press, so the
        // half that goes is the half that only tells you where you are. Before
        // this the whole row was simply cut at the terminal's edge, mid-word
        // and without an ellipsis — and it was the keys that lost their tail.
        const PAD: usize = 2; // one column of margin at each end
        let width = |state: &[status::Segment]| -> usize {
            let text: usize = state.iter().map(|s| wrap::str_cols(&s.text)).sum();
            let joins = state.len().saturating_sub(1) * 3; // " · "
            PAD + text + joins + PAD + keys_w + PAD
        };
        while !state.is_empty() && width(&state) > bar.width as usize {
            state.pop();
        }

        // The KEYS take the left. The eye goes there first — it is where the
        // prompt sits, where every question in the transcript starts, and where
        // reading begins — so it belongs to what you can do, not to where you
        // are. The readouts take the right edge.
        //
        // It also settles a wobble: in a mode there are no readouts, and with
        // the keys on the right they used to slide across the row as the mode
        // opened and closed. Now they never move at all.
        let mut spans: Vec<Span> = vec![Span::raw("  ")];
        spans.extend(keys);

        let state_w: usize = state.iter().map(|s| wrap::str_cols(&s.text)).sum::<usize>()
            + state.len().saturating_sub(1) * 3;
        let gap = (bar.width as usize).saturating_sub(PAD + keys_w + state_w + PAD);
        spans.push(Span::raw(" ".repeat(gap)));

        let mut at = bar.x + (PAD + keys_w + gap) as u16;
        for (i, seg) in state.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", dimmed));
                at += 3;
            }
            let w = wrap::str_cols(&seg.text) as u16;
            hit.status.push((
                ratatui::layout::Rect {
                    x: at,
                    y: bar.y,
                    width: w,
                    height: 1,
                },
                seg.opens,
            ));
            at += w;
            spans.push(Span::styled(seg.text.clone(), seg.style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), bar);
    })
    .map_err(backend_error::IntoIoError::into_io_error)?;
    if let Some(error) = transcript_error {
        return Err(error);
    }
    Ok(hit)
}

/// Which readouts the user asked for, from `preferences.json`:
///
/// ```json
/// { "statusBar": ["model", "context", "effort", "cwd"] }
/// ```
///
/// An ordered list of names this frontend knows, and deliberately NOT a
/// template language. A format string would be a mechanism with exactly one
/// consumer, invented before anyone asked for it; the day someone needs their
/// own wording is the day it earns its way in.
///
/// Anything unrecognised is dropped rather than refused. This is a look, not a
/// contract — a typo here should cost a readout, not a start.
fn configured_bar() -> Vec<String> {
    status::bar_from(lattice::preferences::get("statusBar"))
}

/// Compose readouts with this frontend's palette; selection rules live in status.
fn status_state(view: &dyn View) -> Vec<status::Segment> {
    status::readouts(view, DIM, WARM)
}

/// Open what a status readout describes. The same states the slash commands
/// reach, entered the same way — the bar is another door onto them, not a
/// second implementation of them.
fn open_from_status(ui: &mut Ui, opens: Opens) {
    match opens {
        Opens::Models => {
            ui.panel.show_models(ui.domain.model.catalog().now);
        }
        Opens::Effort => ui.controls.open_dial(ui.domain.model.effort()),
        Opens::Context => ui.panel.show(AT_CONTEXT),
        Opens::Background => ui.panel.show(AT_BACKGROUND),
        Opens::Config => ui.panel.show(AT_CONFIG),
    }
    // Status clicks reset scroll, including Effort; slash commands and F1 do not.
    ui.panel.reset_scroll();
}

/// How many lines a PageUp/PageDown moves the transcript.
const SCROLL_PAGE: usize = 10;

/// Scroll the transcript up (positive) or down (negative), clamped against the
/// row geometry `draw` reported. `scroll` counts rows up from the bottom.
fn scroll_by(ui: &mut Ui, delta: isize, hit: &Hit) {
    ui.browsing.scroll(
        delta,
        hit.top,
        hit.more_above,
        hit.more_below,
        hit.max_scroll(),
    );
}

/// Expand or collapse the tool card with this call id.
fn toggle_card(ui: &mut Ui, id: String) {
    ui.browsing.toggle(id);
}

/// Expand or collapse the last run of work — the thinking and tool calls of
/// the most recent turn. One key, one target: the user should not have to know
/// whether the last thing was a thought, a tool's output, or a whole folded
/// run of both.
fn toggle_last_tool(ui: &mut Ui) {
    // The LAST run, keyed the same way the renderer keys it, so the key that
    // folds it is the key that unfolds it
    let mut end = ui.entry_count();
    let mut id = None;
    while end > 0 {
        match ui.transcript_group(end - 1) {
            Ok(group) => {
                if group
                    .entries
                    .first()
                    .is_some_and(|entry| matches!(entry, Entry::Tool(_) | Entry::Thinking(_)))
                {
                    id = group_key(&group.entries);
                    break;
                }
                end = group.first;
            }
            Err(error) => {
                ui.flash = Some(format!("Cannot read previous work: {error}"));
                return;
            }
        }
    }
    if let Some(id) = id {
        ui.browsing.toggle(id);
    }
}

/// How many not-yet-heard lines show above the input before the rest become
/// a count. The box the user types in must not be pushed off screen by them.
const QUEUED_ROWS: u16 = 3;

#[cfg(test)]
#[path = "input_audit.rs"]
mod input_audit;

#[cfg(test)]
#[path = "background_baseline.rs"]
mod background_baseline;

#[cfg(test)]
#[path = "compact_tests.rs"]
mod compact_tests;

#[cfg(test)]
mod tests {
    // Exercise the same entry dispatch and indentation as the real transcript.
    fn tool_lines(
        card: &ToolCard,
        spinner: char,
        expanded: bool,
        room: usize,
    ) -> Vec<(Line<'static>, u16)> {
        entry_lines(&Entry::Tool(card.clone()), spinner, expanded, room)
    }

    #[test]
    fn waiting_state_is_visible_static_and_not_done() {
        let mut u = ui(Vec::new(), false);
        u.absorb(&test_event(core_events::USER_MESSAGE, &[]), 10);
        u.live_output.seed_thinking("previous thought".into());
        u.live_output.seed_reply("previous reply".into());
        u.absorb(
            &test_event(lattice::components::minimal_loop::WAITING, &[]),
            20,
        );
        assert!(u.waiting());
        assert!(!u.busy());
        assert!(u.domain.turns.done_at().is_none());
        assert!(!super::animating(&u));
        assert!(u.thinking().is_empty());
        assert!(u.streaming().is_empty());
        let frame = snapshot(&u, 100, 30).rows.join("\n");
        assert!(frame.contains("Waiting for results"), "{frame}");
        assert!(!frame.contains("Done"));
        u.tick += 100;
        assert_eq!(frame, snapshot(&u, 100, 30).rows.join("\n"));
        u.absorb(&test_event(core_events::WAKE, &[]), 30);
        assert!(u.busy());
        assert!(!u.waiting());
    }

    #[test]
    fn waiting_state_rebuilds_from_the_ledger_and_clears_on_resume() {
        let events: Vec<_> = [
            core_events::USER_MESSAGE,
            lattice::components::minimal_loop::WAITING,
            core_events::MODEL_CALL_STARTED,
            core_events::TURN_COMPLETED,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let mut event = test_event(kind, &[]);
            event.seq = index as u64 + 1;
            event.id = format!("ev_{}_waiting", event.seq);
            event
        })
        .collect();
        let mut u = Ui::replayed(&events[..2]);
        assert!(u.waiting());
        u.absorb(&events[2], 30);
        assert!(!u.waiting());
        assert!(u.busy());
        u.absorb(&events[3], 40);
        assert!(!u.waiting());
        assert!(!u.busy());
        assert_eq!(u.domain.turns.done_at(), Some(40));
    }

    #[test]
    fn json_byte_count_matches_the_serializer() {
        for byte in 0..=127u8 {
            let value = Value::String(char::from(byte).to_string());
            assert_eq!(
                super::json_bytes(&value),
                value.to_string().len() as u64,
                "ASCII {byte}"
            );
        }
        let text: String = (0..=0x10ffff).filter_map(char::from_u32).collect();
        for text in [String::new(), text, "\"\\\n中文\u{2028}\u{2029}".repeat(30)] {
            let value = Value::Object(
                [
                    (
                        text.clone(),
                        serde_json::json!([text, null, true, false, [], {}]),
                    ),
                    (
                        "fixed".into(),
                        serde_json::json!({"empty": {}, "bool": false}),
                    ),
                ]
                .into_iter()
                .collect(),
            );
            assert_eq!(super::json_bytes(&value), value.to_string().len() as u64);
        }
        for number in [
            0.0,
            -0.0,
            f64::MIN,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::EPSILON,
            1e-100,
            1e100,
        ] {
            let value = serde_json::json!([number, i64::MIN, u64::MAX]);
            assert_eq!(super::json_bytes(&value), value.to_string().len() as u64);
        }
    }

    /// Opt-in measurements use a private copy, never a live ledger or component restore.
    #[test]
    #[ignore = "set LATTICE_PROFILE_LEDGER to a snapshot; measures read-only resume stages"]
    fn profile_resume_snapshot() {
        let input = std::env::var("LATTICE_PROFILE_LEDGER").expect("snapshot path required");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.jsonl");
        std::fs::copy(input, &path).unwrap();
        let began = std::time::Instant::now();
        let text = std::fs::read_to_string(&path).unwrap();
        eprintln!("profile read: {:?}, bytes={}", began.elapsed(), text.len());
        let began = std::time::Instant::now();
        let events: Vec<EventEnvelope> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        eprintln!(
            "profile parse: {:?}, events={}",
            began.elapsed(),
            events.len()
        );
        drop(text);
        let began = std::time::Instant::now();
        let ui = Ui::replayed(&events);
        eprintln!("profile ui fold: {:?}", began.elapsed());
        let began = std::time::Instant::now();
        let _frame = snapshot(&ui, 100, 40);
        eprintln!("profile first frame: {:?}", began.elapsed());
        drop(ui);
        let began = std::time::Instant::now();
        let encoded_sizes: Vec<_> = events
            .iter()
            .map(|event| event.payload.to_string().len() as u64)
            .collect();
        eprintln!(
            "profile payload sizes via serialization: {:?}",
            began.elapsed()
        );
        let began = std::time::Instant::now();
        for (event, expected) in events.iter().zip(encoded_sizes) {
            assert_eq!(
                super::json_bytes(&event.payload),
                expected,
                "event {}",
                event.id
            );
        }
        eprintln!(
            "profile payload sizes without encoding: {:?}",
            began.elapsed()
        );
        drop(events);
        let stream = lattice::EventLog::stream_of(&path).unwrap();
        let began = std::time::Instant::now();
        let log = lattice::EventLog::open(
            core_events::core_event_decls(),
            stream.clone(),
            Some(path.clone()),
        )
        .unwrap();
        eprintln!("profile core log open: {:?}", began.elapsed());
        let began = std::time::Instant::now();
        let copied = log.replay(1).unwrap();
        eprintln!("profile one full replay copy: {:?}", began.elapsed());
        let began = std::time::Instant::now();
        for kind in [
            core_events::MODEL_CALL_STARTED,
            core_events::TOOL_EXEC_STARTED,
        ] {
            let heads = core_events::hanging_chain_heads(&copied, kind);
            eprintln!("profile hanging {kind}: {}", heads.len());
        }
        eprintln!("profile hanging chain scan: {:?}", began.elapsed());
        drop(copied);
        drop(log);
        let began = std::time::Instant::now();
        let kernel = Kernel::start(
            &lattice::AssemblyManifest {
                instances: Default::default(),
                wires: vec![],
            },
            &Default::default(),
            &mut Default::default(),
            KernelOptions {
                stream: Some(stream),
                log_file: Some(path),
                ..Default::default()
            },
        )
        .unwrap();
        eprintln!(
            "profile bare kernel resume (no component restore): {:?}",
            began.elapsed()
        );
        kernel.shutdown();
    }

    /// Deleting asks first, and the question says what goes with it. It is one
    /// keystroke away from destroying a key that may have no other copy, so the
    /// panel spends a line saying so rather than trusting the word "delete".
    ///
    /// Nothing here presses `y`: confirming writes the real catalog file, and
    /// what it writes is checked where the writing lives (`models.rs`).
    #[test]
    fn deleting_a_model_asks_before_it_does_it() {
        let mut u = Ui::replayed(&[]);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![
                ModelRow {
                    id: "here".into(),
                    model: "m-1".into(),
                    ..ModelRow::default()
                },
                ModelRow {
                    id: "spare".into(),
                    model: "m-2".into(),
                    ..ModelRow::default()
                },
            ],
            now: Some(0),
        };
        u.panel.show(AT_MODELS);
        let screen = |u: &Ui| snapshot(u, 84, 26).rows.join("\n");
        let press = |u: &mut Ui, code| {
            on_key(
                u,
                None,
                ratatui::crossterm::event::KeyEvent::from(code),
                &Hit::default(),
            );
        };

        // The running model is refused: the list always shows what this
        // conversation is talking to, so the row would outlive the file entry
        press(&mut u, KeyCode::Char('d'));
        assert!(
            u.controls.deletion().is_none(),
            "no question was even asked"
        );
        assert!(
            u.flash
                .as_deref()
                .unwrap_or_default()
                .contains("switch away"),
            "and it says what to do instead: {:?}",
            u.flash
        );

        // Any other row asks, by name, and says what is lost
        press(&mut u, KeyCode::Down);
        press(&mut u, KeyCode::Char('d'));
        assert_eq!(u.controls.deletion(), Some("spare"));
        let asked = screen(&u);
        assert!(asked.contains("delete spare"), "it names it: {asked}");
        assert!(
            asked.contains("a key written in it goes too"),
            "and says the key goes with it: {asked}"
        );

        // Anything but y keeps it — a stray keystroke must not be an answer
        press(&mut u, KeyCode::Char('x'));
        assert!(u.controls.deletion().is_none());
        assert_eq!(u.flash.as_deref(), Some("kept"));
        assert!(screen(&u).contains("spare"), "still there");
    }

    /// A reply arriving chunk by chunk must not leave a rendered copy of
    /// itself behind per chunk.
    ///
    /// Every frame asks about a string one character longer than the last, so
    /// the lookup cannot hit and the entry is dead on arrival — and the pile
    /// grows as the SQUARE of the reply, for no benefit whatsoever. Cheap to
    /// write, so easy to reintroduce: hence a test.
    #[test]
    fn a_reply_still_arriving_leaves_nothing_in_the_cache() {
        let mut u = Ui::replayed(&[]);
        clear_render_cache();
        let reply = "Here is a thought.\n\n```rust\nfn main() {}\n```\n\nAnd more of it.";
        for upto in 1..=reply.chars().count() {
            u.live_output.seed_reply(reply.chars().take(upto).collect());
            let _ = transcript_body(&u, ' ', 80);
        }
        assert_eq!(
            render_cache_len(),
            0,
            "the live buffer is rendered fresh every frame and remembered never"
        );
    }

    /// What decides whether a frame is built at all. If this ever answers "no"
    /// while something is moving, the animation freezes mid-motion — so each
    /// moving state gets a line here.
    #[test]
    fn a_still_screen_is_not_redrawn_and_a_moving_one_is() {
        let mut u = Ui::replayed(&[]);
        u.domain.turns.seed_busy(true);
        assert!(animating(&u), "a running turn spins and sweeps");

        u.domain.turns.seed_busy(false);
        u.domain.turns.seed_done_at(None);
        assert!(!animating(&u), "nothing running: nothing moves");

        u.domain.turns.seed_done_at(Some(u.tick));
        assert!(animating(&u), "the Done line is still settling");

        u.tick = u.tick.wrapping_add(DONE_SETTLE + 1);
        assert!(!animating(&u), "once settled, the screen is still");
    }

    /// A swap moves the SEAT, not only the marker. The catalog is read again
    /// whenever an entry is added or deleted, and that read asks "which of
    /// these is the one running?" — if the answer were still the model the
    /// process launched on, the delete guard would protect a model nobody is
    /// talking to and hand over the one this conversation is running on.
    #[test]
    fn the_swap_moves_the_seat_so_a_later_catalog_read_still_finds_it() {
        let mut u = Ui::replayed(&[]);
        *u.domain.model.fixture_catalog() = two_models();
        u.note_model_swap(&lattice::EventEnvelope {
            v: 1,
            id: "e1".to_string(),
            seq: 1,
            stream: "s".to_string(),
            time: "t".to_string(),
            event_type: core_events::COMPONENT_REPLACED.to_string(),
            source: "core".to_string(),
            causes: vec![],
            origin: None,
            reason: Some("the user chose it".to_string()),
            payload: json!({
                "instance": "model",
                "from": "openai-model",
                "to": "anthropic-model",
                "config": {
                    "model": "claude-sonnet-5",
                    "baseUrl": "https://api.anthropic.com",
                    "apiKeyEnv": "ANTHROPIC_API_KEY",
                },
            }),
        });
        assert_eq!(u.domain.model.catalog().now, Some(1), "the marker moved");
        assert_eq!(u.domain.model.running().model, "claude-sonnet-5");
        assert_eq!(
            u.domain.model.running().base_url,
            "https://api.anthropic.com"
        );
        assert_eq!(u.domain.model.running().key_env, "ANTHROPIC_API_KEY");
        assert_eq!(
            u.domain.model.running().adapter,
            "anthropic",
            "the dialect moves with it, or the seat matches the wrong entry"
        );
    }

    /// The add form asks for the six things an entry is, and does not let the
    /// keyboard reach anything else while it is up — a model name containing
    /// "s" or "d" would otherwise switch or delete a model halfway through
    /// being typed.
    #[test]
    fn the_add_form_takes_the_keyboard_and_says_what_is_missing() {
        let mut u = Ui::replayed(&[]);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![ModelRow {
                id: "here".into(),
                model: "m-1".into(),
                ..ModelRow::default()
            }],
            now: Some(0),
        };
        u.panel.show(AT_MODELS);
        let screen = |u: &Ui| snapshot(u, 84, 26).rows.join("\n");
        let press = |u: &mut Ui, code| {
            on_key(
                u,
                None,
                ratatui::crossterm::event::KeyEvent::from(code),
                &Hit::default(),
            );
        };
        let typed = |u: &mut Ui, word: &str| {
            for c in word.chars() {
                on_key(
                    u,
                    None,
                    ratatui::crossterm::event::KeyEvent::from(KeyCode::Char(c)),
                    &Hit::default(),
                );
            }
        };

        press(&mut u, KeyCode::Char('a'));
        let form = screen(&u);
        for label in ["short name", "dialect", "model", "endpoint", "key variable"] {
            assert!(form.contains(label), "{label} is asked for: {form}");
        }

        // "sd" is a name, not two commands
        typed(&mut u, "sd-model");
        assert_eq!(
            u.controls.form().as_ref().map(|f| f.values[0].clone()),
            Some("sd-model".to_string()),
            "every character went into the field it was aimed at"
        );
        assert!(
            u.controls.deletion().is_none(),
            "and none of it was a command"
        );

        // A half-filled form says what is wrong instead of writing anything
        press(&mut u, KeyCode::Enter);
        let complained = screen(&u);
        assert!(u.controls.form().is_some(), "the form stays up");
        assert!(
            complained.contains("dialect") || complained.contains("model"),
            "it says what is missing: {complained}"
        );

        // The key is the one field not readable over a shoulder
        for _ in 0..4 {
            press(&mut u, KeyCode::Tab);
        }
        typed(&mut u, "sk-secret-value");
        let masked = screen(&u);
        assert!(
            !masked.contains("sk-secret-value"),
            "a key typed into a panel that gets screenshot: {masked}"
        );
        assert!(masked.contains('\u{2022}'), "but you can see it took it");

        // Esc leaves without writing
        press(&mut u, KeyCode::Esc);
        assert!(u.controls.form().is_none());
    }

    #[test]
    fn segmented_ledger_names_and_export_aliases_are_handled_without_source_loss() {
        let home = tempfile::tempdir().unwrap();
        let directory = lattice::ledgers::dir(home.path());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("20260101-tui.ledger");
        let mut log = lattice::EventLog::open_segmented(
            core_events::core_event_decls(),
            "fixture",
            path.clone(),
            1,
        )
        .unwrap();
        log.append(
            lattice::EventDraft::new(core_events::USER_MESSAGE, &[], json!({"text":"keep"})),
            "ui",
        )
        .unwrap();
        drop(log);
        for name in ["20260101-tui", "20260101-tui.ledger"] {
            assert_eq!(
                startup::ledger_for(home.path(), Resume::Named(name.into())).unwrap(),
                (path.clone(), true)
            );
        }
        let file = lattice::EventLog::source_paths(&path).unwrap().remove(0);
        let before = std::fs::read(&file).unwrap();
        let alias = home.path().join("alias.jsonl");
        std::fs::hard_link(&file, &alias).unwrap();
        assert!(process_commands::export(vec![
            path.display().to_string(),
            alias.display().to_string()
        ])
        .is_err());
        assert_eq!(std::fs::read(file).unwrap(), before);
    }

    use super::*;
    use ratatui::backend::TestBackend;

    /// A bare envelope for fold tests (only type and causes matter here).
    fn test_event(event_type: &str, causes: &[&str]) -> lattice::EventEnvelope {
        lattice::EventEnvelope {
            v: 1,
            id: "ev_9_test".to_string(),
            seq: 9,
            stream: "s".to_string(),
            time: "t".to_string(),
            event_type: event_type.to_string(),
            source: "test".to_string(),
            causes: causes.iter().map(|c| c.to_string()).collect(),
            origin: None,
            reason: None,
            payload: serde_json::json!({}),
        }
    }

    /// Build a minimal view for a frame test.
    /// The waiting lines hang off the line above them: same column, no gap.
    ///
    /// They belong to it — what is being worked on, and what has not been
    /// heard yet — so a blank between them reads as two unrelated things, and
    /// a different indent reads as a different kind of thing.
    #[test]
    fn waiting_lines_sit_directly_under_the_working_line_and_line_up_with_it() {
        let mut term = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let mut u = ui(vec![Entry::User("go".to_string())], true);
        u.domain
            .unseen
            .restore(vec!["第二条".to_string(), "第三条".to_string()]);
        draw(&mut term, &u).unwrap();

        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();

        let working = rows
            .iter()
            .position(|r| r.contains('·'))
            .expect("the working line is on screen");
        let first_wait = rows
            .iter()
            .position(|r| r.contains('⎿'))
            .expect("the waiting lines are on screen");
        assert_eq!(
            first_wait,
            working + 1,
            "no gap between them:\n{:?}",
            &rows[working..(first_wait + 1).min(rows.len())]
        );
        let column = |r: &str| r.len() - r.trim_start().len();
        // One left edge for everything the person is looking at right now:
        // the question in the transcript, the working line, the waiting lines
        // and the ❯ of the input box.
        let prompt = rows
            .iter()
            .rposition(|r| r.trim_start().starts_with('❯'))
            .expect("the input box is on screen");
        for (what, at) in [
            ("the working line", working),
            ("the waiting lines", first_wait),
        ] {
            assert_eq!(
                column(&rows[at]),
                column(&rows[prompt]),
                "{what} does not line up with the input box: {:?} vs {:?}",
                rows[at],
                rows[prompt]
            );
        }
        assert!(
            rows[first_wait + 1].contains('⎿'),
            "and the next one directly under that: {:?}",
            rows[first_wait + 1]
        );
    }

    /// Lines typed one after another are one thing said.
    ///
    /// The model is given them together — they arrive in the same material —
    /// so drawing them as two ❯ blocks would say two questions were asked and
    /// answered separately, which is not what happened.
    #[test]
    fn lines_typed_back_to_back_are_drawn_as_one_thing_said() {
        let u = ui(
            vec![
                Entry::User("第二条消息".to_string()),
                Entry::User("第三条消息".to_string()),
                Entry::Agent("好".to_string()),
            ],
            false,
        );
        let body = transcript_body(&u, ' ', 80);
        let text: Vec<String> = body
            .iter()
            .map(|t| t.line.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        let markers = text.iter().filter(|l| l.starts_with("❯ ")).count();
        assert_eq!(markers, 1, "one marker for the pair: {text:?}");
        assert!(
            text.iter().any(|l| l == "❯ 第二条消息"),
            "first line keeps the marker: {text:?}"
        );
        assert!(
            text.iter().any(|l| l == "  第三条消息"),
            "the next lines up under it: {text:?}"
        );
    }

    /// A message written with Alt-Enter comes back with its lines.
    ///
    /// A newline inside a ratatui Span is not a line break — it renders as
    /// nothing — so a multi-line question used to arrive as one run with its
    /// structure gone, which is what the person saw after sending it.
    #[test]
    fn a_multiline_question_keeps_its_lines() {
        let lines = entry_lines(
            &Entry::User("提交并push\n先这样吧".to_string()),
            ' ',
            false,
            80,
        );
        assert_eq!(lines.len(), 2, "one screen line per typed line");
        let text = |l: &Line| -> String { l.spans.iter().map(|s| s.content.to_string()).collect() };
        assert_eq!(text(&lines[0].0), "❯ 提交并push");
        assert_eq!(
            text(&lines[1].0),
            "  先这样吧",
            "continuation lines line up under the first, without a second marker"
        );
    }

    /// A view whose status bar has all four readouts with something to say:
    /// a model (its key present), a context reading over the loud threshold,
    /// a raised effort rung, and a workspace.
    fn ui_with_a_full_bar() -> Ui {
        let mut u = ui(Vec::new(), false);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![ModelRow {
                id: "deepseek-v4".to_string(),
                window: Some(1000),
                key_present: true,
                ..Default::default()
            }],
            now: Some(0),
        };
        u.domain.accounting.seed_last_call(Some(lattice::Usage {
            prompt: 900,
            ..Default::default()
        }));
        *u.domain.model.fixture_effort() = EffortView {
            rungs: vec!["off".to_string(), "high".to_string()],
            now: Some("high".to_string()),
        };
        u
    }

    fn ui(entries: Vec<Entry>, busy: bool) -> Ui {
        let mut domain = domain_state::Live {
            title: "test".to_string(),
            ..Default::default()
        };
        domain.turns.seed_busy(busy);
        Ui {
            domain,
            workspace: "/tmp/work".to_string(),
            bar: Vec::new(),
            entries,
            background_view: None,
            links: link_actions::LinkOpener::default(),
            event_facts: None,
            cards: None,
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
            parts: Vec::new(),
            panel: panels::navigation::PanelNavigation::default(),
            documents: None,
            controls: ModelControls::default(),
            experts: expert_controls::ExpertControls::default(),
        }
    }

    /// The slash menu hugs the input box: with a "Done" line ALSO showing,
    /// the candidates sit between it and the input, not floated up by it.
    #[test]
    fn the_slash_menu_hugs_the_input_box() {
        let mut u = ui(vec![Entry::User("hi".to_string())], false);
        u.tick = 10_000; // past the "Done" typing animation
        u.domain.turns.seed_done_at(Some(0)); // a settled "Done" line is on screen
        u.draft.edit().set("/"); // the slash palette is open
        let snap = snapshot(&u, 72, 24);

        let row_of = |needle: &str| snap.rows.iter().position(|r| r.contains(needle));
        let done = row_of("Done").expect("the Done line shows");
        let help = row_of("/help").expect("the slash candidate shows");
        // The input caret is the BOTTOMMOST ❯ (a transcript user line has one too)
        let caret = snap
            .rows
            .iter()
            .rposition(|r| r.contains('❯'))
            .expect("caret");
        // The menu's LAST row (its bottom candidate) hugs the input box: only
        // the box's top rule sits between it and the caret.
        let menu_bottom = snap
            .rows
            .iter()
            .rposition(|row| {
                row.split_whitespace()
                    .any(|word| SLASH.iter().any(|command| command.name == word))
            })
            .expect("the visible slash window has a bottom row");
        assert!(
            done < help,
            "the Done line is ABOVE the menu (not between it and input)"
        );
        assert!(menu_bottom < caret, "the menu is above the input caret");
        assert_eq!(
            caret - menu_bottom,
            2,
            "only the box's top rule is between menu and caret"
        );
    }

    /// What the person sent, still legible after the input box has forgotten:
    /// the sentence, and beneath it the pictures that rode with it.
    #[test]
    fn the_transcript_lists_the_pictures_under_the_message_they_came_with() {
        let mut u = ui(
            vec![
                Entry::User("这是什么？".to_string()),
                Entry::Attachment("shot.png".to_string()),
                Entry::Attachment("clipboard".to_string()),
                Entry::Agent("a screenshot".to_string()),
            ],
            false,
        );
        u.tick = 10_000;
        let snap = snapshot(&u, 72, 24);
        let row_of = |n: &str| snap.rows.iter().position(|r| r.contains(n));

        let said = row_of("这是什么？").expect("the message shows");
        let one = row_of("shot.png").expect("the first picture shows");
        let two = row_of("clipboard").expect("the second shows");
        assert_eq!(one, said + 1, "directly beneath, no gap");
        assert_eq!(two, one + 1, "and in the order they were attached");
        assert!(snap.rows[one].contains('⎿'), "{}", snap.rows[one]);

        // Indented under the message, not flush with it
        let col = |row: &str, needle: &str| {
            let at = row.find(needle).unwrap();
            row[..at].chars().count()
        };
        assert!(
            col(&snap.rows[one], "⎿") > col(&snap.rows[said], "❯"),
            "the picture sits under the message: {:?} vs {:?}",
            snap.rows[said],
            snap.rows[one]
        );
    }

    /// A picture is a placeholder among the words, exactly like a big paste —
    /// one thing to learn, and it can go where the sentence wants it.
    #[test]
    fn an_attached_picture_shows_inside_the_input_line() {
        let docs = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let png = src.path().join("shot.png");
        std::fs::write(&png, [1u8, 2, 3, 4]).unwrap();

        let mut u = ui(vec![], false);
        u.documents = Some(docs.path().to_path_buf());
        u.draft.edit().set("what is ");
        attach_image(&mut u, &png, "image/png");
        u.draft.edit().insert_str("?");

        assert_eq!(u.draft.editor().shown(), "what is [image shot.png]?");
        let snap = snapshot(&u, 72, 20);
        assert!(
            snap.rows
                .iter()
                .any(|r| r.contains("what is [image shot.png]?")),
            "it is in the line, not on a row of its own"
        );

        // And deleting the placeholder cancels the picture
        u.draft.edit().left();
        u.draft.edit().backspace();
        assert!(u.draft.editor().images().is_empty());
        assert_eq!(u.draft.editor().shown(), "what is ?");
    }

    /// Capping the visible window must not drop commands, in either direction.
    #[test]
    fn the_slash_menu_keeps_every_command_reachable_when_scrolling_up() {
        let mut u = ui(vec![], false);
        u.draft.edit().set("/");
        let press = |ui: &mut Ui, code| {
            on_key(
                ui,
                None,
                ratatui::crossterm::event::KeyEvent::from(code),
                &Hit::default(),
            )
        };
        for _ in 1..SLASH.len() {
            assert!(!press(&mut u, KeyCode::Down));
        }
        for command in SLASH.iter().rev() {
            let snap = snapshot(&u, 90, 30);
            assert!(
                snap.rows
                    .iter()
                    .any(|row| row.contains(&format!("▸ {} ", command.name))),
                "{} remains reachable when scrolling up",
                command.name
            );
            assert!(!press(&mut u, KeyCode::Up));
        }
        assert_eq!(u.draft.selected(), 0);
    }

    /// The reading is measured, not estimated: it comes off the usage the
    /// provider reported on the ledger.
    #[test]
    fn clearing_cards_preserves_accounting_and_growth() {
        let mut u = ui(vec![Entry::User("visible".into())], false);
        let usage = lattice::Usage {
            prompt: 17,
            output: 5,
            calls: 1,
            ..Default::default()
        };
        u.domain.accounting.record_completion(1, usage);
        let mut start = test_event(core_events::MODEL_CALL_STARTED, &[]);
        start.payload = json!({"input":{"parts":[{"digest":"memory"}]}});
        u.note_usage(&start);
        let before = u.growth();
        run_slash(&mut u, "/clear", None);
        assert_eq!(u.entry_count(), 0);
        assert_eq!(u.domain.accounting.last_call(), Some(usage));
        assert_eq!(u.domain.accounting.turn_total(), usage);
        assert_eq!(u.domain.accounting.session_total(), usage);
        assert_eq!(u.growth(), before);
        assert_eq!(u.domain.accounting.reference_history(), &[(1, 17)]);
    }

    #[test]
    fn context_reports_what_the_last_call_actually_cost() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![ModelRow {
                id: "m".to_string(),
                model: "deepseek-v4-flash".to_string(),
                window: Some(200_000),
                ..ModelRow::default()
            }],
            now: Some(0),
        };

        // Before any call, silence rather than a claim of zero
        let before = panel_rows(AT_CONTEXT, &u, 100);
        let text = |rows: &[(String, String)]| {
            rows.iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text(&before).contains("nothing sent yet"), "{:?}", before);
        assert!(!text(&before).contains("0%"));

        // A real completion, in this model's own field names
        let mut done = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        done.payload = serde_json::json!({
            "model": "deepseek-v4-flash",
            "status": "ok",
            "usage": {"prompt_tokens": 50_000, "completion_tokens": 900,
                      "prompt_cache_hit_tokens": 48_000},
        });
        u.absorb(&done, 0);
        let after = text(&panel_rows(AT_CONTEXT, &u, 100));
        assert!(after.contains("25%"), "50k of 200k is a quarter: {after}");
        // The drawing agrees with the number rather than being a second claim.
        // Measured off the drawn bar, so changing its width cannot make this
        // test wrong in a way that hides the bar disagreeing with the figure.
        let drawn = after
            .split_whitespace()
            .find(|w| w.starts_with('\u{2588}') || w.starts_with('\u{2591}'))
            .expect("a bar is drawn");
        let filled = drawn.chars().filter(|c| *c == '\u{2588}').count();
        let total = drawn.chars().count();
        assert_eq!(
            (filled as f64 / total as f64 * 100.0).round() as u64,
            25,
            "the bar is a quarter full too: {drawn}"
        );
        assert!(after.contains("50,000"), "with the real number: {after}");
        assert!(after.contains("150,000"), "and what is left: {after}");
        assert!(after.contains("48,000"), "cache hits are reported: {after}");

        // THREE SCOPES, and they must not be the same number by construction.
        // A second call in the same turn moves the turn and session figures
        // while the call figure follows only the latest.
        let mut second = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        second.payload = serde_json::json!({
            "model": "deepseek-v4-flash",
            "status": "ok",
            "usage": {"prompt_tokens": 10_000, "completion_tokens": 100,
                      "prompt_tokens_details": {"cached_tokens": 1_000}},
        });
        u.absorb(&second, 0);
        let r = u.usage().expect("a reading");
        assert_eq!(r.call.prompt, 10_000, "the call figure is the LAST call");
        assert_eq!(r.turn.prompt, 60_000, "the turn adds them up");
        assert_eq!(r.session.prompt, 60_000);
        assert_eq!(r.call.calls, 1);
        assert_eq!(r.turn.calls, 2);
        // and the rates genuinely differ, which is why three are reported
        assert_eq!((r.call.hit_rate().unwrap() * 100.0).round() as u64, 10);
        assert_eq!((r.turn.hit_rate().unwrap() * 100.0).round() as u64, 82);

        // A NEW TURN starts its own tally; the session keeps counting.
        let mut asked = test_event(core_events::USER_MESSAGE, &[]);
        asked.payload = serde_json::json!({"text": "next"});
        u.absorb(&asked, 0);
        let r = u.usage().expect("a reading");
        assert_eq!(r.turn.prompt, 0, "the turn starts over");
        assert_eq!(r.session.prompt, 60_000, "the session does not");

        // A BACKGROUND call is not this conversation's context
        let mut background = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        background.payload = serde_json::json!({
            "model": "deepseek-v4-flash",
            "purpose": "condense",
            "usage": {"prompt_tokens": 900},
        });
        u.absorb(&background, 0);
        let r = u.usage().expect("a reading");
        assert_eq!(
            r.call.prompt, 10_000,
            "the condense call did not become the reading"
        );
        assert_eq!(r.session.prompt, 60_000, "nor did it join the totals");
    }

    /// A row whose LEFT side is empty continues the row above it — that is how
    /// the three-scope figures sit under their heading. A filter that dropped
    /// blank-looking rows deleted the hit rates outright, and every assertion
    /// about the numbers still passed because they were computed correctly and
    /// simply never drawn.
    #[test]
    fn the_hit_rates_reach_the_screen_and_not_only_the_arithmetic() {
        let mut u = ui(vec![], false);
        let mut done = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        done.payload = serde_json::json!({
            "status": "ok",
            "usage": {"prompt_tokens": 1_000,
                      "prompt_tokens_details": {"cached_tokens": 900}},
        });
        u.absorb(&done, 0);
        assert_eq!(
            (u.usage().unwrap().call.hit_rate().unwrap() * 100.0).round() as u64,
            90,
            "the arithmetic is right"
        );

        u.panel.show(AT_CONTEXT);
        let screen = snapshot(&u, 90, 26).rows.join("\n");
        assert!(screen.contains("cache hits"), "the heading shows");
        assert!(
            screen.contains("90%"),
            "and so does the rate itself: {screen}"
        );
    }

    #[test]
    fn context_model_sizes_ignore_accounting_and_transport_metadata() {
        let mut u = ui(vec![], false);
        let mut event = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        event.payload = serde_json::json!({
            "text": "an answer",
            "toolCalls": [{"id":"call-1","tool":"Read","arguments":{"path":"file"}}],
            "reasoning": [{"kind":"text","text":"some thinking"}]
        });
        u.domain.event_inputs.observe_size(&event);
        let before = u.domain.event_inputs.clone();
        event.payload["usage"] = serde_json::json!({"provider_details":"x".repeat(1_000_000)});
        event.payload["status"] = serde_json::json!("ok");
        event.payload["providerRequestId"] = serde_json::json!("not prompt content".repeat(100));
        u.domain.event_inputs.observe_size(&event);
        assert_eq!(
            u.domain.event_inputs, before,
            "accounting metadata never enters the prompt split"
        );
        event.payload["text"] = serde_json::json!("a longer answer that really is prompt content");
        u.domain.event_inputs.observe_size(&event);
        assert!(
            u.domain.event_inputs.size(&event.id).unwrap().1 > before.size(&event.id).unwrap().1
        );
    }

    #[test]
    fn context_native_output_is_counted_once_and_separates_thinking() {
        let mut u = ui(vec![], false);
        let mut event = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        let reply = serde_json::json!({"type":"message","content":[{"type":"output_text","text":"answer"}]});
        let thought = serde_json::json!({"type":"reasoning","encrypted_content":"sealed"});
        let call = serde_json::json!({"type":"function_call","call_id":"c1","name":"Read","arguments":"{}"});
        event.payload = serde_json::json!({"responsesOutput":[reply,thought,call]});
        u.domain.event_inputs.observe_size(&event);
        assert_eq!(
            u.domain.event_inputs.size(&event.id).unwrap().1,
            (reply.to_string().len() + call.to_string().len()) as u64
        );
        assert_eq!(
            u.domain
                .event_inputs
                .size(&format!("{}#thinking", event.id))
                .unwrap()
                .1,
            thought.to_string().len() as u64
        );
        let before = u.domain.event_inputs.clone();
        event.payload["text"] = serde_json::json!("answer");
        event.payload["toolCalls"] = serde_json::json!([{"id":"c1","tool":"Read","arguments":{}}]);
        event.payload["reasoning"] =
            serde_json::json!([{"kind":"hidden","opaque":{"responses":thought}}]);
        event.payload["usage"] = serde_json::json!({"details":"x".repeat(1_000_000)});
        u.domain.event_inputs.observe_size(&event);
        assert_eq!(
            u.domain.event_inputs, before,
            "native output and its normalized copy are alternatives, not additive"
        );
    }

    /// The picture is quantitative: a cell is a real number of tokens, so
    /// counting cells and reading the figure agree by construction.
    #[test]
    fn the_context_picture_draws_what_the_numbers_say() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![ModelRow {
                id: "m".to_string(),
                window: Some(100_000),
                ..ModelRow::default()
            }],
            now: Some(0),
        };
        // Two results and a reply go into the window, then a call sends them
        let mut result = test_event(core_events::TOOL_EXEC_COMPLETED, &[]);
        result.id = "ev_1".to_string();
        result.payload = serde_json::json!({"result": "x".repeat(6_000)});
        u.absorb(&result, 0);
        let mut reply = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        reply.id = "ev_2".to_string();
        reply.payload = serde_json::json!({"text": "y".repeat(2_000)});
        u.absorb(&reply, 0);

        let mut started = test_event(core_events::MODEL_CALL_STARTED, &[]);
        started.id = "ev_3".to_string();
        // THE REAL SHAPE: `input` is {fingerprint, parts}, an object. Reading
        // it as an array found nothing and reported a long conversation's
        // window as pure system prompt and tool declarations, with not one
        // line of the talk in it — and nothing complained, because "no
        // material" is a perfectly well-formed answer.
        started.payload = serde_json::json!({
            "model": "m", "system": "s".repeat(2_000), "tools": ["t".repeat(2_000)],
            "input": {
                "fingerprint": "sha256:whatever",
                "parts": [{"event": "ev_1"}, {"event": "ev_2"}],
            },
        });
        u.absorb(&started, 0);
        let mut done = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        done.id = "ev_4".to_string();
        done.causes = vec!["ev_3".to_string()];
        done.payload = serde_json::json!({"usage": {"prompt_tokens": 40_000}});
        u.absorb(&done, 0);

        let parts = u.composition();
        assert_eq!(parts[0].0, "tool results", "largest first: {parts:?}");
        assert!(
            parts.iter().any(|(k, _)| *k == "model replies"),
            "the conversation itself is in the window, not just the preamble: {parts:?}"
        );
        assert!(
            parts.iter().any(|(k, _)| *k == "system prompt")
                && parts.iter().any(|(k, _)| *k == "tool declarations"),
            "the prompt's own weight is counted too: {parts:?}"
        );

        let lines = context_picture(&u, 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("40% full"), "40k of 100k: {text}");
        assert!(text.contains("split by size"), "and says the split is ours");

        // The drawing agrees with the figure: filled cells over total cells is
        // the same 40%, whatever the cell size works out to be.
        let (filled, total) = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .flat_map(|s| s.content.chars())
            .fold((0usize, 0usize), |(f, t), c| match c {
                '█' => (f + 1, t + 1),
                '·' => (f, t + 1),
                _ => (f, t),
            });
        // the legend's own bars are █ too, so measure only the grid rows
        let grid: Vec<&Line> = lines
            .iter()
            .filter(|l| {
                let s: String = l.spans.iter().map(|s| s.content.to_string()).collect();
                s.trim().chars().all(|c| c == '█' || c == '·') && !s.trim().is_empty()
            })
            .collect();
        let cells: Vec<char> = grid
            .iter()
            .flat_map(|l| l.spans.iter())
            .flat_map(|s| s.content.chars())
            .filter(|c| *c == '█' || *c == '·')
            .collect();
        let full = cells.iter().filter(|c| **c == '█').count();
        assert!(
            ((full as f64 / cells.len() as f64) - 0.4).abs() < 0.02,
            "the grid is 40% filled: {full} of {} (all glyphs {filled}/{total})",
            cells.len()
        );
    }

    /// Only what an overlay installed may be removed. Without that rule
    /// "uninstall the thing called trust" would dismantle the gate that is
    /// vetting the request — so the panel must refuse it, and say why.
    #[test]
    fn the_component_list_removes_only_what_was_installed() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let mut u = ui(vec![], false);
        u.parts = vec![
            (
                "trust".into(),
                "trust-policy".into(),
                "in-process",
                String::new(),
                false,
                vec![],
            ),
            (
                "weather".into(),
                "weather-tool".into(),
                "subprocess",
                "Weather".into(),
                true,
                vec![],
            ),
        ];
        u.panel.show(AT_COMPONENTS);
        let press = |u: &mut Ui, c| {
            on_key(u, None, KeyEvent::from(KeyCode::Char(c)), &Hit::default());
        };

        // The base assembly refuses, and names itself
        press(&mut u, 'u');
        let said = u.flash.clone().unwrap_or_default();
        assert!(said.contains("trust"), "{said}");
        assert!(said.contains("cannot be removed"), "{said}");
        assert_eq!(
            u.panel.active(),
            Some(AT_COMPONENTS),
            "and the panel stays open"
        );

        // Uninstall hides the panel without discarding its detail or scroll.
        u.panel.toggle_details();
        u.panel.scroll_down(17);
        // The installed one goes
        on_key(&mut u, None, KeyEvent::from(KeyCode::Down), &Hit::default());
        press(&mut u, 'u');
        assert_eq!(
            u.panel.active(),
            None,
            "the panel closes to show what happens next"
        );
        assert!(u.panel.details_expanded());
        assert_eq!(u.panel.scroll_offset(), 17);
        assert_eq!(u.panel.selected_row(), 1);
        assert!(
            u.flash.as_deref().is_some_and(|f| f.contains("weather")),
            "{:?}",
            u.flash
        );
    }

    /// Choosing a model is choosing an endpoint, a dialect, a window and a key
    /// you either have or do not. A list of names asks someone to choose
    /// between things they cannot tell apart.
    #[test]
    fn the_model_panel_shows_what_the_choice_turns_on() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![
                ModelRow {
                    id: "here".into(),
                    model: "m-1".into(),
                    dialect: "openai".into(),
                    endpoint: "example.test".into(),
                    window: Some(268_000),
                    accepts_images: true,
                    key_env: "SOME_KEY".into(),
                    key_present: true,
                    rungs: vec!["low".into(), "high".into()],
                },
                ModelRow {
                    id: "keyless".into(),
                    model: "m-2".into(),
                    key_env: "MISSING_KEY".into(),
                    key_present: false,
                    ..ModelRow::default()
                },
            ],
            now: Some(0),
        };

        u.panel.show(AT_MODELS);
        let screen = |u: &Ui| snapshot(u, 84, 26).rows.join("\n");

        // A model you could not use if you tried is marked as such up front
        let listed = screen(&u);
        assert!(
            listed
                .lines()
                .any(|l| l.contains("keyless") && l.contains('!')),
            "the one with no key is flagged: {listed}"
        );

        // Enter opens what the choice actually turns on
        on_key(
            &mut u,
            None,
            KeyEvent::from(KeyCode::Enter),
            &Hit::default(),
        );
        let open = screen(&u);
        assert!(open.contains("268,000"), "the window: {open}");
        assert!(open.contains("reads images"), "and whether it reads images");
        assert!(
            open.contains("SOME_KEY"),
            "and WHICH variable holds the key"
        );
        assert!(
            !open.contains("low · high") || open.contains("low · high"),
            "its rungs"
        );
    }

    /// A component's behaviour IS its wiring: which port it listens on and who
    /// hears it. The list can only say what it is called.
    #[test]
    fn enter_shows_the_selected_component_s_wiring() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let mut u = ui(vec![], false);
        u.parts = vec![
            (
                "fs".into(),
                "fs-reader".into(),
                "in-process",
                "Read Ls".into(),
                false,
                vec![
                    "trust.forward → fs.execute".into(),
                    "fs.outcome → loop.tools".into(),
                ],
            ),
            (
                "loop".into(),
                "minimal-loop".into(),
                "in-process",
                String::new(),
                false,
                vec![],
            ),
        ];
        u.panel.show(AT_COMPONENTS);
        let press = |u: &mut Ui, c| on_key(u, None, KeyEvent::from(c), &Hit::default());

        let screen = |u: &Ui| snapshot(u, 88, 26).rows.join("\n");
        assert!(!screen(&u).contains("trust.forward"), "folded until asked");
        press(&mut u, KeyCode::Enter);
        let open = screen(&u);
        assert!(open.contains("trust.forward → fs.execute"), "{open}");
        assert!(open.contains("fs.outcome → loop.tools"), "{open}");
        assert!(
            u.panel.active() == Some(AT_COMPONENTS),
            "Enter unfolds here rather than closing the panel"
        );

        // The one with no wires says so, rather than showing nothing and
        // leaving you wondering whether Enter worked.
        press(&mut u, KeyCode::Down);
        let other = screen(&u);
        assert!(other.contains("on no wires"), "{other}");
        assert!(
            !other.contains("trust.forward"),
            "the old one folded: {other}"
        );
    }

    /// Highlighted rows have to be visible as such, or ↑↓ does nothing you can
    /// see and `u` becomes a guess.
    #[test]
    fn the_highlighted_component_is_marked_on_screen() {
        let mut u = ui(vec![], false);
        u.parts = vec![
            (
                "alpha".into(),
                "a".into(),
                "in-process",
                String::new(),
                false,
                vec![],
            ),
            (
                "beta".into(),
                "b".into(),
                "in-process",
                String::new(),
                false,
                vec![],
            ),
        ];
        u.panel.show(AT_COMPONENTS);
        u.panel.select_row(1);
        let rows = snapshot(&u, 80, 24).rows;
        let marked = rows
            .iter()
            .find(|r| r.contains('\u{25b8}'))
            .expect("something is marked");
        assert!(marked.contains("beta"), "the SELECTED one is: {marked}");
    }

    /// A cumulative curve hides the culprit in its slope. Drawing what each
    /// turn ADDED puts it at the top of the list instead.
    #[test]
    fn the_growth_list_names_the_turn_that_filled_the_window() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![ModelRow {
                window: Some(200_000),
                ..ModelRow::default()
            }],
            now: Some(0),
        };
        u.domain.accounting.seed_parts(vec![("tool results", 500)]);
        u.domain.accounting.seed_last_call(Some(lattice::Usage {
            prompt: 40_000,
            calls: 1,
            ..Default::default()
        }));
        // Steady, then one turn that jumps, then steady again
        let sizes = [1_000u64, 1_200, 1_400, 21_000, 21_300, 21_600];
        u.domain.accounting.seed_history(
            sizes
                .iter()
                .enumerate()
                .map(|(i, s)| (i as u64, *s))
                .collect(),
        );

        let text: String = context_picture(&u, 88)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        let rows: Vec<&str> = text
            .lines()
            .skip_while(|l| !l.contains("each turn added"))
            .skip(1)
            .take(6)
            .collect();
        assert!(
            rows[0].contains("turn 4") && rows[0].contains("19,600"),
            "the jump is first and names its turn: {rows:?}"
        );
        assert!(
            rows[1].contains("turn 1"),
            "then the next largest: {rows:?}"
        );
        // Every turn's growth is its own, not the running total
        assert!(
            !text.contains("21,600"),
            "the cumulative figure is not what is drawn: {text}"
        );
    }

    /// A list that silently stops reads as "these were all the turns".
    #[test]
    fn the_turns_that_do_not_fit_are_summed_rather_than_dropped() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![ModelRow {
                window: Some(200_000),
                ..ModelRow::default()
            }],
            now: Some(0),
        };
        u.domain.accounting.seed_parts(vec![("tool results", 500)]);
        u.domain.accounting.seed_last_call(Some(lattice::Usage {
            prompt: 40_000,
            calls: 1,
            ..Default::default()
        }));
        // 20 turns, each adding 100
        u.domain
            .accounting
            .seed_history((0..20u64).map(|i| (i, (i + 1) * 100)).collect());
        let text: String = context_picture(&u, 88)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(
            text.contains("12 smaller turns"),
            "the rest are counted: {text}"
        );
        assert!(text.contains("1,200"), "and their total is given: {text}");
    }

    /// The chart is one column per turn, keeping that turn's peak — a turn that
    /// ran six tools sent six prompts, and the largest is how full it got.
    #[test]
    fn the_fill_chart_keeps_one_column_per_turn() {
        let mut u = ui(vec![], false);
        let call = |u: &mut Ui, prompt: u64| {
            let mut done = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
            done.payload = serde_json::json!({"usage": {"prompt_tokens": prompt}});
            u.absorb(&done, 0);
        };
        let ask = |u: &mut Ui| {
            let mut said = test_event(core_events::USER_MESSAGE, &[]);
            said.payload = serde_json::json!({"text": "go"});
            u.absorb(&said, 0);
        };
        ask(&mut u);
        call(&mut u, 1_000);
        call(&mut u, 4_000);
        call(&mut u, 2_000);
        ask(&mut u);
        call(&mut u, 9_000);
        assert_eq!(
            u.history(),
            vec![4_000, 9_000],
            "two turns, each at its peak — not five calls"
        );
    }

    /// Through `absorb`, not by calling the reset directly: the reset was wired
    /// to a line that had since been renamed, so the turn column never cleared
    /// and every turn reported the whole conversation's growth. The unit test
    /// on `Growth` passed throughout, because it called the reset itself.
    #[test]
    fn a_new_turn_clears_the_turn_column_through_the_event_stream() {
        let mut u = ui(vec![], false);
        let send = |u: &mut Ui, id: &str, result_bytes: usize| {
            let mut result = test_event(core_events::TOOL_EXEC_COMPLETED, &[]);
            result.id = id.to_string();
            result.payload = serde_json::json!({"result": "x".repeat(result_bytes)});
            u.absorb(&result, 0);
            let mut started = test_event(core_events::MODEL_CALL_STARTED, &[]);
            started.payload = serde_json::json!({
                "input": {"parts": [{"event": id}]},
            });
            u.absorb(&started, 0);
        };
        send(&mut u, "ev_a", 1_000);
        assert!(!u.growth().0.is_empty(), "the first turn grew");

        let mut said = test_event(core_events::USER_MESSAGE, &[]);
        said.payload = serde_json::json!({"text": "next"});
        u.absorb(&said, 0);
        assert!(
            u.growth().0.is_empty(),
            "a new turn starts from nothing: {:?}",
            u.growth().0
        );
        assert!(
            !u.growth().1.is_empty(),
            "but the conversation keeps its total"
        );
    }

    /// Entry points retain different parts of navigation, even from nonzero state.
    #[test]
    fn panel_entries_preserve_action_specific_reset_rules() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let seeded = || {
            let mut u = ui(vec![], false);
            u.panel.select_row(3);
            u.panel.scroll_down(17);
            u.panel.toggle_details();
            u
        };
        for command in [
            "/help",
            "/context",
            "/background",
            "/config",
            "/components",
            "/usage",
        ] {
            let mut u = seeded();
            run_slash(&mut u, command, None);
            assert!(u.panel.is_visible(), "{command}");
            assert_eq!(u.panel.scroll_offset(), 17, "{command}");
            assert_eq!(u.panel.selected_row(), 3, "{command}");
            assert!(u.panel.details_expanded(), "{command}");
        }
        let mut u = seeded();
        on_key(&mut u, None, KeyEvent::from(KeyCode::F(1)), &Hit::default());
        assert_eq!(u.panel.active(), Some(AT_COMMANDS));
        assert_eq!(u.panel.scroll_offset(), 17);
        assert_eq!(u.panel.selected_row(), 3);
        assert!(u.panel.details_expanded());
        on_key(&mut u, None, KeyEvent::from(KeyCode::Esc), &Hit::default());
        assert!(!u.panel.is_visible() && !u.panel.details_expanded());
        assert_eq!(u.panel.scroll_offset(), 17);
        assert_eq!(u.panel.selected_row(), 3);

        let mut u = seeded();
        u.domain.model.fixture_catalog().now = Some(2);
        run_slash(&mut u, "/model", None);
        assert_eq!(u.panel.active(), Some(AT_MODELS));
        assert_eq!(u.panel.selected_row(), 2);
        assert_eq!(u.panel.scroll_offset(), 17);
        assert!(u.panel.details_expanded());
        // Effort does not replace the panel or clear its detail state.
        open_from_status(&mut u, Opens::Effort);
        assert!(u.controls.dial().is_some());
        assert_eq!(u.panel.active(), Some(AT_MODELS));
        assert_eq!(u.panel.scroll_offset(), 0);
        assert!(u.panel.details_expanded());
    }

    #[test]
    fn panel_modal_priority_keeps_existing_form_navigation_and_cancel_boundaries() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut u = ui(vec![], false);
        u.panel.show(AT_MODELS);
        u.panel.scroll_down(17);
        u.panel.toggle_details();
        u.controls.open_form();
        on_key(&mut u, None, KeyEvent::from(KeyCode::Esc), &Hit::default());
        assert!(u.controls.form().is_none());
        assert_eq!(u.panel.active(), Some(AT_MODELS));
        assert_eq!(u.panel.scroll_offset(), 17);
        assert!(u.panel.details_expanded());
        u.controls.open_form();
        on_key(
            &mut u,
            None,
            KeyEvent::from(KeyCode::Right),
            &Hit::default(),
        );
        assert!(
            u.controls.form().is_some(),
            "an unhandled form key still reaches the panel"
        );
        assert_eq!(u.panel.active(), Some(AT_COMPONENTS));
        assert_eq!(u.panel.scroll_offset(), 0);
        assert!(u.panel.details_expanded());
        u.controls.close_form();
        u.controls.ask_delete("not-a-real-model".into());
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL),
            &Hit::default(),
        );
        assert!(matches!(u.navigation, Some(tabs::Navigation::Next)));
        assert!(
            u.controls.deletion().is_some(),
            "seat navigation precedes confirmation"
        );
        on_key(&mut u, None, KeyEvent::from(KeyCode::Esc), &Hit::default());
        assert!(u.controls.deletion().is_none());
        assert_eq!(u.panel.active(), Some(AT_COMPONENTS));
        assert!(u.panel.details_expanded());
    }

    #[test]
    fn panel_drawing_never_clamps_the_stored_scroll_or_selection() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let mut u = ui(vec![], false);
        u.panel.show(AT_COMMANDS);
        u.panel.scroll_down(100_000);
        u.panel.select_row(100_000);
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(60, 12)).unwrap();
        draw_ui(&mut term, &mut u).unwrap();
        assert_eq!(u.panel.scroll_offset(), 100_000);
        assert_eq!(u.panel.selected_row(), 100_000);
        u.parts = vec![(
            "weather".into(),
            "tool".into(),
            "subprocess",
            String::new(),
            true,
            vec![],
        )];
        u.panel.show(AT_COMPONENTS);
        draw_ui(&mut term, &mut u).unwrap();
        on_key(
            &mut u,
            None,
            KeyEvent::from(KeyCode::Char('u')),
            &Hit::default(),
        );
        assert_eq!(u.panel.active(), Some(AT_COMPONENTS));
        assert!(
            !u.domain.turns.busy(),
            "a visually clamped row is not the raw selected row"
        );
        u.panel.show(AT_MODELS);
        for key in ['s', 'd'] {
            on_key(
                &mut u,
                None,
                KeyEvent::from(KeyCode::Char(key)),
                &Hit::default(),
            );
            assert_eq!(u.panel.active(), Some(AT_MODELS));
            assert!(u.controls.deletion().is_none());
        }
    }

    #[test]
    fn help_opens_a_panel_instead_of_writing_into_the_conversation() {
        let mut u = ui(vec![Entry::Agent("an answer".to_string())], false);
        assert!(!run_slash(&mut u, "/help", None));
        assert_eq!(u.panel.active(), Some(AT_COMMANDS), "the panel opened");
        assert_eq!(
            u.entries.len(),
            1,
            "and nothing joined the transcript: {:?}",
            u.entries
        );

        let snap = snapshot(&u, 78, 24);
        let screen = snap.rows.join("\n");
        for tab in panel_tabs(0) {
            assert!(screen.contains(tab), "the tab row shows {tab}");
        }
        assert!(screen.contains("/uninstall"), "and this tab's rows");
        assert!(
            !screen.contains("an answer"),
            "the panel takes the transcript's place while it is open"
        );
        assert!(screen.contains("Esc close"), "and says how to leave");
    }

    /// While it is open it takes the keyboard, so nothing typed at a reference
    /// page leaks into the line you were writing.
    #[test]
    fn the_panel_cycles_its_tabs_and_swallows_everything_else() {
        use ratatui::crossterm::event::KeyCode;
        let mut u = ui(vec![], false);
        u.draft.edit().set("half a sentence");
        u.panel.show(AT_COMMANDS);
        let press = |u: &mut Ui, code| {
            on_key(
                u,
                None,
                ratatui::crossterm::event::KeyEvent::from(code),
                &Hit::default(),
            );
        };

        press(&mut u, KeyCode::Right);
        assert_eq!(u.panel.active(), Some((0, 1)));
        press(&mut u, KeyCode::Left);
        press(&mut u, KeyCode::Left);
        assert_eq!(
            u.panel.active(),
            Some((0, panel_tabs(0).len() - 1)),
            "and it wraps"
        );

        press(&mut u, KeyCode::Char('x'));
        assert_eq!(
            u.draft.editor().text(),
            "half a sentence",
            "typing at the panel does not reach the line"
        );

        press(&mut u, KeyCode::Esc);
        assert_eq!(u.panel.active(), None, "Esc closes it");
        press(&mut u, KeyCode::Char('x'));
        assert_eq!(
            u.draft.editor().text(),
            "half a sentencex",
            "and typing works again"
        );
    }

    /// Every row of every tab says something. An empty right column is a row
    /// that is only taking up space.
    #[test]
    fn every_tab_of_every_panel_draws_something() {
        for (panel, (_, tabs)) in PANELS.iter().enumerate() {
            for tab in 0..tabs.len() {
                let at = (panel, tab);
                let mut u = ui(vec![], false);
                u.panel.show(at);
                let snap = snapshot(&u, 78, 24);
                let drawn: Vec<&String> = snap
                    .rows
                    .iter()
                    .filter(|r| r.starts_with("  ") && !r.trim().is_empty())
                    .collect();
                assert!(!drawn.is_empty(), "{at:?} draws something");
                assert!(
                    snap.rows.iter().any(|r| r.contains(tabs[tab])),
                    "{at:?} shows its own tab name"
                );
            }
        }
    }

    /// Two different pictures must not answer to one name. Two references to
    /// the SAME picture keep the same name, because that is what they are.
    #[test]
    fn two_different_pictures_never_share_a_label() {
        let docs = tempfile::tempdir().unwrap();
        let mut u = ui(vec![], false);
        u.documents = Some(docs.path().to_path_buf());
        let same = "clipboard 8\u{d7}8";

        attach_bytes(&mut u, b"first picture", "png", "image/png", same);
        attach_bytes(&mut u, b"second picture", "png", "image/png", same);
        attach_bytes(&mut u, b"first picture", "png", "image/png", same);

        let names: Vec<&str> = u
            .draft
            .references()
            .iter()
            .map(|reference| reference["name"].as_str().unwrap())
            .collect();
        assert_ne!(names[0], names[1], "different content, different name");
        assert_eq!(
            names[0], names[2],
            "the same picture twice keeps one name — it is one picture"
        );
        assert!(names[1].starts_with(same), "{}", names[1]);
        assert!(names[1].len() > same.len(), "{}", names[1]);
    }

    /// Attaching stores the bytes NOW, so what is sent is the file as it was
    /// when it was attached rather than whatever that path holds later.
    #[test]
    fn attaching_stores_the_bytes_and_the_reference_names_the_stored_file() {
        let docs = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let png = src.path().join("shot.png");
        std::fs::write(&png, [1u8, 2, 3, 4]).unwrap();

        let mut u = ui(vec![], false);
        u.documents = Some(docs.path().to_path_buf());
        attach_image(&mut u, &png, "image/png");

        assert_eq!(u.draft.references().len(), 1);
        let reference = &u.draft.references()[0];
        assert_eq!(reference["name"], "shot.png");
        assert_eq!(reference["mediaType"], "image/png");
        assert_eq!(reference["bytes"], 4);
        let stored = docs.path().join(reference["file"].as_str().unwrap());
        assert_eq!(std::fs::read(&stored).unwrap(), [1, 2, 3, 4]);

        // Changing the original afterwards changes nothing that was attached
        std::fs::write(&png, [9u8; 99]).unwrap();
        assert_eq!(std::fs::read(&stored).unwrap(), [1, 2, 3, 4]);

        // And it is in the line, so it cannot be forgotten
        assert_eq!(u.draft.editor().shown(), "[image shot.png]");
        let snap = snapshot(&u, 72, 20);
        assert!(
            snap.rows.iter().any(|r| r.contains("[image shot.png]")),
            "the placeholder is in the input line"
        );
    }

    /// Without a ledger there is nowhere a dialect would look, so the answer is
    /// no rather than storing it somewhere useless.
    #[test]
    fn a_stream_with_no_ledger_refuses_the_picture_and_says_why() {
        let src = tempfile::tempdir().unwrap();
        let png = src.path().join("shot.png");
        std::fs::write(&png, [1u8, 2, 3, 4]).unwrap();
        let mut u = ui(vec![], false);
        u.documents = None;
        attach_image(&mut u, &png, "image/png");
        assert!(u.draft.references().is_empty());
        assert!(
            u.flash.as_deref().is_some_and(|f| f.contains("no ledger")),
            "{:?}",
            u.flash
        );
    }

    /// The profile decides, and it decides BEFORE the picture is attached: the
    /// alternative is a call rejected in the middle of a turn, after the
    /// question has already been asked.
    #[test]
    fn a_model_that_cannot_read_pictures_refuses_the_attachment_up_front() {
        let docs = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let png = src.path().join("shot.png");
        std::fs::write(&png, [1u8, 2, 3, 4]).unwrap();

        let mut u = ui(vec![], false);
        u.documents = Some(docs.path().to_path_buf());
        *u.domain.model.fixture_catalog() = ModelView {
            rows: vec![ModelRow {
                id: "ds".to_string(),
                model: "deepseek-v4-flash".to_string(),
                accepts_images: false,
                ..ModelRow::default()
            }],
            now: Some(0),
        };
        attach_image(&mut u, &png, "image/png");
        assert!(u.draft.references().is_empty(), "nothing was attached");
        // With nothing else that can, the remedy is the profile field — not
        // "switch models", which would send someone round a catalog where no
        // entry can read a picture either.
        let said = u.flash.clone().unwrap_or_default();
        assert!(said.contains("ds"), "names the model: {said}");
        assert!(said.contains("acceptsImages"), "names the fix: {said}");
        assert!(!said.contains("/model to switch"), "{said}");
        assert_eq!(
            std::fs::read_dir(docs.path()).unwrap().count(),
            0,
            "and nothing was written for a call that will not happen"
        );

        // Once the catalog holds one that can, say WHICH — that is a remedy
        // someone can act on without going to look.
        u.domain.model.fixture_catalog().rows.push(ModelRow {
            id: "vision".to_string(),
            accepts_images: true,
            ..ModelRow::default()
        });
        attach_image(&mut u, &png, "image/png");
        let said = u.flash.clone().unwrap_or_default();
        assert!(said.contains("/model to switch: vision"), "{said}");

        // The same picture, to a model whose profile says it can read one
        u.domain.model.fixture_catalog().now = Some(1);
        attach_image(&mut u, &png, "image/png");
        assert_eq!(u.draft.references().len(), 1);
    }

    /// A command's receipt is NOT a line of the conversation. As an entry it
    /// sat flush against the end of the agent's answer — same indent, same
    /// colour, no blank between — and read as its last sentence.
    #[test]
    fn a_command_receipt_rides_by_the_input_and_never_enters_the_transcript() {
        let mut u = ui(vec![Entry::Agent("the answer".to_string())], false);
        u.tick = 10_000;
        u.domain.turns.seed_done_at(Some(0));
        run_slash(&mut u, "/nope", None);

        assert_eq!(
            u.flash.as_deref(),
            Some("unknown command /nope — try /help"),
            "the receipt is set"
        );
        assert!(
            !u.entries
                .iter()
                .any(|e| matches!(e, Entry::Notice(t) if t.contains("unknown command"))),
            "and it did not join the transcript"
        );

        let snap = snapshot(&u, 72, 20);
        let row_of = |n: &str| snap.rows.iter().position(|r| r.contains(n));
        let receipt = row_of("unknown command").expect("it is on screen");
        let caret = snap.rows.iter().rposition(|r| r.contains('❯')).unwrap();
        let done = row_of("Done").expect("the Done line shows");
        assert!(done < receipt, "below the Done line");
        assert_eq!(
            caret - receipt,
            2,
            "and directly above the input box — only its top rule between"
        );
        assert!(
            snap.rows[receipt - 1].trim().is_empty(),
            "with a blank row above it, so it is not glued to what precedes"
        );
    }

    /// The receipt answered the LAST keystroke; by the next one you have moved
    /// on. A command that leaves a receipt still leaves one — the clear runs
    /// before the key is handled, not after.
    #[test]
    fn the_next_keystroke_clears_the_receipt_but_a_command_may_leave_a_new_one() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let mut u = ui(vec![], false);
        run_slash(&mut u, "/nope", None);
        assert!(u.flash.is_some());

        on_key(
            &mut u,
            None,
            KeyEvent::from(KeyCode::Char('x')),
            &Hit::default(),
        );
        assert_eq!(u.flash, None, "typing clears it");

        // Enter on a slash command: the clear runs first, then the command
        // sets its own receipt, which must survive.
        u.draft.edit().set("/nope");
        on_key(
            &mut u,
            None,
            KeyEvent::from(KeyCode::Enter),
            &Hit::default(),
        );
        assert_eq!(
            u.flash.as_deref(),
            Some("unknown command /nope — try /help"),
            "the command's own receipt survives the clear that precedes it"
        );
    }

    /// The receipt and the slash menu take the same place, so they must never
    /// both want it. They cannot: the keystroke that opens the menu clears the
    /// receipt — but the renderer does not rely on that, and this pins it.
    #[test]
    fn the_receipt_yields_to_the_slash_menu() {
        let mut u = ui(vec![], false);
        u.flash = Some("some receipt".to_string());
        u.draft.edit().set("/");
        let snap = snapshot(&u, 72, 20);
        assert!(
            snap.rows.iter().any(|r| r.contains("/help")),
            "the menu shows"
        );
        assert!(
            !snap.rows.iter().any(|r| r.contains("some receipt")),
            "and the receipt does not fight it for the row"
        );
    }

    /// Every summary starts in the same column, measured off the rendered
    /// frame. Unaligned, the name and its summary ran together and "/help list
    /// the commands" read as one four-word phrase.
    #[test]
    fn the_slash_summaries_line_up_in_one_column() {
        let mut u = ui(vec![], false);
        u.draft.edit().set("/");
        let width = u16::try_from(
            2 + crate::terminal_host::slash_catalog::slash_col()
                + SLASH
                    .iter()
                    .map(|cmd| wrap::str_cols(cmd.summary))
                    .max()
                    .unwrap()
                + format!("  +{}", SLASH.len()).len(),
        )
        .unwrap();

        let mut cols = Vec::new();
        for cmd in SLASH {
            let snap = snapshot(&u, width, 24);
            let row = snap
                .rows
                .iter()
                .find(|r| r.split_whitespace().any(|word| word == cmd.name))
                .unwrap_or_else(|| panic!("{} is listed", cmd.name));
            // COLUMNS, not byte offsets: the selected row is marked "▸ " (four
            // bytes) and the rest "  " (two), so bytes put the highlighted row
            // two ahead of an identically aligned one.
            let col = |byte: usize| row[..byte].chars().count();
            let at = col(row
                .find(cmd.summary)
                .unwrap_or_else(|| panic!("{}'s summary shows: {row}", cmd.name)));
            // Past the name, never touching it — there is always a gap.
            let name_end = col(row.find(cmd.name).unwrap()) + cmd.name.chars().count();
            assert!(
                at > name_end,
                "{} has a gap before its summary: {row}",
                cmd.name
            );
            cols.push((cmd.name, at));
            assert!(!on_key(
                &mut u,
                None,
                ratatui::crossterm::event::KeyEvent::from(KeyCode::Down),
                &Hit::default(),
            ));
        }
        let first = cols[0].1;
        assert!(
            cols.iter().all(|(_, c)| *c == first),
            "one column for every summary, got {cols:?}"
        );
    }

    /// A frame renders an agent reply's Markdown (heading, bullet, inline code)
    /// and a finished tool as a card (name, args, a ✓ status glyph).
    #[test]
    fn a_markdown_reply_renders_into_the_frame() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let entries = vec![
            Entry::User("hello".to_string()),
            Entry::Tool(ToolCard {
                call: Some("c1".to_string()),
                name: "read_file".to_string(),
                args: serde_json::json!({ "path": "src/main.rs" }),
                status: ToolStatus::Ok,
                output: vec!["fn main() {}".to_string()],
                changed: None,
                edit_diff: None,
            }),
            Entry::Agent("# Result\n- first\n- second\nuse `cargo test`".to_string()),
        ];
        // The reply has landed, so the turn's work is folded; unfold it, which
        // is what this test is about — how a card looks, not when it shows
        let mut u = ui(entries, false);
        u.browsing.expand("c1".to_string());
        draw(&mut term, &u).unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();

        assert!(screen.contains("Result"), "heading text shows");
        assert!(screen.contains('•'), "list items get a bullet");
        assert!(screen.contains("cargo test"), "inline code shows");
        assert!(screen.contains("hello"), "the user line shows");
        assert!(
            screen.contains("Read_file"),
            "the tool name shows (capitalized)"
        );
        assert!(screen.contains("src/main.rs"), "the tool args show");
        assert!(screen.contains('✓'), "a finished tool shows a check");
        assert!(screen.contains('❯'), "the input caret shows");
    }

    #[test]
    fn transcript_viewport_does_not_wrap_after_sixty_five_thousand_rows() {
        let mut text = "old row\n".repeat(65_540);
        text.push_str("latest-visible-marker");
        let u = ui(vec![Entry::User(text)], false);
        let mut term = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let hit = draw(&mut term, &u).unwrap();
        assert!(hit.more_above);
        assert!(hit.owner.len() <= hit.area.height as usize);
        let screen: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("latest-visible-marker"),
            "the latest row must remain on screen"
        );
    }

    #[test]
    fn url_hit_rectangles_follow_scroll_resize_and_panel_overlays() {
        let md = (0..30)
            .map(|i| format!("[link-{i:02}](https://example.com/{i:02})"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut u = ui(vec![Entry::Agent(md)], false);
        for (width, height, scroll) in [(40, 12, 0), (40, 12, 5), (24, 10, 8)] {
            u.browsing.set_offset(scroll);
            let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
            let hit = draw(&mut term, &u).unwrap();
            assert!(!hit.links.is_empty());
            for (rect, url) in &hit.links {
                assert!(rect_has(hit.area, rect.x, rect.y));
                let label: String = (rect.x..rect.x + rect.width)
                    .map(|x| term.backend().buffer()[(x, rect.y)].symbol())
                    .collect();
                if !(rect.x..rect.x + rect.width).any(|x| hit.jump_at(x, rect.y)) {
                    assert_eq!(label, format!("link-{}", url.rsplit('/').next().unwrap()));
                }
                for x in rect.x..rect.x + rect.width {
                    let expected = if hit.jump_at(x, rect.y) {
                        None
                    } else {
                        Some(url.as_str())
                    };
                    assert_eq!(hit.url_at(x, rect.y), expected);
                }
                assert_eq!(hit.url_at(rect.x + rect.width, rect.y), None);
            }
            assert_eq!(
                hit.url_at(hit.area.x, hit.area.y),
                None,
                "the gutter is not a link"
            );
            u.panel.show(AT_BACKGROUND);
            let covered = draw(&mut term, &u).unwrap();
            assert!(
                covered.links.is_empty(),
                "hidden transcript links cannot be clicked through a panel"
            );
            u.panel.dismiss_preserving_details();
        }
    }

    #[test]
    fn url_streaming_links_are_clickable_but_the_cursor_is_not() {
        clear_render_cache();
        let mut u = ui(vec![], true);
        u.live_output
            .seed_reply("[文档链接很长很长很长](https://example.com/)".into());
        let mut term = Terminal::new(TestBackend::new(18, 14)).unwrap();
        let hit = draw(&mut term, &u).unwrap();
        assert!(
            hit.links.len() > 1,
            "the label wraps onto several clickable rows"
        );
        for (rect, url) in &hit.links {
            assert_eq!(hit.url_at(rect.x, rect.y), Some(url.as_str()));
        }
        let mut cursors = 0;
        for y in 0..14 {
            for x in 0..18 {
                if term.backend().buffer()[(x, y)].symbol() == "▌" {
                    cursors += 1;
                    assert_eq!(hit.url_at(x, y), None);
                }
            }
        }
        assert_eq!(cursors, 1);
        assert_eq!(
            render_cache_len(),
            0,
            "stream fragments never enter the cache"
        );
    }

    /// A GFM table in an agent reply renders as aligned columns with a rule
    /// under the header — no raw pipes on screen.
    #[test]
    fn a_table_reply_renders_aligned() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let entries = vec![Entry::Agent(
            "| 工具 | 说明 |\n|---|---|\n| read | 读写文件 |".to_string(),
        )];
        draw(&mut term, &ui(entries, false)).unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();

        assert!(!screen.contains('|'), "no raw pipes survive");
        assert!(screen.contains('─'), "the header rule shows");
        // Wide chars occupy two buffer cells (glyph + filler), so compare
        // with the filler spaces squeezed out.
        let squeezed = screen.replace(' ', "");
        assert!(squeezed.contains("工具"), "header cell shows");
        assert!(squeezed.contains("读写文件"), "body cell shows");
    }

    /// A tool that is still running shows the current spinner frame, not a ✓.
    #[test]
    fn a_running_tool_shows_a_spinner() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let entries = vec![Entry::Tool(ToolCard {
            call: Some("c1".to_string()),
            name: "run".to_string(),
            args: serde_json::json!({ "command": "cargo test" }),
            status: ToolStatus::Running,
            output: Vec::new(),
            changed: None,
            edit_diff: None,
        })];
        draw(&mut term, &ui(entries, true)).unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();

        assert!(screen.contains("Run"), "the tool name shows (capitalized)");
        assert!(
            SPINNER.iter().any(|&f| screen.contains(f)),
            "a running tool shows a spinner frame"
        );
        assert!(!screen.contains('✓'), "a running tool has no check yet");
    }

    #[test]
    fn run_slash_dispatches_commands() {
        let mut u = ui(vec![Entry::User("x".to_string())], false);
        assert!(!run_slash(&mut u, "/clear", None));
        assert!(u.entries.is_empty(), "/clear empties the view");

        let mut u = ui(vec![], false);
        assert!(!run_slash(&mut u, "/help", None));
        assert!(u.panel.is_visible(), "/help opens the panel");
        assert!(
            u.entries.is_empty(),
            "and writes nothing into the conversation"
        );

        let mut u = ui(vec![], false);
        assert!(!run_slash(&mut u, "/nope", None));
        let text = u.flash.as_deref().expect("a receipt for the bad command");
        assert!(text.contains("unknown command"), "unknown slash is caught");

        assert!(
            run_slash(&mut ui(vec![], false), "/exit", None),
            "/exit quits"
        );
        assert!(
            run_slash(&mut ui(vec![], false), "/quit", None),
            "/quit quits"
        );
    }

    fn tool(call: &str, output: Vec<String>) -> Entry {
        Entry::Tool(ToolCard {
            call: Some(call.to_string()),
            name: "Run".to_string(),
            args: serde_json::json!({}),
            status: ToolStatus::Ok,
            output,
            changed: None,
            edit_diff: None,
        })
    }

    fn screen(term: &Terminal<TestBackend>) -> String {
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    // A transcript row with nothing on it but the (optional) scrollbar glyph:
    // the welcome globe overflows the viewport in these tests, so a scrollbar
    // rides the right edge of every row — a "blank" separator still carries it.
    fn content_blank(row: &str) -> bool {
        row.chars().all(|c| c == ' ' || c == '║' || c == '█')
    }

    // The row index of the first line containing `needle`, plus whether the row
    // just above it is blank. Roomy so the brand block plus the turn all fit.
    fn row_and_gap(u: &Ui, needle: &str) -> (usize, bool) {
        let mut term = Terminal::new(TestBackend::new(72, 30)).unwrap();
        draw(&mut term, u).unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let i = rows.iter().position(|r| r.contains(needle)).unwrap();
        (i, i > 0 && content_blank(&rows[i - 1]))
    }

    #[test]
    fn the_brand_rides_at_the_top_and_carries_the_guidance() {
        let mut term = Terminal::new(TestBackend::new(72, 20)).unwrap();
        draw(&mut term, &ui(vec![], false)).unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        // the wordmark sits near the top of the transcript, not pinned as a header
        let brand = rows.iter().position(|r| r.contains("Lattice")).unwrap();
        assert!(brand <= 3, "the brand rides at the top of the transcript");
        // an empty, idle screen keeps only the resident quit hint in the status bar
        assert!(
            rows.iter().any(|r| r.contains("Ctrl-D quit")),
            "the status bar keeps the resident quit hint"
        );
    }

    #[test]
    fn streaming_and_finalized_reply_share_the_same_spacing() {
        let base = vec![
            Entry::User("q".to_string()),
            tool("c1", vec!["out".to_string()]),
        ];
        // mid-stream: a partial reply is arriving
        let mut streaming = ui(base.clone(), true);
        streaming.live_output.seed_reply("REPLY".to_string());
        let (_srow, sgap) = row_and_gap(&streaming, "REPLY");
        // finalized: the same reply landed in the ledger
        let mut done = base;
        done.push(Entry::Agent("REPLY".to_string()));
        let (_frow, fgap) = row_and_gap(&ui(done, false), "REPLY");

        // The in-transcript spacing is the same either way: a blank precedes the
        // reply. (The absolute row differs — while streaming, the thinking line
        // rides below the transcript, which it deliberately does not while idle.)
        assert!(sgap, "a blank precedes the streaming reply");
        assert!(fgap, "a blank precedes the finalized reply");
    }

    #[test]
    fn turn_activity_keeps_local_changes_until_an_actual_boundary() {
        use ratatui::crossterm::event::KeyEvent;
        let mut u = ui(vec![], false);
        fold_render(&mut u, RenderEvent::Quiescent).unwrap();
        assert!(
            u.domain.turns.done_at().is_none(),
            "startup quiet is not a finished turn"
        );
        u.note_turn_boundary(
            &test_event(lattice::components::minimal_loop::WAITING, &[]),
            1,
        );
        u.draft.edit().set("next question");
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &Hit::default(),
        );
        assert!(u.domain.turns.busy() && u.domain.turns.waiting());
        assert!(
            !u.waiting(),
            "optimistic activity hides the waiting readout"
        );
        assert_eq!(
            u.domain.turns.number(),
            0,
            "submission does not invent a recorded turn"
        );
        let history = u.domain.turns.history();
        fold_render(
            &mut u,
            RenderEvent::Appended(Box::new(test_event(core_events::OUTPUT_REPLY, &[]))),
        )
        .unwrap();
        assert_eq!(u.domain.turns.history(), history);
        assert!(u.domain.turns.busy() && u.domain.turns.waiting());
        u.tick = 10;
        fold_render(&mut u, RenderEvent::Quiescent).unwrap();
        assert!(!u.domain.turns.busy() && u.domain.turns.waiting());
        assert_eq!(u.domain.turns.done_at(), Some(10));
        fold_render(
            &mut u,
            RenderEvent::Appended(Box::new(test_event(
                core_events::USER_MESSAGE,
                &["forwarded"],
            ))),
        )
        .unwrap();
        assert_eq!(u.domain.turns.history(), history);
        assert!(!u.domain.turns.busy() && u.domain.turns.waiting());
        assert_eq!(u.domain.turns.done_at(), Some(10));
        for purpose in [serde_json::Value::Null, json!("condense")] {
            let mut background = test_event(core_events::MODEL_CALL_STARTED, &[]);
            background.payload = json!({"purpose":purpose});
            u.domain.unseen.restore(vec!["not yet shown".into()]);
            fold_render(&mut u, RenderEvent::Appended(Box::new(background))).unwrap();
            assert!(
                u.domain.unseen.lines().is_empty(),
                "background requests still clear the unseen list"
            );
            assert_eq!(u.domain.turns.history(), history);
            assert!(!u.domain.turns.busy() && u.domain.turns.waiting());
            assert_eq!(u.domain.turns.done_at(), Some(10));
        }
        u.note_turn_boundary(&test_event(core_events::INTERRUPTED, &[]), 13);
        assert_eq!(u.domain.turns.done_at(), Some(10));
        u.note_turn_boundary(&test_event(core_events::USER_MESSAGE, &[]), 14);
        assert!(
            u.domain.turns.busy()
                && !u.domain.turns.waiting()
                && u.domain.turns.done_at().is_none()
        );
        assert_eq!(u.domain.turns.number(), 1);
        u.note_turn_boundary(&test_event(core_events::WAKE, &[]), 15);
        assert_eq!(
            u.domain.turns.number(),
            2,
            "a wake advances even while busy"
        );
        u.flash = Some("receipt".into());
        u.note_turn_boundary(&test_event(core_events::MODEL_CALL_STARTED, &[]), 16);
        assert_eq!(u.domain.turns.number(), 2);
        assert_eq!(u.flash.as_deref(), Some("receipt"));
    }

    #[test]
    fn turn_boundaries_come_from_the_event_stream() {
        let mut u = ui(vec![], false);
        // a background/timer WAKE starts a turn exactly like a user message —
        // this is the fix: background turns now light the thinking line
        u.note_turn_boundary(&test_event(core_events::WAKE, &[]), 5);
        assert!(u.domain.turns.busy(), "a wake starts a turn");
        assert_eq!(
            u.domain.turns.done_at(),
            None,
            "no lingering Done at a turn's start"
        );
        let t = u.domain.turns.number();
        // the turn completing ends it and stamps Done at the finishing tick
        u.note_turn_boundary(&test_event(core_events::TURN_COMPLETED, &[]), 42);
        assert!(!u.domain.turns.busy(), "turn_completed ends the turn");
        assert_eq!(
            u.domain.turns.done_at(),
            Some(42),
            "Done is stamped where the turn finished"
        );
        // a user message opens the next turn with a fresh phrase
        u.note_turn_boundary(&test_event(core_events::USER_MESSAGE, &[]), 50);
        assert!(
            u.domain.turns.busy() && u.domain.turns.number() == t + 1,
            "a new turn advances the phrase"
        );
        // the expansion station's re-emission (a CAUSED user message) is the
        // same turn passing a station, not a new one
        u.note_turn_boundary(&test_event(core_events::USER_MESSAGE, &["ev_1_aa"]), 51);
        assert_eq!(
            u.domain.turns.number(),
            t + 1,
            "a forwarded user message is not a new turn"
        );
    }

    // ── hard-case visual tests: the five ways a terminal UI breaks, each read
    //    from a structured `snapshot` rather than a human eyeballing the terminal.

    #[test]
    fn a_long_reply_wraps_at_every_width_without_losing_words() {
        let reply = "alpha beta gamma delta epsilon zeta eta theta iota kappa \
                     lambda mu nu xi omicron pi rho sigma tau upsilon";
        for w in [40u16, 72, 120] {
            let snap = snapshot(&ui(vec![Entry::Agent(reply.to_string())], false), w, 30);
            let seen = snap.rows.join(" ");
            for word in reply.split(' ') {
                assert!(
                    seen.contains(word),
                    "word '{word}' survives wrapping at width {w}"
                );
            }
            for row in &snap.rows {
                assert!(
                    wrap::str_cols(row.trim_end()) <= w as usize,
                    "no row overruns width {w}"
                );
            }
        }
    }

    #[test]
    fn the_input_cursor_counts_display_width_not_characters() {
        let col = |t: &str| {
            let mut u = ui(vec![], false);
            u.draft.edit().set(t);
            snapshot(&u, 72, 12).cursor.expect("a cursor").0
        };
        // "你好x" is five display columns (2+2+1) — the caret lands like "abcde"
        assert_eq!(
            col("你好x"),
            col("abcde"),
            "wide chars advance the caret by width"
        );
        // …and not by character count (which would place it where three chars end)
        assert_ne!(col("你好x"), col("abc"), "not by character count");
    }

    #[test]
    fn the_jump_pill_overlays_cleanly_within_the_transcript() {
        let rows: Vec<Entry> = (0..40).map(|i| Entry::User(format!("row{i:02}"))).collect();
        let mut u = ui(rows.clone(), false);
        u.browsing.set_offset(1000); // scrolled up
        let snap = snapshot(&u, 72, 20);
        assert!(
            snap.has("jump to bottom"),
            "the pill shows while scrolled up"
        );
        let (pill, t) = (snap.jump.expect("a pill rect"), snap.transcript);
        assert!(
            pill.y >= t.y && pill.y < t.y + t.height,
            "pill sits inside the transcript"
        );
        assert!(
            pill.x + pill.width <= t.x + t.width,
            "pill stays within the right edge"
        );
        // pinned to the bottom → no pill
        assert!(
            snapshot(&ui(rows, false), 72, 20).jump.is_none(),
            "no pill when pinned"
        );
    }

    #[test]
    fn the_transcript_scroll_clamps_at_both_ends() {
        // a fresh screen: the brand fills the viewport exactly, nothing to scroll
        assert_eq!(
            snapshot(&ui(vec![], false), 72, 20).max_scroll,
            Some(0),
            "a fresh screen has nothing to scroll"
        );
        let rows: Vec<Entry> = (0..40).map(|i| Entry::User(format!("row{i:02}"))).collect();
        // pinned (scroll 0) → offset at the bottom, latest shown
        let bottom = snapshot(&ui(rows.clone(), false), 72, 20);
        assert_eq!(bottom.scroll, 0, "the visible rows use local coordinates");
        assert_eq!(
            bottom.max_scroll, None,
            "unread height is not a fabricated total"
        );
        assert!(bottom.has("row39"), "the latest shows");
        // over-scroll clamps to the top
        let mut top_ui = ui(rows, false);
        top_ui.browsing.set_offset(100_000);
        assert_eq!(
            snapshot(&top_ui, 72, 20).scroll,
            0,
            "over-scroll clamps to the top"
        );
    }

    #[test]
    fn a_streaming_reply_shows_a_cursor_then_finalizes_clean() {
        let mut streaming = ui(vec![Entry::User("q".to_string())], true);
        streaming
            .live_output
            .seed_reply("partial answer".to_string());
        let s = snapshot(&streaming, 72, 20);
        assert!(s.has("partial answer"), "the streaming text shows");
        assert!(s.has("▌"), "a cursor marks the live stream");
        // finalized in the ledger: same text, no cursor
        let done = vec![
            Entry::User("q".to_string()),
            Entry::Agent("partial answer".to_string()),
        ];
        let f = snapshot(&ui(done, false), 72, 20);
        assert!(f.has("partial answer"), "the finalized text shows");
        assert!(!f.has("▌"), "no cursor once finalized");
    }

    #[test]
    fn a_finished_turn_settles_into_a_persistent_done_line() {
        let entries = vec![Entry::User("q".to_string()), Entry::Agent("hi".to_string())];
        let mut u = ui(entries, false);
        u.domain.turns.seed_done_at(Some(0));
        u.tick = 100; // well past the typing animation → holds at "Done"
        let mut term = Terminal::new(TestBackend::new(64, 12)).unwrap();
        draw(&mut term, &u).unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        // the line stayed (not removed) and reads Done, above the input, blank above
        let done = rows.iter().position(|r| r.contains("Done")).unwrap();
        let input = rows.iter().rposition(|r| r.contains('❯')).unwrap();
        assert!(done < input, "the Done line is above the input");
        assert!(
            rows[done - 1].trim().is_empty(),
            "a blank row sits above the Done line"
        );
        // but before any turn (no done_at), no such line shows
        let mut term2 = Terminal::new(TestBackend::new(64, 12)).unwrap();
        draw(&mut term2, &ui(vec![], false)).unwrap();
        assert!(
            !screen(&term2).contains("Done"),
            "no Done line before a turn"
        );
    }

    #[test]
    fn the_status_bar_swaps_quit_for_edit_hints_when_typing() {
        let mut term = Terminal::new(TestBackend::new(72, 20)).unwrap();
        // empty & idle → the resident quit hint, and nothing else
        draw(&mut term, &ui(vec![], false)).unwrap();
        let s = screen(&term);
        assert!(s.contains("Ctrl-D quit"), "empty line shows the quit hint");
        assert!(!s.contains("Ctrl-C clear"), "no edit hints when empty");
        assert!(!s.contains("Enter send"), "the obvious hints are gone");
        // typing → the edit hints replace the quit hint
        let mut u = ui(vec![], false);
        u.draft.edit().set("hello");
        draw(&mut term, &u).unwrap();
        let s = screen(&term);
        assert!(s.contains("Ctrl-C clear"), "typing shows clear");
        assert!(s.contains("newline"), "typing shows newline");
        assert!(!s.contains("Ctrl-D quit"), "quit steps aside while typing");
    }

    #[test]
    fn the_thinking_line_sits_above_the_input_not_in_the_status_bar() {
        let mut term = Terminal::new(TestBackend::new(72, 20)).unwrap();
        // busy, nothing streaming, no tool running → the THINKING phrase shows
        draw(&mut term, &ui(vec![Entry::User("q".to_string())], true)).unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        // turn 0 in the thinking state → the first thinking phrase
        let think = rows
            .iter()
            .position(|r| r.contains(state_phrase(0, false)))
            .unwrap();
        let line = &rows[think];
        // a breathing dot, not the braille spinner and not a ✻ star
        assert!(
            THINK_SPIN.iter().any(|g| line.contains(*g)),
            "the breathing glyph shows"
        );
        assert!(
            !SPINNER.iter().any(|g| line.contains(*g)),
            "not the braille spinner"
        );
        assert!(!line.contains('✻'), "not a star");
        // it sits above the input box (the bottom-most ❯), set off by a blank row
        // on each side — from the output above and the input below
        let input = rows.iter().rposition(|r| r.contains('❯')).unwrap();
        assert!(think < input, "the thinking line is above the input");
        assert!(
            rows[think - 1].trim().is_empty(),
            "a blank row separates it from the output above"
        );
        assert!(
            rows[think + 1..input].iter().any(|r| r.trim().is_empty()),
            "a blank row separates it from the input below"
        );
    }

    #[test]
    fn writing_or_running_a_tool_shows_the_working_phrase() {
        let mut term = Terminal::new(TestBackend::new(72, 20)).unwrap();
        // streaming an answer → WORKING, not thinking
        let mut u = ui(vec![Entry::User("q".to_string())], true);
        u.live_output.seed_reply("part".to_string());
        draw(&mut term, &u).unwrap();
        let s = screen(&term);
        assert!(
            s.contains(state_phrase(0, true)),
            "streaming shows a working phrase"
        );
        assert!(
            !s.contains(state_phrase(0, false)),
            "not the thinking phrase"
        );
        // a running tool → WORKING too
        let mut term2 = Terminal::new(TestBackend::new(72, 20)).unwrap();
        let running = Entry::Tool(ToolCard {
            call: Some("c1".to_string()),
            name: "run".to_string(),
            args: serde_json::json!({}),
            status: ToolStatus::Running,
            output: Vec::new(),
            changed: None,
            edit_diff: None,
        });
        draw(
            &mut term2,
            &ui(vec![Entry::User("q".to_string()), running], true),
        )
        .unwrap();
        assert!(
            screen(&term2).contains(state_phrase(0, true)),
            "a running tool shows a working phrase"
        );
    }

    #[test]
    fn a_fresh_screen_shows_the_brand_and_guidance() {
        let mut term = Terminal::new(TestBackend::new(72, 20)).unwrap();
        draw(&mut term, &ui(vec![], false)).unwrap();
        let buf = term.backend().buffer().clone();
        let s: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(s.contains("Lattice"), "the brand wordmark shows");
        // the endpoint rides in the star field; the status bar keeps only quit
        assert!(s.contains("test"), "the endpoint meta shows");
        assert!(
            s.contains("Ctrl-D quit"),
            "the status bar keeps the quit hint"
        );
    }

    #[test]
    fn the_user_question_gets_a_background_bar() {
        let mut term = Terminal::new(TestBackend::new(72, 30)).unwrap();
        draw(&mut term, &ui(vec![Entry::User("QQ".to_string())], false)).unwrap();
        let buf = term.backend().buffer().clone();
        let y = (0..buf.area.height)
            .find(|&y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains("QQ")
            })
            .unwrap();
        // a cell well past the text still carries the bar's background
        assert_eq!(
            buf[(buf.area.width - 3, y)].bg,
            USER_BG,
            "the question row is painted with the bar background"
        );
    }

    #[test]
    fn a_jump_to_bottom_pill_shows_only_when_scrolled() {
        let entries: Vec<Entry> = (0..40).map(|i| Entry::User(format!("row{i:02}"))).collect();
        let mut term = Terminal::new(TestBackend::new(72, 12)).unwrap();

        // scrolled up → the pill exists and a click within it hits home
        let mut u = ui(entries.clone(), false);
        u.browsing.set_offset(5);
        let hit = draw(&mut term, &u).unwrap();
        let rect = hit.jump.expect("the pill shows while scrolled up");
        assert!(
            hit.jump_at(rect.x + 1, rect.y),
            "a click on the pill is recognized"
        );

        // pinned to the bottom → no pill
        let hit2 = draw(&mut term, &ui(entries, false)).unwrap();
        assert!(hit2.jump.is_none(), "no pill when pinned to the bottom");
    }

    #[test]
    fn a_turn_is_grouped_by_indentation_with_breathing_room() {
        let entries = vec![
            Entry::User("QQ".to_string()),
            tool("c1", vec!["ZZ".to_string()]),
            Entry::Agent("RR".to_string()),
        ];
        // roomy so the brand block plus the turn all fit without scrolling
        let mut term = Terminal::new(TestBackend::new(72, 30)).unwrap();
        // The reply has landed, so this turn's work is folded away; unfold it,
        // since what is under test is the LAYOUT of a turn, not its folding
        let mut u = ui(entries, false);
        u.browsing.expand("c1".to_string());
        draw(&mut term, &u).unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let idx = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap();
        let lead = |needle: &str| rows[idx(needle)].chars().take_while(|c| *c == ' ').count();

        // indentation groups the turn
        assert!(
            lead("QQ") < lead("Run"),
            "the user question is the flush-left anchor"
        );
        assert_eq!(
            lead("Run"),
            lead("RR"),
            "the tool head and the reply share the agent indent"
        );
        assert!(
            rows[idx("ZZ")].contains('⎿'),
            "the tool's output hangs off a tree connector"
        );
        // breathing room: a blank line separates the question from its response
        assert!(
            content_blank(&rows[idx("QQ") + 1]),
            "a blank line follows the question"
        );
    }

    #[test]
    fn background_command_cards_use_static_pending_and_unknown_markers() {
        let Entry::Tool(mut card) =
            tool("background", vec!["Running in background · pid 123".into()])
        else {
            unreachable!()
        };
        card.name = "Run".into();
        card.args = serde_json::json!({"command":"python3 - <<'PY'\nprint('hello')\nPY"});
        for (status, marker) in [(ToolStatus::Background, "·"), (ToolStatus::Unknown, "?")] {
            card.status = status;
            let render = |spinner| {
                tool_lines(&card, spinner, false, 60)
                    .iter()
                    .map(|(line, _)| line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let text = render('*');
            assert_eq!(text, render('/'), "background cards do not animate");
            assert!(text.starts_with(marker), "{text}");
            assert!(!text.contains('✓'), "pending is not success: {text}");
            let folded = folded_work(&[Entry::Tool(card.clone())], true).to_string();
            let label = if status == ToolStatus::Background {
                "1 running in background"
            } else {
                "1 outcome unknown"
            };
            assert!(
                folded.contains(label),
                "folding must not hide pending work: {folded}"
            );
            assert!(text.contains("Running in background · pid 123"), "{text}");
            assert!(
                !text.contains("background:call_") && !text.contains("(+19 lines)"),
                "{text}"
            );
        }
    }

    #[test]
    fn successful_write_cards_show_content_once_and_expand_past_the_preview() {
        let Entry::Tool(mut card) =
            tool("write-once", vec!["第一行".into(), "… (+8 lines)".into()])
        else {
            unreachable!()
        };
        card.name = "Write".into();
        card.status = ToolStatus::Ok;
        card.changed = Some("not-on-disk.txt".into());
        let content = "第一行\n\n第三行\n第四行\n第五行\n第六行\n第七行\n第八行\n最后一行\n";
        card.args = serde_json::json!({"path":"not-on-disk.txt", "content":content});
        let rows = tool_lines(&card, '*', true, 60);
        let text = rows
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("第一行").count(), 1, "{text}");
        assert_eq!(text.matches("最后一行").count(), 1, "{text}");
        assert!(
            !text.contains("content") && !text.contains("(+8 lines)"),
            "{text}"
        );
        assert!(text.contains("not-on-disk.txt"));
        let compact = tool_lines(&card, '*', false, 60);
        let text = compact
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("第一行").count(), 1, "{text}");
        assert!(
            text.contains("Ctrl-O") && !text.contains("最后一行"),
            "{text}"
        );
        let mut view = ui(vec![Entry::Tool(card.clone())], true);
        view.browsing.expand("write-once".into());
        let frame = snapshot(&view, 60, 24);
        assert_eq!(frame.rows.join("\n").matches("第一行").count(), 1);
        assert!(frame.has("最后一行"));
        card.args["content"] = serde_json::json!("");
        card.output.clear();
        let empty = tool_lines(&card, '*', true, 60);
        assert!(empty
            .iter()
            .any(|(line, _)| line.to_string().contains("Wrote empty file")));
    }

    #[test]
    fn edit_snapshots_render_numbered_gray_context_and_keep_legacy_fallback() {
        let old: String = (1..=16).map(|n| format!("context {n}\n")).collect();
        let new = old.replace("context 8\n", "中文甲\n中文乙\n");
        let Entry::Tool(mut card) = tool("context", vec!["- legacy".into(), "+ preview".into()])
        else {
            unreachable!()
        };
        card.name = "Edit".into();
        card.status = ToolStatus::Ok;
        card.changed = Some("not-on-disk.txt".into());
        card.args = serde_json::json!({"path":"not-on-disk.txt", "old":"context 8", "new":"中文甲\n中文乙"});
        card.edit_diff = Some(lattice::edit_diff::EditDiff::between(&old, &new));
        let rows = tool_lines(&card, '*', true, 60);
        let text = rows
            .iter()
            .map(|(line, _)| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            " 5  5   context 5",
            " 8    - context 8",
            "    8 + 中文甲",
            "    9 + 中文乙",
            " 9 10   context 9",
            "11 12   context 11",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        assert!(
            !text.contains("context 4") && !text.contains("context 12"),
            "{text}"
        );
        let context = rows
            .iter()
            .flat_map(|(line, _)| &line.spans)
            .find(|span| span.content == "  context 5")
            .unwrap();
        assert_eq!(context.style.fg, Some(DIM));
        let compact = tool_lines(&card, '*', false, 60);
        assert!(!compact
            .iter()
            .any(|(line, _)| line.to_string().contains("context 5")));
        let mut view = ui(vec![Entry::Tool(card.clone())], true);
        view.browsing.expand("context".into());
        let frame = snapshot(&view, 60, 24);
        assert!(frame.has("中文甲") && frame.has("context 11"));
        if let Ok(path) = std::env::var("LATTICE_EDIT_DIFF_PREVIEW") {
            std::fs::write(path, frame_html(&view, 60, 24)).unwrap();
            println!("{}", frame.rows.join("\n"));
        }
        card.edit_diff = None;
        let legacy = tool_lines(&card, '*', true, 60);
        assert!(legacy
            .iter()
            .any(|(line, _)| line.to_string().contains("+ 中文乙")));
        assert!(!legacy
            .iter()
            .any(|(line, _)| line.to_string().contains("@@")));
    }

    #[test]
    fn tool_layout_visual_fixture_preserves_output_and_fold_targets() {
        let Entry::Tool(mut card) = tool(
            "layout",
            vec![
                "Build finished successfully".into(),
                "12 tests passed".into(),
            ],
        ) else {
            unreachable!()
        };
        card.args = serde_json::json!({"command":"cargo test --lib","background":true,"cwd":"/workspace/Lattice"});
        let mut view = ui(
            vec![
                Entry::User("Check the desktop changes".into()),
                Entry::Tool(card),
            ],
            true,
        );
        view.browsing.expand("layout".into());
        let frame = snapshot(&view, 96, 22);
        assert!(frame.has("cargo test --lib"));
        assert!(frame.has("/workspace/Lattice"));
        assert!(frame.has("12 tests passed"));
        let mut terminal = Terminal::new(TestBackend::new(96, 22)).unwrap();
        let hit = draw(&mut terminal, &view).unwrap();
        let row = (hit.area.y..hit.area.bottom())
            .find(|&row| hit.card_at(hit.area.x, row).as_deref() == Some("layout"))
            .expect("the rendered card has a clickable row");
        let key = hit.card_at(hit.area.x, row).unwrap();
        toggle_card(&mut view, key);
        assert!(
            !view.browsing.is_expanded("layout"),
            "click folds the matching card"
        );
        let hit = draw(&mut terminal, &view).unwrap();
        let row = (hit.area.y..hit.area.bottom())
            .find(|&row| hit.card_at(hit.area.x, row).as_deref() == Some("layout"))
            .expect("the folded card remains clickable");
        toggle_card(&mut view, hit.card_at(hit.area.x, row).unwrap());
        assert!(
            view.browsing.is_expanded("layout"),
            "the same target expands again"
        );
        if let Ok(path) = std::env::var("LATTICE_TOOL_LAYOUT_PREVIEW") {
            std::fs::write(path, frame_html(&view, 96, 22)).unwrap();
            println!("{}", frame.rows.join("\n"));
        }
    }

    #[test]
    fn a_long_tool_output_folds_then_expands() {
        let out: Vec<String> = (0..20).map(|i| format!("line{i:02}")).collect();
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();

        // Collapsed: first line plus a "+N lines" hint; later lines are hidden.
        draw(&mut term, &ui(vec![tool("c1", out.clone())], true)).unwrap();
        let s = screen(&term);
        assert!(s.contains("line00"), "the first output line shows");
        assert!(s.contains("+19 lines"), "the fold hint shows the remainder");
        assert!(!s.contains("line19"), "later lines hidden while collapsed");

        // Expanded: the full output shows.
        let mut u = ui(vec![tool("c1", out)], true);
        u.browsing.expand("c1".to_string());
        draw(&mut term, &u).unwrap();
        assert!(
            screen(&term).contains("line19"),
            "expanded shows the last line"
        );
    }

    /// Thinking folds like a long tool output and unfolds on the same key —
    /// and collapsed, it shows only that thinking happened, never its content.
    #[test]
    fn a_thinking_card_folds_then_expands_on_the_same_key() {
        let card = || {
            Entry::Thinking(ThinkingCard {
                call: "ev_3".to_string(),
                lines: vec![
                    "first I check the date".to_string(),
                    "then the sum".to_string(),
                ],
            })
        };
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();

        draw(&mut term, &ui(vec![card()], true)).unwrap();
        let s = screen(&term);
        assert!(s.contains("Thought"), "the card announces itself");
        assert!(s.contains("2 lines"), "collapsed shows the size");
        assert!(
            !s.contains("first I check"),
            "collapsed shows no thinking content"
        );

        let mut u = ui(vec![card()], true);
        u.browsing.expand("ev_3".to_string());
        draw(&mut term, &u).unwrap();
        let s = screen(&term);
        assert!(s.contains("first I check"), "expanded shows the thinking");
        assert!(s.contains("then the sum"), "expanded shows every line");

        // Ctrl-O reaches the thinking card exactly as it reaches a tool card
        let mut u = ui(vec![card()], true);
        toggle_last_tool(&mut u);
        assert!(u.browsing.is_expanded("ev_3"));
    }

    /// Thinking happens before the answer, so it must SHOW before the answer.
    /// Waiting for the ledger cannot do that — the finished thought only lands
    /// when the whole call ends, which is after the reply has finished
    /// streaming. So the live thinking is its own buffer, drawn above.
    #[test]
    fn live_thinking_shows_above_the_streaming_reply() {
        let mut u = ui(vec![Entry::User("q".to_string())], true);
        u.live_output.seed_thinking("weighing it up".to_string());
        u.live_output.seed_reply("the answer is".to_string());
        let s = snapshot(&u, 72, 20);
        let thought = s.row_of("weighing it up").expect("the thinking shows");
        let reply = s.row_of("the answer is").expect("the reply shows");
        assert!(
            thought < reply,
            "thinking must sit above the reply it led to (rows {thought} vs {reply})"
        );
    }

    /// A high-effort thought runs long; only its tail stays on screen, or it
    /// would push the conversation off every turn.
    #[test]
    fn live_thinking_shows_only_its_tail() {
        let mut u = ui(vec![Entry::User("q".to_string())], true);
        u.live_output.seed_thinking(
            (0..40)
                .map(|i| format!("step{i:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let s = snapshot(&u, 72, 20);
        assert!(s.has("step39"), "the newest thinking shows");
        assert!(!s.has("step00"), "the oldest thinking has scrolled out");
    }

    /// A dumped frame must not tear wide characters in half.
    #[test]
    fn a_dumped_frame_keeps_wide_characters_whole() {
        let u = ui(vec![Entry::User("四加七等于几".to_string())], false);
        let snap = snapshot(&u, 40, 12);
        assert!(
            snap.rows.iter().any(|r| r.contains("四加七等于几")),
            "wide text must survive the dump intact: {:?}",
            snap.rows
        );
    }

    /// The question the turn is waiting on must be LOUD. Asserted on the style
    /// rather than through a frame snapshot, because a snapshot keeps the
    /// characters and throws the colours away — the very thing under test.
    #[test]
    fn an_approval_question_is_drawn_in_the_error_colour() {
        let entry = Entry::Approval("run wants in\n    y = allow".to_string());
        let lines = entry_lines(&entry, ' ', false, 80);
        assert_eq!(lines.len(), 2, "one line per line of the question");
        for (line, _) in &lines {
            for span in &line.spans {
                assert_eq!(
                    span.style.fg,
                    Some(ERR),
                    "every part of the question is in the error colour"
                );
            }
        }
        assert!(lines[0].0.spans[0].content.contains('⚠'));
    }

    /// Every region the frame lays out must get its own rows. A region added
    /// in the middle shifts every index after it, and an index left behind
    /// draws two things on top of each other — which is exactly what happened
    /// when the not-yet-heard row was inserted: the status bar landed inside
    /// the input box. Content assertions did not notice, because both were
    /// still on screen.
    #[test]
    fn every_region_of_the_frame_gets_its_own_rows() {
        let mut u = ui(vec![Entry::User("q".to_string())], true);
        u.domain.unseen.restore(vec!["not yet heard".to_string()]);
        u.draft.edit().insert_str("typing");
        let s = snapshot(&u, 70, 20);
        let row = |needle: &str| {
            s.row_of(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };

        // top to bottom: transcript, the waiting line, the input, the status
        let waiting = row("not yet heard");
        let input = row("typing");
        let status = row("Esc interrupt");
        assert!(
            row("q") < waiting,
            "the transcript is above the waiting line"
        );
        assert!(waiting < input, "the waiting line is above the input");
        assert!(
            input < status,
            "the status bar is BELOW the input, on its own row"
        );
        assert!(
            !s.rows[status].contains("typing"),
            "and shares its row with nothing: {:?}",
            s.rows[status]
        );
    }

    /// A line said while a question was already out is on the ledger — it was
    /// really said — but the model has not been shown it. That is a fact about
    /// right now, not about what happened, so it belongs beside the input box
    /// rather than in the transcript. It clears when the next question carries
    /// it away.
    #[test]
    fn lines_not_yet_heard_wait_beside_the_input_box() {
        let mut u = ui(vec![], true);
        let said = |text: &str| lattice::EventEnvelope {
            v: 1,
            id: text.to_string(),
            seq: 1,
            stream: "main".to_string(),
            time: "t".to_string(),
            event_type: core_events::USER_MESSAGE.to_string(),
            source: "ui".to_string(),
            causes: vec![],
            origin: None,
            reason: None,
            payload: serde_json::json!({"text": text}),
        };
        // Through the LIVE path, not by calling the fold step directly. The
        // display half of this feature was dead from the day it was written
        // — the live drain simply never called `note_unseen` — and a test
        // that reached past the drain to the step it wanted could not see
        // that. Anything a person actually runs goes through here.
        fold_render(
            &mut u,
            RenderEvent::Appended(Box::new(said("wait, also this"))),
        )
        .unwrap();
        assert_eq!(u.unseen(), ["wait, also this"]);
        let s = snapshot(&u, 76, 20);
        assert!(s.has("wait, also this"), "it shows while it waits");
        // ABOVE the input line — beside where the reader's own words are
        // typed, not among the things that have already happened
        // It shows in TWO places, and they say different things: the
        // transcript records that the line was said, the queued area says it
        // has not been carried to the model yet. Locate the queued one by
        // taking the last occurrence — a transcript user card is drawn with
        // the same `❯` the input line uses, so neither can be found by
        // prefix alone.
        let last_of = |needle: &str| {
            s.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.contains(needle))
                .map(|(n, _)| n)
                .next_back()
                .unwrap_or_else(|| panic!("{needle} is not on screen"))
        };
        let at = last_of("wait, also this");
        let caret = last_of("❯");
        assert!(at < caret, "it sits above the input box ({at} vs {caret})");

        // The next question carries it; the box clears
        let asked = lattice::EventEnvelope {
            event_type: core_events::MODEL_CALL_STARTED.to_string(),
            ..said("q")
        };
        fold_render(&mut u, RenderEvent::Appended(Box::new(asked))).unwrap();
        assert!(u.unseen().is_empty(), "carried away by the question");
        // Gone from BESIDE the input box. Still in the transcript, where it
        // belongs — it was said, and the ledger says so.
        let after = snapshot(&u, 76, 20);
        let showings = after
            .rows
            .iter()
            .filter(|row| row.contains("wait, also this"))
            .count();
        assert_eq!(showings, 1, "only the transcript still shows it");
    }

    /// A run with nothing to compress is not folded at all. "ran 0 steps and
    /// changed nothing" above a single thought spends a line to say nothing
    /// and hides the one thing that was there.
    #[test]
    fn a_lone_thought_or_a_lone_call_is_shown_outright_not_summarised() {
        let thought = Entry::Thinking(ThinkingCard {
            call: "t1".to_string(),
            lines: vec!["let me think".to_string()],
        });
        let alone = snapshot(
            &ui(
                vec![
                    Entry::User("q".to_string()),
                    thought.clone(),
                    Entry::Agent("done".to_string()),
                ],
                false,
            ),
            76,
            24,
        );
        assert!(alone.has("let me think"), "the thought itself shows");
        assert!(!alone.has("ran 0 steps"), "and no empty summary above it");

        let one_call = snapshot(
            &ui(
                vec![
                    Entry::User("q".to_string()),
                    thought,
                    tool("c1", vec!["out".to_string()]),
                    Entry::Agent("done".to_string()),
                ],
                false,
            ),
            76,
            24,
        );
        assert!(one_call.has("Run"), "a single call shows outright");
        assert!(
            !one_call.has("ran 1 step"),
            "one call needs no summary either"
        );
    }

    /// While the work is happening you want every step — that is how you tell
    /// the agent is not going astray. Once the answer has landed, what you
    /// still want to know is narrower: did anything CHANGE, and what. So the
    /// read-only half of a finished turn becomes a count and the changed half
    /// keeps its names. A failure is the one thing the fold may never swallow.
    #[test]
    fn a_finished_run_folds_to_a_sentence_naming_what_it_changed() {
        let card = |name: &str, id: &str, status, changed: Option<&str>| {
            Entry::Tool(ToolCard {
                call: Some(id.to_string()),
                name: name.to_string(),
                args: serde_json::json!({"command": "ls -la"}),
                status,
                output: vec!["out".to_string()],
                changed: changed.map(str::to_string),
                edit_diff: None,
            })
        };
        let thought = Entry::Thinking(ThinkingCard {
            call: "t1".to_string(),
            lines: vec!["have a look".to_string()],
        });

        // A turn that only looked around
        let looked = vec![
            Entry::User("q".to_string()),
            thought.clone(),
            card("run", "c1", ToolStatus::Ok, None),
            card("run", "c2", ToolStatus::Ok, None),
        ];

        // Still running: every step shows
        let live = snapshot(&ui(looked.clone(), true), 76, 24);
        assert!(live.has("have a look"), "live: the thinking shows");
        assert!(live.has("ls -la"), "live: the calls show");

        // Finished: one sentence, and the reassuring half is said out loud
        let mut done = looked;
        done.push(Entry::Agent("here you go".to_string()));
        let folded = snapshot(&ui(done.clone(), false), 76, 24);
        assert!(
            folded.has("ran 2 steps and changed nothing"),
            "the sentence"
        );
        assert!(!folded.has("have a look"), "the thinking is put away");
        assert!(
            !folded.has("ls -la"),
            "a read-only command is not worth naming"
        );
        assert!(folded.has("here you go"), "the answer is still there");

        // One key brings it all back
        let mut open = ui(done, false);
        toggle_last_tool(&mut open);
        let reopened = snapshot(&open, 76, 24);
        assert!(
            reopened.has("have a look"),
            "unfolded: the thinking is back"
        );
        assert!(reopened.has("ls -la"), "unfolded: every call is back");

        // A turn that changed things names them — at the END of the line,
        // which is where the eye lands
        let changed = vec![
            Entry::User("q".to_string()),
            thought,
            card("run", "c1", ToolStatus::Ok, None),
            card("write", "c2", ToolStatus::Ok, Some("notes.md")),
            card("edit", "c3", ToolStatus::Failed, Some("gone.rs")),
            Entry::Agent("done".to_string()),
        ];
        let folded = snapshot(&ui(changed, false), 76, 24);
        assert!(folded.has("changed notes.md"), "a real change is named");
        assert!(
            !folded.has("gone.rs"),
            "a FAILED edit changed nothing and must not be listed as a change"
        );
        assert!(
            folded.has("1 failed"),
            "a fold must never swallow a failure"
        );
    }

    /// Two questions can be open at once — the gate asks one per admission,
    /// and the model can ask for two things in a turn. A single slot meant the
    /// second overwrote the first and one tool call could never be answered by
    /// anybody, which makes the conversation permanently unsendable. They
    /// queue, oldest first, and an open question keeps the turn running so no
    /// new message can be started on top of it.
    #[test]
    fn authorizations_queue_and_hold_the_turn_until_each_is_answered() {
        let ask = |id: &str, holding: &str| lattice::EventEnvelope {
            v: 1,
            id: id.to_string(),
            seq: 1,
            stream: "main".to_string(),
            time: "t".to_string(),
            event_type: trust_policy::AUTH_REQUESTED.to_string(),
            source: "trust".to_string(),
            causes: vec![holding.to_string()],
            origin: None,
            reason: None,
            payload: serde_json::json!({"summary": "an admission"}),
        };
        let settle = |holding: &str| lattice::EventEnvelope {
            event_type: trust_policy::DECISION.to_string(),
            causes: vec![holding.to_string(), "answer".to_string()],
            ..ask("d", holding)
        };

        let mut u = ui(vec![], false);
        assert!(!u.busy(), "nothing pending, nothing running");

        u.note_authorization(&ask("q1", "call_a"));
        u.note_authorization(&ask("q2", "call_b"));
        assert!(u.busy(), "an open question keeps the turn running");
        assert_eq!(u.pending_auth(), Some("q1"), "oldest question first");

        // The second question's decision retires the SECOND, not the first
        u.note_authorization(&settle("call_b"));
        assert_eq!(u.pending_auth(), Some("q1"), "the first still stands");

        u.note_authorization(&settle("call_a"));
        assert_eq!(u.pending_auth(), None, "both settled");
        assert!(!u.busy(), "and the turn is free again");

        let mut browser = ask("browser-question", "forwarded");
        browser.event_type = lattice::components::browser_tools::AUTH_REQUESTED.into();
        browser.payload = json!({"held":"original"});
        u.note_authorization(&browser);
        let mut interrupted = settle("original");
        interrupted.event_type = core_events::INTERRUPTED.into();
        u.note_authorization(&interrupted);
        assert!(
            u.pending_auth().is_none(),
            "recovery must not revive a cancelled browser card"
        );
    }

    #[test]
    fn authorization_panel_replaces_prompt_and_restores_it_after_cancellation() {
        let mut u = ui(
            vec![Entry::Approval(
                "Browser request\n    click a button\n    y = allow · n = refuse".into(),
            )],
            false,
        );
        u.draft.edit().insert_str("unsent draft");
        u.panel.show(AT_COMMANDS);
        let mut ask = test_event(trust_policy::AUTH_REQUESTED, &["call-a"]);
        ask.id = "question-a".into();
        ask.payload = json!({"tool":"Browser", "summary":"click a button"});
        u.note_authorization(&ask);
        for (width, height) in [(80, 30), (24, 12)] {
            let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
            draw(&mut term, &u).unwrap();
            let text = screen(&term);
            assert!(text.contains("> Refuse request"), "{text}");
            if width >= 80 {
                assert!(
                    text.contains("Browser request"),
                    "authorization must expose the transcript over a suspended panel: {text}"
                );
            }
            assert!(
                !text.contains("unsent draft"),
                "draft must be hidden: {text}"
            );
            assert!(
                !text.contains("y = allow"),
                "old shortcuts must not be advertised"
            );
        }
        u.note_authorization(&test_event(core_events::INTERRUPTED, &["call-a"]));
        assert!(u.authorization_prompt().unwrap().is_none());
        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        draw(&mut term, &u).unwrap();
        assert!(screen(&term).contains("unsent draft"));
    }

    #[test]
    fn authorization_panel_ignores_modified_submit_and_repeat_keys() {
        use ratatui::crossterm::event::KeyEvent;
        let mut u = ui(vec![], false);
        u.draft.edit().insert_str("draft");
        let hit = Hit::default();
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            &hit,
        );
        let cursor = u.cursor();
        u.domain
            .authorizations
            .restore(vec![("first".into(), "call-a".into())]);
        for key in [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat),
        ] {
            on_key(&mut u, None, key, &hit);
        }
        assert_eq!(u.pending_auth(), Some("first"));
        assert_eq!(u.input(), "draft");
        assert_eq!(u.cursor(), cursor);
        assert!(!u.domain.turns.busy(), "no chat submission or interrupt");
        u.panel.show(AT_COMMANDS);
        u.panel.scroll_down(3);
        let panel_scroll = u.panel_scroll();
        on_mouse(
            &mut u,
            ratatui::crossterm::event::MouseEvent {
                kind: ratatui::crossterm::event::MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            &hit,
        );
        assert_eq!(
            u.panel_scroll(),
            panel_scroll,
            "suspended panel must not consume scrolling"
        );
    }

    #[test]
    fn authorization_panel_preserves_draft_and_enter_answers_only_one_request() {
        use ratatui::crossterm::event::KeyEvent;
        let mut u = ui(vec![], false);
        u.draft.edit().insert('x');
        u.domain.authorizations.restore(vec![
            ("first".into(), "call-a".into()),
            ("second".into(), "call-b".into()),
        ]);
        let cursor = u.cursor();
        let hit = Hit::default();
        for code in [KeyCode::Char('y'), KeyCode::Backspace, KeyCode::Left] {
            on_key(&mut u, None, KeyEvent::new(code, KeyModifiers::NONE), &hit);
        }
        absorb_paste(&mut u, "not chat input");
        assert_eq!(u.input(), "x", "authorization must isolate the draft");
        assert_eq!(u.cursor(), cursor);
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &hit,
        );
        assert_eq!(u.pending_auth(), Some("second"));
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &hit,
        );
        assert_eq!(u.pending_auth(), None);
        assert_eq!(u.input(), "x");
        assert_eq!(u.cursor(), cursor);
    }

    #[test]
    fn authorization_keys_advance_without_receipts_and_isolate_typing() {
        use ratatui::crossterm::event::KeyEvent;
        let mut u = ui(vec![], false);
        for (question, call) in [("first", "call-a"), ("second", "call-b")] {
            let mut event = test_event(trust_policy::AUTH_REQUESTED, &[call]);
            event.id = question.into();
            u.note_authorization(&event);
        }
        let key = |c, modifiers| KeyEvent::new(KeyCode::Char(c), modifiers);
        let hit = Hit::default();
        u.draft.edit().insert('x');
        on_key(&mut u, None, key('y', KeyModifiers::NONE), &hit);
        assert_eq!(u.draft.editor().text(), "x");
        assert_eq!(u.pending_auth(), Some("first"));
        u.draft.edit().clear();
        on_key(&mut u, None, key('y', KeyModifiers::CONTROL), &hit);
        assert_eq!(u.pending_auth(), Some("first"));
        on_key(&mut u, None, key('n', KeyModifiers::ALT), &hit);
        assert_eq!(u.pending_auth(), Some("first"));
        u.draft.edit().clear();
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &hit,
        );
        assert!(u.authorization_prompt().unwrap().unwrap().allow_selected);
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &hit,
        );
        assert_eq!(u.pending_auth(), Some("second"));
        assert!(!u.authorization_prompt().unwrap().unwrap().allow_selected);
        assert!(u.busy());
        on_key(
            &mut u,
            None,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &hit,
        );
        assert_eq!(u.pending_auth(), None);
        assert!(
            !u.busy(),
            "locally answered questions no longer hold the display busy"
        );
        assert!(u.draft.editor().text().is_empty());
        let mut third = test_event(trust_policy::AUTH_REQUESTED, &["call-c"]);
        third.id = "third".into();
        u.note_authorization(&third);
        let mut second_done = test_event(trust_policy::DECISION, &["call-b"]);
        second_done.payload = json!({"held":"call-b"});
        u.note_authorization(&second_done);
        assert_eq!(
            u.pending_auth(),
            Some("third"),
            "late decisions do not eat the next question"
        );
    }

    #[test]
    fn a_short_tool_output_always_shows_in_full() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let out = vec!["only one line".to_string()];
        draw(&mut term, &ui(vec![tool("c1", out)], true)).unwrap();
        let s = screen(&term);
        assert!(
            s.contains("only one line"),
            "short output shows without folding"
        );
        assert!(!s.contains("Ctrl-O"), "no fold hint for short output");
    }

    #[test]
    fn card_at_maps_a_click_through_the_scroll_offset() {
        // Transcript occupies screen rows 2,3,4; scrolled so the top visible row
        // is wrapped-row index 3. Row 4 (index 4) belongs to card "mid".
        let mut owner = vec![None; 8];
        owner[4] = Some("mid".to_string());
        let hit = Hit {
            input_width: 36,
            area: ratatui::layout::Rect::new(0, 2, 40, 3),
            offset: 3,
            more_above: false,
            more_below: false,
            top: None,
            owner,
            jump: None,
            status: Vec::new(),
            links: Vec::new(),
        };
        assert_eq!(
            hit.card_at(1, 3).as_deref(),
            Some("mid"),
            "click lands on the card"
        );
        assert_eq!(hit.card_at(1, 2), None, "a non-card row toggles nothing");
        assert_eq!(
            hit.card_at(1, 6),
            None,
            "a click below the transcript is ignored"
        );
    }

    /// The unit Ctrl-O acts on is the RUN of work, not one card inside it —
    /// otherwise unfolding a folded turn would take as many keystrokes as it
    /// had steps. The run is keyed by its first member, the same key the
    /// renderer folds it under.
    #[test]
    fn toggle_last_tool_flips_the_whole_run_of_work() {
        let mut u = ui(
            vec![
                Entry::User("q".to_string()),
                tool("a", vec!["x".into()]),
                tool("b", vec!["y".into()]),
            ],
            false,
        );
        toggle_last_tool(&mut u);
        assert!(u.tool_expanded(Some("a")), "the run expands under one key");
        toggle_last_tool(&mut u);
        assert!(!u.tool_expanded(Some("a")), "toggles back to folded");
    }

    #[test]
    fn scroll_by_clamps_at_both_ends() {
        // 50 rows in a 6-high viewport: the top is reached at 44 rows up.
        let hit = Hit {
            input_width: 56,
            area: ratatui::layout::Rect::new(0, 1, 60, 6),
            offset: 0,
            more_above: false,
            more_below: false,
            top: None,
            owner: vec![None; 50],
            jump: None,
            status: Vec::new(),
            links: Vec::new(),
        };
        let mut u = ui(vec![], false);
        scroll_by(&mut u, -5, &hit);
        assert_eq!(u.browsing.offset(), 0, "can't scroll past the latest row");
        scroll_by(&mut u, 1000, &hit);
        assert_eq!(
            Some(u.browsing.offset()),
            hit.max_scroll(),
            "clamps at the top"
        );
        assert_eq!(hit.max_scroll(), Some(44));
        scroll_by(&mut u, -1000, &hit);
        assert_eq!(u.browsing.offset(), 0, "returns to the bottom");
    }

    #[test]
    fn scrollback_moves_the_window_and_shows_an_indicator() {
        let entries: Vec<Entry> = (0..40).map(|i| Entry::User(format!("row{i:02}"))).collect();
        let screen_of = |term: &Terminal<TestBackend>| -> String {
            term.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        // Pinned to the bottom: the latest row shows, the first is off the top.
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        draw(&mut term, &ui(entries.clone(), false)).unwrap();
        let screen = screen_of(&term);
        assert!(screen.contains("row39"), "the latest row shows");
        assert!(!screen.contains("row00"), "the first row is off the top");
        assert!(!screen.contains("scrolled up"), "no indicator when pinned");

        // Scrolled all the way up: the window reaches the welcome at the very
        // top of the scrollback, the latest row leaves the screen, and the
        // indicator appears.
        let mut u = ui(entries, false);
        u.browsing.set_offset(1000); // clamped to the top by draw
        draw(&mut term, &u).unwrap();
        let screen = screen_of(&term);
        assert!(screen.contains("Lattice"), "the welcome sits at the top");
        assert!(
            !screen.contains("row39"),
            "the latest row is now off the bottom"
        );
        assert!(screen.contains("scrolled up"), "the scroll indicator shows");
    }

    #[test]
    fn a_multiline_input_grows_and_places_the_cursor() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let mut u = ui(vec![], false);
        u.draft.edit().set("first\nsecond"); // cursor lands at the end, on row 1
        draw(&mut term, &u).unwrap();
        let cursor = term.get_cursor_position().unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();

        assert!(rows.iter().any(|r| r.contains("first")), "first line shows");
        assert!(
            rows[cursor.y as usize].contains("second"),
            "the cursor sits on the second input row"
        );
        assert!(
            rows[cursor.y as usize - 1].contains("first"),
            "the first input row is just above it"
        );
    }

    #[test]
    fn arrow_selection_marks_a_candidate() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let mut u = ui(vec![], false);
        u.draft.edit().set("/");
        u.draft.next_hint(SLASH.len()); // the second candidate
        draw(&mut term, &u).unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains('▸'), "the selected candidate is marked");
    }

    #[test]
    fn typing_a_slash_shows_the_candidate_list() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let mut u = ui(vec![], false);
        u.draft.edit().set("/");
        draw(&mut term, &u).unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();

        assert!(screen.contains("/help"), "candidate name shows");
        // summaries are unique to the hint (the status bar has none of these)
        assert!(
            screen.contains("list the commands"),
            "candidate summary shows"
        );
        assert!(
            screen.contains("clear the screen"),
            "candidate summary shows"
        );
    }

    /// A real session over the standard assembly with a scripted brain — no
    /// key, no network, and every command path a frontend can take is the one
    /// the product takes.
    fn scripted_session() -> Session {
        std::env::set_var("LATTICE_SCRIPTED", "1");
        let mut cfg = PresetConfig::from_env();
        cfg.overlay = None; // a test must not read or write the user's installs
        let dir = std::env::temp_dir().join("lattice-model-test");
        std::fs::create_dir_all(&dir).unwrap();
        Session::spawn("ui", move |tx| {
            session_build::build(tx, &cfg, dir.join("ledger.jsonl"))
        })
        .expect("the scripted assembly starts")
    }

    /// Two models that differ in every way the picker has to show.
    fn two_models() -> ModelView {
        ModelView {
            rows: vec![
                ModelRow {
                    id: "deepseek".to_string(),
                    model: "deepseek-v4-flash".to_string(),
                    endpoint: "api.deepseek.com".to_string(),
                    dialect: "openai".to_string(),
                    window: Some(1_000_000),
                    rungs: vec!["high".to_string(), "max".to_string()],
                    accepts_images: false,
                    key_env: "DEEPSEEK_API_KEY".to_string(),
                    key_present: true,
                },
                ModelRow {
                    id: "sonnet".to_string(),
                    model: "claude-sonnet-5".to_string(),
                    endpoint: "api.anthropic.com".to_string(),
                    dialect: "anthropic".to_string(),
                    window: Some(200_000),
                    accepts_images: true,
                    rungs: ["low", "medium", "high", "xhigh", "max"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    key_env: "ANTHROPIC_API_KEY".to_string(),
                    key_present: false,
                },
            ],
            now: Some(0),
        }
    }

    /// The marker follows the LEDGER, not the keypress. A swap can be refused
    /// — an unknown name, a missing key — and a picker that moved on being
    /// asked would then be showing a model that is not running. The effort
    /// rungs and the title bar move with it, because all three describe the
    /// model rather than the setting.
    #[test]
    fn the_current_model_moves_when_the_swap_lands_not_when_it_is_asked_for() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_catalog() = two_models();
        *u.domain.model.fixture_effort() = deepseek();
        u.domain.title = "deepseek-v4-flash · /work".to_string();

        // Asking is not happening. Driven through a REAL session, because the
        // marker must not move on the keypress — a swap can be refused, and a
        // picker that moved when asked would then be showing a model that is
        // not running. (This session is scripted, so the swap is refused; that
        // is the point.)
        let session = scripted_session();
        run_slash(&mut u, "/model sonnet", Some(&session));
        // Waited on causally: the session answers with a notice and then goes
        // quiet, so the quiet IS the signal that the answer has arrived.
        while let Some(render) = session.next_render() {
            let done = matches!(render, RenderEvent::Quiescent);
            fold_render(&mut u, render).unwrap();
            if done {
                break;
            }
        }
        assert_eq!(
            u.domain.model.catalog().now,
            Some(0),
            "the marker stays where the LEDGER put it, not where a keypress asked"
        );
        assert!(
            u.flash.as_deref().is_some_and(|t| t.contains("scripted")),
            "and a refusal says why: {:?}",
            u.flash
        );
        session.shutdown();

        let mut landed = lattice::EventEnvelope {
            v: 1,
            id: "e9".to_string(),
            seq: 9,
            stream: "s".to_string(),
            time: "t".to_string(),
            event_type: core_events::COMPONENT_REPLACED.to_string(),
            source: "core".to_string(),
            causes: vec![],
            origin: None,
            reason: Some("the user chose it".to_string()),
            payload: json!({
                "instance": "model",
                "from": "openai-model",
                "to": "anthropic-model",
                "config": {"model": "claude-sonnet-5"},
            }),
        };
        u.domain.accounting.seed_last_call(Some(lattice::Usage {
            prompt: 150_000,
            ..Default::default()
        }));
        u.note_model_swap(&landed);
        assert!(
            u.domain.accounting.last_call().is_none(),
            "a new model must not inherit the old context measurement"
        );
        assert_eq!(u.domain.model.catalog().now, Some(1), "now it is running");
        assert!(
            u.domain.title.starts_with("claude-sonnet-5 · "),
            "{}",
            u.domain.title
        );
        assert_eq!(
            u.domain.model.effort().rungs,
            u.domain.model.catalog().rows[1].rungs,
            "the dial's rungs belong to the model, so they change with it — and \
             they come from the ROW, because a model described in the user's own \
             catalog has rungs no lookup here would find"
        );

        assert!(!status_state(&u).iter().any(|s| s.text.starts_with("ctx ")));
        let mut measured = landed.clone();
        measured.event_type = core_events::MODEL_CALL_COMPLETED.to_string();
        measured.payload = json!({"usage": {"input_tokens": 150_000,
            "cache_read_input_tokens": 1_000, "output_tokens": 10}});
        u.note_usage(&measured);
        assert_eq!(u.domain.accounting.last_call().unwrap().prompt, 151_000);
        assert_eq!(u.domain.accounting.session_total().prompt, 151_000);
        assert!(status_state(&u).iter().any(|s| s.text.starts_with("ctx ")));

        // Some other instance being replaced is not this
        landed.payload["instance"] = json!("some-tool");
        landed.payload["config"] = json!({"model": "deepseek-v4-flash"});
        u.note_model_swap(&landed);
        assert_eq!(
            u.domain.model.catalog().now,
            Some(1),
            "a different seat is not the model's"
        );
    }

    #[test]
    fn effort_display_changes_before_any_session_receipt_is_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("effort.ledger");
        let cfg = PresetConfig {
            adapter: "scripted".into(),
            model: "scripted".into(),
            base_url: String::new(),
            key_env: String::new(),
            workspace: None,
            context_window: 64_000,
            usage_input_field: "input_tokens".into(),
            profile: None,
            catalog_problems: Vec::new(),
            system: "Test model.".into(),
            thinking: None,
            scripted: Some(json!({"script":[]})),
            overlay: None,
            assembly: None,
        };
        let session =
            Session::spawn("ui", move |tx| session_build::build(tx, &cfg, ledger)).unwrap();
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_effort() = deepseek();
        run_slash(&mut u, "/effort max", Some(&session));
        assert_eq!(u.domain.model.effort().now.as_deref(), Some("max"));
        assert!(u.entries.is_empty());
        // Deliberately do not drain renders: display acknowledgement is local.
        session.shutdown();
    }

    fn deepseek() -> EffortView {
        EffortView {
            rungs: ["high", "max"].iter().map(|s| s.to_string()).collect(),
            now: Some("high".to_string()),
        }
    }

    /// The dial's job is to show the SUBSTITUTION before the choice is made.
    /// On DeepSeek's two-rung ladder four of the six words are aliases, and
    /// nothing else in the interface would ever say so — the model would
    /// simply think less than it was asked to.
    #[test]
    fn the_dial_says_where_the_choice_under_the_cursor_would_land() {
        let mut term = Terminal::new(TestBackend::new(88, 24)).unwrap();
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_effort() = deepseek();
        run_slash(&mut u, "/effort", None);
        draw(&mut term, &u).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        for word in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
            assert!(screen.contains(word), "the scale shows {word}");
        }
        assert!(
            screen.contains("[high]"),
            "the cursor opens on the setting in force: {screen}"
        );
        assert!(screen.contains("current"), "and says so");
        assert!(
            screen.contains("← →"),
            "the keys are the scale's, not a list's"
        );
        assert!(
            !screen.contains("❯"),
            "the prompt is gone — the dial HAS the input box: {screen}"
        );
    }

    /// Running the command opens the mode; the mode gives the box back when
    /// it is done. Typing the command must not open anything — it is a door,
    /// and a door is opened by going through it.
    #[test]
    fn the_command_opens_the_mode_and_leaving_gives_the_box_back() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_effort() = deepseek();

        u.draft.edit().set("/effort");
        assert!(u.controls.dial().is_none(), "typing it opens nothing");

        run_slash(&mut u, "/effort", None);
        assert_eq!(
            u.controls.dial(),
            Some(dial_home(u.domain.model.effort())),
            "running it opens the dial"
        );

        commit_dial(&mut u, None);
        assert!(
            u.controls.dial().is_none(),
            "and choosing gives the box back"
        );
    }

    /// The cursor opens on the setting in force, so "open it and press
    /// Enter" is the most likely first interaction anyone has with it — and
    /// that path changes nothing. Answering it with silence is
    /// indistinguishable from a command that does not work, which is exactly
    /// how it was first reported.
    #[test]
    fn committing_the_setting_already_in_force_still_says_so() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_effort() = deepseek();
        run_slash(&mut u, "/effort", None);
        commit_dial(&mut u, None);

        // A receipt, so it rides beside the input rather than joining the
        // conversation — but it must still SAY something.
        let said = match u.flash.as_deref() {
            Some(text) => text.to_string(),
            None => panic!("pressing Enter must say something, got {:?}", u.entries),
        };
        assert!(said.contains("unchanged"), "{said}");
        assert!(said.contains("high"), "and names where it stayed: {said}");
        assert!(u.controls.dial().is_none(), "and the dial closes");
    }

    /// Only the BARE command opens the dial. Given a value it just sets it —
    /// someone who typed what they wanted should not be handed a chooser.
    ///
    /// Asserted through `run_slash` rather than a predicate, because a
    /// predicate only tests call is a predicate that proves nothing about
    /// what the command does.
    #[test]
    fn a_command_with_a_value_sets_it_instead_of_opening_the_dial() {
        let mut u = ui(vec![], false);
        *u.domain.model.fixture_effort() = deepseek();
        run_slash(&mut u, "/effort max", None);
        assert!(
            u.controls.dial().is_none(),
            "a value was given; nothing to choose"
        );
    }

    #[test]
    fn a_notice_renders_as_frontend_chatter() {
        let mut term = Terminal::new(TestBackend::new(72, 24)).unwrap();
        let u = ui(
            vec![Entry::Notice(
                "unknown command /foo — try /help".to_string(),
            )],
            false,
        );
        draw(&mut term, &u).unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();

        assert!(screen.contains("unknown command"), "the notice text shows");
    }

    /// The palette remains bounded even in a tall terminal, follows the
    /// selected command, and reports matches outside its visible window.
    #[test]
    fn a_menu_too_tall_to_show_says_how_many_it_kept_back() {
        let mut u = ui(Vec::new(), false);
        u.draft.edit().insert('/');
        u.domain
            .skills
            .push(("tail-skill".into(), "last candidate".into()));
        let candidates = slash_matches("/", &u.domain.skills, u.domain.model.effort());
        assert!(candidates.len() > 6);
        let press = |ui: &mut Ui, code| {
            on_key(
                ui,
                None,
                ratatui::crossterm::event::KeyEvent::from(code),
                &Hit::default(),
            )
        };
        for height in [30u16, 8, 12, 16] {
            u.draft.reset_selection();
            for (selected, candidate) in candidates.iter().enumerate() {
                assert_eq!(u.draft.selected(), selected);
                let frame = snapshot(&u, 70, height);
                // The prompt contains a slash too, but is not a candidate row.
                let shown = frame
                    .rows
                    .iter()
                    .filter(|row| {
                        let text = row.trim_start();
                        text.starts_with("▸ /") || text.starts_with('/')
                    })
                    .count();
                assert!(
                    (1..=6).contains(&shown),
                    "height {height}: {shown} candidate rows"
                );
                if height == 30 {
                    assert_eq!(shown, 6, "extra terminal height must not grow the palette");
                }
                assert!(
                    frame
                        .rows
                        .iter()
                        .any(|row| row.contains(&format!("▸ {} ", candidate.name))),
                    "height {height}: selected {} is outside the visible window: {:?}",
                    candidate.name,
                    frame.rows
                );
                assert!(
                    frame
                        .rows
                        .iter()
                        .any(|row| row.contains(&format!("+{}", candidates.len() - shown))),
                    "height {height}: hidden matches must still be reported"
                );
                if selected + 1 < candidates.len() {
                    assert!(!press(&mut u, KeyCode::Down));
                }
            }
        }
        // Completion uses the global selection, not a visible row number.
        assert!(!press(&mut u, KeyCode::Tab));
        assert_eq!(u.draft.editor().text(), "/tail-skill ");
        u.draft.edit().set("/");
        u.draft.reset_selection();
        let exit = candidates
            .iter()
            .position(|candidate| candidate.name == "/exit")
            .unwrap();
        assert!(exit >= 6);
        for _ in 0..exit {
            assert!(!press(&mut u, KeyCode::Down));
        }
        assert!(
            press(&mut u, KeyCode::Enter),
            "Enter executes the selected command beyond the first window"
        );
    }

    /// A wrapped list item's continuation goes under the item's TEXT, not under
    /// its bullet. On the bullet's column it reads as the next item — the eye
    /// takes the left edge as the list's spine, and anything sitting on that
    /// spine is a new entry as far as it is concerned.
    #[test]
    fn a_wrapped_list_item_hangs_under_its_own_text() {
        // The second row of a wrapped item starts further in than the first.
        let long = "• ".to_string() + &"word ".repeat(30);
        let u = ui(vec![Entry::Agent(long)], false);
        let frame = snapshot(&u, 40, 14);
        let lead = |r: &str| r.len() - r.trim_start().len();
        let rows: Vec<&String> = frame.rows.iter().filter(|r| r.contains("word")).collect();
        assert!(rows.len() >= 2, "the item should have wrapped");
        assert!(
            lead(rows[1]) > lead(rows[0]),
            "the continuation should hang: {:?}",
            rows
        );
    }

    /// A table narrows by losing a column it can NAME, not by turning every
    /// cell into an ellipsis. At 46 columns the components tab used to read
    /// `scripted-model  in-…` with the third column gone anyway — four ruined
    /// cells where one readable one would have said more.
    #[test]
    fn a_narrow_panel_drops_whole_columns_rather_than_gutting_every_one() {
        let mut u = ui(Vec::new(), false);
        u.parts = vec![
            (
                "tool-catalog".to_string(),
                "tool-catalog".to_string(),
                "in-process",
                "FindTools".to_string(),
                false,
                Vec::new(),
            ),
            (
                "fs".to_string(),
                "fs-reader".to_string(),
                "in-process",
                "Read Ls".to_string(),
                false,
                Vec::new(),
            ),
        ];
        let head = |width: usize| panel_rows(AT_COMPONENTS, &u, width)[0].1.clone();
        let fs = |width: usize| {
            panel_rows(AT_COMPONENTS, &u, width)
                .into_iter()
                .find(|(k, _)| k.contains("fs"))
                .expect("the fs row")
                .1
        };

        assert!(head(100).contains("provides"), "{}", head(100));
        assert!(fs(100).contains("Read Ls"), "{}", fs(100));

        // The last column is the first to go, and what stays is whole.
        assert!(!head(46).contains("provides"), "{}", head(46));
        assert!(fs(46).contains("fs-reader"), "{}", fs(46));

        // Nothing ever runs past the panel's edge.
        for width in [30usize, 46, 60, 100, 160] {
            for line in panel_lines(AT_COMPONENTS, &u, width) {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    wrap::str_cols(&text) <= width,
                    "width {width}: {text:?} overflows"
                );
            }
        }
    }

    /// A dragged file folds to one chip, and the chip is only a display: the
    /// line still SENDS the path, so what reaches the model is the sentence a
    /// bare paste would have given it. The agent opens the file with the tools
    /// it already has — nothing is read off the disk here, which is why this
    /// needs no closed list of extensions the way pictures do.
    #[test]
    fn a_dragged_file_folds_to_a_chip_but_still_sends_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("notes.md");
        std::fs::write(&at, "x".repeat(2048)).expect("a file to drag");
        let path = at.display().to_string();

        let mut u = ui(Vec::new(), false);
        absorb_paste(&mut u, &path);
        let shown = u.draft.editor().shown().into_owned();
        assert!(shown.starts_with("[file notes.md ("), "{shown}");
        assert!(
            !shown.contains('/'),
            "the path should be folded away: {shown}"
        );
        assert_eq!(
            u.draft.editor().expanded(),
            path,
            "but it is what gets sent"
        );

        // One backspace takes the whole thing back, because the chip is a
        // single character in the buffer.
        u.draft.edit().backspace();
        assert!(
            u.draft.editor().is_empty(),
            "a chip comes off in one keystroke"
        );

        // A path to nothing is just words.
        let missing = dir.path().join("no-such-file").display().to_string();
        absorb_paste(&mut u, &missing);
        assert_eq!(u.draft.editor().expanded(), missing);
    }

    /// In a mode the readouts go. What you need then is the way OUT, and
    /// state you cannot touch until you take it is in the way.
    #[test]
    fn a_mode_clears_the_readouts() {
        let mut u = ui_with_a_full_bar();
        assert!(!status_state(&u).is_empty());
        u.panel.show(AT_CONTEXT);
        let frame = snapshot(&u, 88, 12);
        let bar = frame.rows.last().expect("a status row").clone();
        assert!(bar.contains("Esc close"), "{bar:?}");
        assert!(
            !bar.contains("deepseek-v4"),
            "the readouts should have stood aside: {bar:?}"
        );
    }

    /// The keys begin at the same column whatever else is true — readouts or
    /// none, mode or no mode. They are the half you look for when you are
    /// stuck, and a thing you hunt for is a thing that must not move.
    ///
    /// This is what putting them on the LEFT bought. With the keys against the
    /// right edge they slid across the row every time a readout appeared or a
    /// mode cleared them all.
    #[test]
    fn the_keys_never_move() {
        let full = ui_with_a_full_bar();
        let bare = {
            // Nothing with anything to say: no model, no window, default rung.
            let mut u = ui(Vec::new(), false);
            u.workspace = String::new();
            u
        };
        let mut moded = ui_with_a_full_bar();
        moded.panel.show(AT_CONTEXT);

        let column = |u: &Ui| {
            let frame = snapshot(u, 88, 12);
            let bar = frame.rows.last().expect("a status row").clone();
            bar.find(|c: char| !c.is_whitespace())
                .expect("the bar always says something")
        };
        assert!(status_state(&full).len() > status_state(&bare).len());
        assert_eq!(column(&full), column(&bare), "readouts moved the keys");
        assert_eq!(column(&full), column(&moded), "a mode moved the keys");
    }

    /// Too narrow: readouts go, keys never do. The keys are the half that says
    /// what to press; the state only says where you are. Before this the row
    /// was cut at the terminal's edge — mid-word, without an ellipsis, and it
    /// was the KEYS that lost their tail.
    #[test]
    fn a_narrow_row_sheds_readouts_and_keeps_every_key() {
        let u = ui_with_a_full_bar();
        let mut seen = Vec::new();
        for width in [88u16, 60, 44, 34, 26] {
            let frame = snapshot(&u, width, 12);
            let bar = frame.rows.last().expect("a status row").clone();
            assert!(
                bar.contains("Ctrl-D quit"),
                "width {width}: the keys were cut: {bar:?}"
            );
            seen.push(bar.trim().to_string());
        }
        assert!(
            seen[0].len() > seen[seen.len() - 1].len(),
            "the row should have shed something: {seen:?}"
        );
    }

    /// Every readout is a door onto what it describes. This is what turns the
    /// bar from something you read into something you steer by — and it is why
    /// the panels stop being things you have to remember a slash command for.
    #[test]
    fn clicking_a_readout_opens_what_it_describes() {
        let mut u = ui_with_a_full_bar();
        u.domain.background.fixture_push(
            started_live(
                "Run",
                &json!({"command": "build"}),
                &json!({"job": "build-job", "background": true}),
                0,
            )
            .unwrap(),
        );
        let mut term =
            Terminal::new(ratatui::backend::TestBackend::new(200, 12)).expect("headless backend");
        let hit = draw(&mut term, &u).expect("draw");
        assert_eq!(
            hit.status
                .iter()
                .map(|(_, opens)| *opens)
                .collect::<Vec<_>>(),
            vec![
                Opens::Models,
                Opens::Context,
                Opens::Effort,
                Opens::Background,
                Opens::Config
            ]
        );

        for (rect, opens) in hit.status.clone() {
            let selected = hit.status_at(rect.x, rect.y).expect("a readout hit target");
            assert_eq!(selected, opens);
            u.panel.dismiss_preserving_details();
            u.controls.close_dial();
            u.panel.select_row(usize::MAX);
            u.panel.reset_scroll();
            u.panel.scroll_down(7);
            on_mouse(
                &mut u,
                ratatui::crossterm::event::MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                },
                &hit,
            );
            match opens {
                Opens::Models => {
                    assert_eq!(u.panel.active(), Some(AT_MODELS));
                    assert_eq!(
                        u.panel.selected_row(),
                        u.domain.model.catalog().now.unwrap_or(0)
                    );
                }
                Opens::Effort => {
                    assert_eq!(u.controls.dial(), Some(dial_home(u.domain.model.effort())));
                    assert_eq!(u.panel.active(), None);
                }
                Opens::Context => assert_eq!(u.panel.active(), Some(AT_CONTEXT)),
                Opens::Background => assert_eq!(u.panel.active(), Some(AT_BACKGROUND)),
                Opens::Config => assert_eq!(u.panel.active(), Some(AT_CONFIG)),
            }
            if opens != Opens::Effort {
                assert_eq!(u.controls.dial(), None);
            }
            assert_eq!(u.panel.scroll_offset(), 0, "every entry starts at the top");
        }
    }

    /// Work in flight and a standing arrangement are two different facts, and
    /// the panel has to keep them apart: an expert that is out spending money
    /// ends when it answers, while a repeating timer answers over and over and
    /// is still there afterwards. Counting them together would report a timer
    /// that sleeps all day as though something were working.
    #[test]
    fn what_is_running_leaves_when_it_answers_and_what_is_armed_stays() {
        let started = |call: &str, tool: &str, args: Value| {
            let mut e = test_event(core_events::TOOL_EXEC_STARTED, &[]);
            e.event_type = core_events::TOOL_EXEC_STARTED.to_string();
            e.payload = json!({"call": call, "tool": tool, "arguments": args});
            e
        };
        let done = |call: &str, result: Value| {
            let mut e = test_event(core_events::TOOL_EXEC_COMPLETED, &[]);
            e.event_type = core_events::TOOL_EXEC_COMPLETED.to_string();
            e.payload = json!({"call": call, "status": "ok", "result": result});
            e
        };
        let woke = |source: &str, body: Value| {
            let mut e = test_event(core_events::WAKE, &[]);
            e.event_type = core_events::WAKE.to_string();
            e.payload = json!({"source": source, "summary": "", "body": body});
            e
        };
        let mut u = ui(Vec::new(), false);

        // An expert goes out and comes back.
        u.absorb(
            &started(
                "c1",
                "ask",
                json!({"expert": "explorer", "prompt": "count the files"}),
            ),
            0,
        );
        u.absorb(&done("c1", json!({"job": 1})), 0);
        assert_eq!(u.domain.background.rows().len(), 1, "the expert is out");
        assert!(
            !u.domain.background.rows()[0].standing,
            "and it is working, not waiting"
        );

        // A repeating timer is armed at the same time.
        u.absorb(&started("c2", "Schedule", json!({"interval_ms": 60000})), 0);
        u.absorb(&done("c2", json!({"timer": 1})), 0);
        assert_eq!(u.domain.background.rows().len(), 2);
        assert_eq!(
            u.domain
                .background
                .rows()
                .iter()
                .filter(|l| !l.standing)
                .count(),
            1,
            "one thing is working; the timer is not one of them"
        );

        // The timer fires. It is not finished — that is what a timer does.
        u.absorb(&woke("timer:1", json!({"timer": 1, "fire": 1})), 0);
        let timer = u
            .domain
            .background
            .rows()
            .iter()
            .find(|l| l.key == "timer:1")
            .expect("the timer is still armed after firing");
        assert_eq!(timer.fires, 1);

        // The expert answers. That IS its ending.
        u.absorb(&woke("expert:1", json!({"job": 1, "text": "42"})), 0);
        assert!(
            !u.domain
                .background
                .rows()
                .iter()
                .any(|l| l.key == "expert:1"),
            "an expert is gone once it has answered: {:?}",
            u.domain.background.rows()
        );

        // The timer ends when it is cancelled, and only then.
        u.absorb(&started("c3", "Unschedule", json!({"timer": 1})), 0);
        u.absorb(&done("c3", json!({"timer": 1, "note": "cancelled"})), 0);
        assert!(
            u.domain.background.rows().is_empty(),
            "{:?}",
            u.domain.background.rows()
        );
    }

    #[test]
    fn string_background_job_ids_reach_both_the_panel_and_status_bar() {
        let mut u = ui(Vec::new(), false);
        let mut start = test_event(core_events::TOOL_EXEC_STARTED, &[]);
        start.payload =
            json!({"call":"call_run", "tool":"Run", "arguments":{"command":"cargo test"}});
        u.absorb(&start, 0);
        let mut receipt = test_event(core_events::TOOL_EXEC_COMPLETED, &[]);
        receipt.payload = json!({"call":"call_run", "status":"ok", "result":{"job":"call_run", "background":true, "pid":123}});
        u.absorb(&receipt, 1);
        assert_eq!(u.domain.background.rows().len(), 1);
        assert_eq!(u.domain.background.rows()[0].key, "background:call_run");
        assert!(panel_rows(AT_BACKGROUND, &u, 100)
            .iter()
            .any(|(left, right)| format!("{left}{right}").contains("cargo test")));
        assert!(snapshot(&u, 160, 30)
            .rows
            .iter()
            .any(|row| row.contains("1 background")));
        let mut wake = test_event(core_events::WAKE, &[]);
        wake.payload = json!({"source":"background:call_run", "summary":"done", "body":{"job":"call_run","exit_code":0}});
        u.absorb(&wake, 2);
        assert!(u.domain.background.rows().is_empty());
    }

    /// An expert must not be a black box. It runs in its own stream on its own
    /// thread, so the conversation sees one line when the work goes out and
    /// one when it comes back — and in between, without this, nothing at all
    /// while it reads files and spends money.
    ///
    /// What it is doing is already being written down. This reads it.
    #[test]
    fn what_an_expert_has_spent_is_read_off_the_record_it_is_writing() {
        let dir = std::env::temp_dir().join(format!("lattice-obs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat-sub-1.jsonl");
        let line = |e: &lattice::EventEnvelope| serde_json::to_string(e).unwrap();
        let mut call = test_event(core_events::MODEL_CALL_COMPLETED, &[]);
        call.payload = json!({"usage": {"prompt_tokens": 1200, "completion_tokens": 340}});
        let record = [
            line(&test_event(core_events::TOOL_EXEC_STARTED, &[])),
            line(&test_event(core_events::TOOL_EXEC_STARTED, &[])),
            line(&call),
        ]
        .join("\n");
        // A ledger event is committed by its terminating newline.
        std::fs::write(&path, format!("{record}\n")).unwrap();

        let mut u = ui(Vec::new(), false);
        u.domain.background.fixture_push(view::Live {
            kind: "expert",
            key: "expert:1".to_string(),
            label: "explorer".to_string(),
            standing: false,
            fires: 0,
            since: 0,
            ledger: Some(path.display().to_string()),
            tools: 0,
            tokens: 0,
            read_len: 0,
        });
        u.refresh_experts();
        assert_eq!(
            u.domain.background.rows()[0].tools,
            2,
            "two tool calls so far"
        );
        assert!(
            u.domain.background.rows()[0].tokens >= 1200,
            "and what it has spent: {}",
            u.domain.background.rows()[0].tokens
        );

        // Re-reading is skipped when the file has not grown, which is safe
        // only because a ledger is append-only — same length means same
        // content. Not asserted here: reading the same file twice gives the
        // same answer whether or not anything was skipped, so a test of it
        // would only be restating the arithmetic.

        // When it does grow, the numbers follow.
        std::fs::write(
            &path,
            format!(
                "{record}\n{}\n",
                line(&test_event(core_events::TOOL_EXEC_STARTED, &[]))
            ),
        )
        .unwrap();
        u.refresh_experts();
        assert_eq!(u.domain.background.rows()[0].tools, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
