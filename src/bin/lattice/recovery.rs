//! A pure historical projection, separate from the live Ui that keypresses and
//! expert progress can change before any corresponding event is committed.

use crate::terminal_host::{event_inputs::EventInputs, view, Ui, SETTLED_TICK};
use lattice::{EventEnvelope, LogReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;

#[path = "recovery/state.rs"]
mod state;
use crate::terminal_host::domain_state::Historical;
use state::State;

#[cfg(test)]
pub(super) fn reference_snapshot(ui: &Ui) -> serde_json::Value {
    serde_json::to_value(State::reference_capture(&ui.domain)).unwrap()
}

#[cfg(test)]
#[path = "recovery/background_baseline.rs"]
mod background_baseline;
#[cfg(test)]
#[path = "recovery/model_baseline.rs"]
mod model_baseline;
#[cfg(test)]
#[path = "recovery/tests.rs"]
mod tests;

// Bump when State, its reducers, or initial-context interpretation changes,
// not when an unrelated part of the executable is released.
// v5 also rejects phantom background rows created from synchronous expert outcomes.
const VERSION: u32 = 5;
const EVERY: u64 = 256;

#[derive(Clone, Serialize)]
pub(super) struct Initial {
    title: String,
    models: view::ModelView,
    running: lattice::models::Entry,
    effort: view::EffortView,
    effective_window: Option<u64>,
    expert_dir: Option<std::path::PathBuf>,
    stream_id: String,
}

impl Initial {
    pub fn of(ui: &Ui) -> Self {
        Self {
            title: ui.domain.title.clone(),
            models: ui.domain.model.catalog().clone(),
            running: ui.domain.model.running().clone(),
            effort: ui.domain.model.effort().clone(),
            effective_window: ui.domain.model.effective_window(),
            expert_dir: ui.domain.expert_dir.clone(),
            stream_id: ui.domain.stream_id.clone(),
        }
    }

    fn projection(&self, reader: &LogReader) -> io::Result<Historical> {
        let mut pure = Historical {
            title: self.title.clone(),
            model: crate::terminal_host::model_state::ModelState::new(
                self.models.clone(),
                self.running.clone(),
                self.effort.clone(),
                self.effective_window,
            ),
            expert_dir: self.expert_dir.clone(),
            stream_id: self.stream_id.clone(),
            ..Historical::default()
        };
        pure.accounting.bind_peaks(reader)?;
        Ok(pure)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    context: [u8; 32],
    state: State,
    peaks: view::peaks::State,
    summary: lattice::ledgers::Summary,
}

pub(super) struct Recovery {
    reader: LogReader,
    initial: Initial,
    key: String,
    context: [u8; 32],
    through: u64,
    saved_through: u64,
    pure: Box<Historical>,
    compaction: lattice::components::context_gate::CompactionObserver,
    summary: lattice::ledgers::Summary,
    pub cold_reason: Option<String>,
    pub replayed: u64,
}

impl Recovery {
    pub fn recover(
        initial: Initial,
        reader: LogReader,
        facts: &mut view::facts::EventFacts,
        through: u64,
        mut trace: Option<&mut lattice::memory::Breakdown>,
    ) -> io::Result<Self> {
        if through > reader.snapshot_end() {
            return Err(invalid("UI recovery boundary is not committed"));
        }
        let key = "terminal-state".to_string();
        let context: [u8; 32] = Sha256::digest(serde_json::to_vec(&initial)?).into();
        let checkpoint = reader.load_checkpoint::<Saved>(&key, VERSION, through)?;
        let boundary = checkpoint.through;
        let mut result = Self {
            pure: Box::new(initial.projection(&reader)?),
            compaction: Default::default(),
            summary: Default::default(),
            reader,
            initial,
            key,
            context,
            through: 0,
            saved_through: 0,
            cold_reason: checkpoint.cold_reason,
            replayed: 0,
        };
        if let Some(saved) = checkpoint.state {
            match (|| {
                if saved.context != context {
                    return Err(invalid("terminal interpretation context changed"));
                }
                let peaks = view::peaks::Peaks::open(&result.reader, saved.peaks)?;
                saved.state.apply(result.pure.as_mut(), ())?;
                result.pure.accounting.install_peaks(peaks);
                result.summary = saved.summary;
                Ok::<_, io::Error>(())
            })() {
                Ok(()) => {
                    result.through = boundary;
                    result.saved_through = boundary;
                }
                Err(error) => {
                    result.reset(error.to_string())?;
                }
            }
        }
        let warmed = result.through > 0;
        if let Err(error) = result.read_tail(facts, through, trace.as_deref_mut()) {
            if !warmed {
                return Err(error);
            }
            result.reset(error.to_string())?;
            result.read_tail(facts, through, trace)?;
        }
        result.refresh_compaction()?;
        result.save()?;
        Ok(result)
    }

    fn reset(&mut self, reason: String) -> io::Result<()> {
        *self.pure = self.initial.projection(&self.reader)?;
        self.compaction = Default::default();
        self.summary = Default::default();
        self.through = 0;
        self.saved_through = 0;
        self.cold_reason = Some(reason);
        self.replayed = 0;
        Ok(())
    }

    fn read_tail(
        &mut self,
        facts: &mut view::facts::EventFacts,
        through: u64,
        mut trace: Option<&mut lattice::memory::Breakdown>,
    ) -> io::Result<()> {
        let reader = self.reader.clone();
        // Includes decoding the next batch and releasing the previous one.
        let mut reading = trace
            .as_deref_mut()
            .and_then(|trace| trace.begin(lattice::memory::Stage::BetweenBatches, self.through));
        let result = reader.visit_range(self.through + 1, through, |batch| {
            if let Some(trace) = trace.as_deref_mut() {
                trace.end(reading.take());
            }
            for event in batch {
                let inputs = match EventInputs::read(facts, event) {
                    Ok(inputs) => inputs,
                    Err(error) => {
                        *facts = facts.rebuild(through, error.to_string())?;
                        EventInputs::read(facts, event)?
                    }
                };
                self.fold(event, inputs, trace.as_deref_mut())?;
                self.replayed += 1;
            }
            reading = trace.as_deref_mut().and_then(|trace| {
                trace.begin(lattice::memory::Stage::BetweenBatches, self.through)
            });
            Ok(())
        });
        if let Some(trace) = trace {
            trace.end(reading);
        }
        result
    }

    fn fold(
        &mut self,
        event: &EventEnvelope,
        inputs: EventInputs,
        trace: Option<&mut lattice::memory::Breakdown>,
    ) -> io::Result<()> {
        if event.seq != self.through + 1
            || event.stream != self.reader.stream()
            || event.seq > self.reader.snapshot_end()
        {
            return Err(invalid(
                "UI projection event is outside its next committed prefix",
            ));
        }
        self.pure.event_inputs = inputs;
        if let Some(usage) = self.pure.completed_usage(event) {
            self.pure
                .accounting
                .record_indexed_peak(self.pure.turns.number as u64, usage.prompt)?;
        }
        self.summary.observe_event(event)?;
        self.pure.absorb(event, (), trace);
        self.pure.event_inputs.release();
        self.through = event.seq;
        if self.through - self.saved_through >= EVERY {
            self.save()?;
        }
        Ok(())
    }

    pub fn advance(
        &mut self,
        event: &EventEnvelope,
        inputs: EventInputs,
        facts: &mut view::facts::EventFacts,
    ) -> io::Result<()> {
        if event.seq <= self.through {
            return Ok(());
        }
        if let Err(error) = self.fold(event, inputs, None) {
            if event.seq > self.reader.snapshot_end() {
                return Err(error);
            }
            self.reset(error.to_string())?;
            self.read_tail(facts, event.seq, None)?;
            self.save()?;
        }
        self.refresh_compaction()?;
        Ok(())
    }

    fn refresh_compaction(&mut self) -> io::Result<()> {
        self.pure.compaction = self.compaction.status_at(&self.reader, self.through)?;
        Ok(())
    }

    pub fn compaction_status(
        &self,
    ) -> Option<&lattice::components::context_gate::CompactionStatus> {
        self.pure.compaction.as_ref()
    }

    fn save(&mut self) -> io::Result<()> {
        let saved = Saved {
            context: self.context,
            state: State::capture(&self.pure),
            peaks: self.pure.accounting.snapshot_peaks()?,
            summary: self.summary.clone(),
        };
        self.reader
            .save_checkpoint(&self.key, VERSION, self.through, &saved)?;
        if let Err(error) = self.summary.save_checkpoint(&self.reader) {
            eprintln!("cannot save ledger summary: {error}");
        }
        self.saved_through = self.through;
        Ok(())
    }

    pub fn apply(&mut self, ui: &mut Ui) -> io::Result<()> {
        let peaks = self.pure.accounting.fork_peaks(&self.reader)?;
        State::capture(&self.pure).apply(&mut ui.domain, SETTLED_TICK)?;
        ui.domain.accounting.install_peaks(peaks);
        ui.domain.accounting.discard_reference_history();
        ui.domain.compaction = self.pure.compaction.clone();
        ui.replayed_through = self.through;
        Ok(())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
