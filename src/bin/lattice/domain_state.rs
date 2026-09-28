//! One event fold for the live domain state and the historical projection.
//! Cards, local receipts, input, and host navigation are outside this module.

use super::{
    accounting::{self, Accounting},
    authorizations::Authorizations,
    background_state::{self, Background, Mutation},
    event_inputs::EventInputs,
    model_state::ModelState,
    turns::{self, Turns},
    unseen::Unseen,
};
use lattice::{components::skill_library, core_events as ce, EventEnvelope, Usage};
use std::path::PathBuf;

#[cfg(test)]
#[path = "domain_state/tests.rs"]
mod tests;

/// The two real consumers share boundary rules, not a display clock. Historical
/// folds use (), so a replay cannot accidentally create a local animation time.
pub(super) trait TurnState: Default {
    type Clock: Copy;
    fn observe_boundary(&mut self, event: &EventEnvelope, clock: Self::Clock) -> bool;
    fn history(&self) -> turns::History;
    fn install(&mut self, history: turns::History, clock: Self::Clock);
}
impl TurnState for Turns {
    type Clock = usize;
    fn observe_boundary(&mut self, event: &EventEnvelope, clock: usize) -> bool {
        self.observe(event, clock)
    }
    fn history(&self) -> turns::History {
        Turns::history(self)
    }
    fn install(&mut self, history: turns::History, clock: usize) {
        self.restore(history, clock);
    }
}
impl TurnState for turns::History {
    type Clock = ();
    fn observe_boundary(&mut self, event: &EventEnvelope, _: ()) -> bool {
        self.observe(event).unwrap_or(false)
    }
    fn history(&self) -> turns::History {
        *self
    }
    fn install(&mut self, history: turns::History, _: ()) {
        *self = history;
    }
}

pub(super) trait BackgroundState<C>: Default {
    fn apply_mutation(&mut self, mutation: Mutation, clock: C);
    fn history(&self) -> &background_state::History;
    fn install(&mut self, history: background_state::History, clock: C);
}
impl BackgroundState<usize> for Background {
    fn apply_mutation(&mut self, mutation: Mutation, clock: usize) {
        self.apply(mutation, clock);
    }
    fn history(&self) -> &background_state::History {
        Background::history(self)
    }
    fn install(&mut self, history: background_state::History, clock: usize) {
        self.restore(history, clock);
    }
}
impl BackgroundState<()> for background_state::History {
    fn apply_mutation(&mut self, mutation: Mutation, _: ()) {
        self.apply(mutation);
    }
    fn history(&self) -> &background_state::History {
        self
    }
    fn install(&mut self, history: background_state::History, _: ()) {
        *self = history;
    }
}

pub(super) type Live = Domain<Turns, Background>;
pub(super) type Historical = Domain<turns::History, background_state::History>;

/// Own the domain owners rather than borrowing an arbitrary collection of Ui
/// fields. Owners still enforce their own mutation boundaries. This aggregate
/// only coordinates the order and consequences shared by both event paths.
pub(super) struct Domain<T: TurnState, B: BackgroundState<T::Clock>> {
    pub(super) title: String,
    pub(super) skills: Vec<(String, String)>,
    pub(super) model: ModelState,
    /// Independently observed at the same committed prefix; not a v4 snapshot field.
    pub(super) compaction: Option<lattice::components::context_gate::CompactionStatus>,
    pub(super) accounting: Accounting,
    pub(super) event_inputs: EventInputs,
    pub(super) authorizations: Authorizations,
    pub(super) unseen: Unseen,
    pub(super) turns: T,
    pub(super) background: B,
    pub(super) expert_dir: Option<PathBuf>,
    pub(super) stream_id: String,
}

impl<T: TurnState, B: BackgroundState<T::Clock>> Default for Domain<T, B> {
    fn default() -> Self {
        Self {
            title: "replay".into(),
            skills: Vec::new(),
            model: ModelState::default(),
            compaction: None,
            accounting: Accounting::default(),
            event_inputs: EventInputs::default(),
            authorizations: Authorizations::default(),
            unseen: Unseen::default(),
            turns: T::default(),
            background: B::default(),
            expert_dir: None,
            stream_id: String::new(),
        }
    }
}

impl<T: TurnState, B: BackgroundState<T::Clock>> Domain<T, B> {
    pub(super) fn completed_usage(&self, event: &EventEnvelope) -> Option<Usage> {
        let current = self.model.catalog().current();
        accounting::completed_usage(
            event,
            &self.event_inputs,
            current.map(|row| row.model.as_str()).unwrap_or_default(),
            current.map(|row| row.dialect.as_str()).unwrap_or_default(),
        )
    }

    /// Returns whether the live coordinator should retire its local receipt.
    /// The order here is also the historical projection's only event order.
    pub(super) fn absorb(
        &mut self,
        event: &EventEnvelope,
        clock: T::Clock,
        mut trace: Option<&mut lattice::memory::Breakdown>,
    ) -> bool {
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
        self.model.observe_window(event);
        let new_turn = measure!(Stage::Bookkeeping, {
            let new_turn = self.note_turn_boundary(event, clock);
            self.authorizations.observe(event);
            self.unseen.observe(event);
            self.note_listing(event);
            self.note_model_swap(event);
            new_turn
        });
        measure!(Stage::Size, self.event_inputs.observe_size(event));
        measure!(Stage::Usage, self.note_usage(event));
        measure!(Stage::Background, self.note_background(event, clock));
        new_turn
    }

    fn note_turn_boundary(&mut self, event: &EventEnvelope, clock: T::Clock) -> bool {
        let new_turn = self.turns.observe_boundary(event, clock);
        if new_turn {
            self.accounting.new_turn();
        }
        new_turn
    }

    // Preserve single-stage test setup without exposing partial production folds.
    #[cfg(test)]
    pub(super) fn test_note_usage(&mut self, event: &EventEnvelope) {
        self.note_usage(event);
    }
    #[cfg(test)]
    pub(super) fn test_note_model_swap(&mut self, event: &EventEnvelope) {
        self.note_model_swap(event);
    }
    #[cfg(test)]
    pub(super) fn test_note_background(&mut self, event: &EventEnvelope, clock: T::Clock) {
        self.note_background(event, clock);
    }
    #[cfg(test)]
    pub(super) fn test_note_turn_boundary(
        &mut self,
        event: &EventEnvelope,
        clock: T::Clock,
    ) -> bool {
        self.note_turn_boundary(event, clock)
    }

    fn note_listing(&mut self, event: &EventEnvelope) {
        if event.event_type != skill_library::SKILL_LISTING {
            return;
        }
        self.skills = event.payload["skills"]
            .as_array()
            .map(|skills| {
                skills
                    .iter()
                    .filter_map(|skill| {
                        Some((
                            skill["name"].as_str()?.to_string(),
                            skill["description"].as_str().unwrap_or("").to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    fn note_model_swap(&mut self, event: &EventEnvelope) {
        let Some(model) = self.model.observe_swap(event) else {
            return;
        };
        self.accounting.invalidate_context_measurement();
        if let Some((_, rest)) = self.title.split_once(" · ") {
            self.title = format!("{model} · {rest}");
        }
    }
    fn note_usage(&mut self, event: &EventEnvelope) {
        if event.event_type == ce::MODEL_CALL_STARTED {
            if event.payload.get("purpose").is_none() {
                self.accounting.observe_prompt(event, &self.event_inputs);
            }
            self.event_inputs.observe_model_start(event);
            return;
        }
        if event.event_type != ce::MODEL_CALL_COMPLETED || event.payload.get("purpose").is_some() {
            return;
        }
        let usage = self.completed_usage(event);
        self.event_inputs.consume_model_start(&event.causes);
        let Some(usage) = usage else {
            return;
        };
        self.accounting
            .record_completion(self.turns.history().number as u64, usage);
    }
    fn note_background(&mut self, event: &EventEnvelope, clock: T::Clock) {
        if event.event_type == ce::TOOL_EXEC_STARTED {
            self.event_inputs.observe_tool_start(event);
            return;
        }
        let request = if event.event_type == ce::TOOL_EXEC_COMPLETED {
            self.event_inputs
                .tool_request(event.payload["call"].as_str().unwrap_or_default())
                .map(|(tool, args)| (tool.as_str(), args))
        } else {
            None
        };
        let Some(mut mutation) = background_state::interpret(event, request) else {
            return;
        };
        if let Mutation::Start(row) = &mut mutation {
            if row.kind == "expert" {
                if let (Some(dir), Some(job)) =
                    (self.expert_dir.as_ref(), row.key.rsplit(':').next())
                {
                    let stream = &self.stream_id;
                    row.ledger = Some(
                        lattice::ledgers::named_path(dir, &format!("{stream}-sub-{job}"))
                            .display()
                            .to_string(),
                    );
                }
            }
        }
        self.background.apply_mutation(mutation, clock);
    }
}
