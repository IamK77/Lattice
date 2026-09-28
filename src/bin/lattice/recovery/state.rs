//! Exact event-derived state. No editor, keypress, live-stream buffer, or
//! filesystem-refreshed expert progress may be captured into this snapshot.

use crate::terminal_host::{
    accounting::Measurements,
    domain_state::{BackgroundState, Domain, Historical, TurnState},
    view,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    title: String,
    busy: bool,
    waiting: bool,
    done: bool,
    turn: usize,
    pending_auth: Vec<(String, String)>,
    unseen: Vec<String>,
    skills: Vec<(String, String)>,
    usage: Option<lattice::Usage>,
    turn_usage: lattice::Usage,
    session_usage: lattice::Usage,
    composition: Vec<(String, u64)>,
    previous: HashMap<String, u64>,
    turn_growth: HashMap<String, u64>,
    session_growth: HashMap<String, u64>,
    background: Vec<Standing>,
    models: view::ModelView,
    running: lattice::models::Entry,
    effort: view::EffortView,
    effective_window: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Standing {
    kind: String,
    key: String,
    label: String,
    standing: bool,
    fires: usize,
    ledger: Option<String>,
}

fn label(value: &str) -> io::Result<&'static str> {
    use view::facts::MaterialKind;
    [
        MaterialKind::User,
        MaterialKind::Reply,
        MaterialKind::ToolResult,
        MaterialKind::ToolCall,
        MaterialKind::Wake,
        MaterialKind::Other,
    ]
    .into_iter()
    .map(MaterialKind::label)
    .chain([
        "system prompt",
        "tool declarations",
        "condensed summaries",
        "thinking",
    ])
    .find(|label| *label == value)
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown recovered material kind",
        )
    })
}

fn owned(values: &HashMap<&'static str, u64>) -> HashMap<String, u64> {
    values
        .iter()
        .map(|(kind, count)| ((*kind).into(), *count))
        .collect()
}

fn material(values: HashMap<String, u64>) -> io::Result<HashMap<&'static str, u64>> {
    values
        .into_iter()
        .map(|(kind, count)| Ok((label(&kind)?, count)))
        .collect()
}

impl State {
    /// Production checkpoints can only capture the historical projection.
    pub fn capture(pure: &Historical) -> Self {
        Self::capture_domain(pure)
    }

    #[cfg(test)]
    pub fn reference_capture(reference: &crate::terminal_host::domain_state::Live) -> Self {
        Self::capture_domain(reference)
    }

    fn capture_domain<T: TurnState, B: BackgroundState<T::Clock>>(pure: &Domain<T, B>) -> Self {
        let turn = pure.turns.history();
        Self {
            title: pure.title.clone(),
            busy: turn.busy,
            waiting: turn.waiting,
            done: turn.done,
            turn: turn.number,
            pending_auth: pure.authorizations.history(),
            unseen: pure.unseen.lines().to_vec(),
            skills: pure.skills.clone(),
            usage: pure.accounting.last_call(),
            turn_usage: pure.accounting.turn_total(),
            session_usage: pure.accounting.session_total(),
            composition: pure
                .accounting
                .parts()
                .iter()
                .map(|(kind, count)| ((*kind).into(), *count))
                .collect(),
            previous: owned(pure.accounting.previous()),
            turn_growth: owned(pure.accounting.turn_growth()),
            session_growth: owned(pure.accounting.session_growth()),
            background: pure
                .background
                .history()
                .rows()
                .iter()
                .map(|live| Standing {
                    kind: live.kind.into(),
                    key: live.key.clone(),
                    label: live.label.clone(),
                    standing: live.standing,
                    fires: live.fires,
                    ledger: live.ledger.clone(),
                })
                .collect(),
            models: pure.model.catalog().clone(),
            running: pure.model.running().clone(),
            effort: pure.model.effort().clone(),
            effective_window: pure.model.effective_window(),
        }
    }

    pub fn apply<T: TurnState, B: BackgroundState<T::Clock>>(
        self,
        ui: &mut Domain<T, B>,
        clock: T::Clock,
    ) -> io::Result<()> {
        // Validate the complete decoded state before mutating its destination.
        let composition = self
            .composition
            .into_iter()
            .map(|(kind, count)| Ok((label(&kind)?, count)))
            .collect::<io::Result<_>>()?;
        let measurements = Measurements::restored(
            self.usage,
            self.turn_usage,
            self.session_usage,
            composition,
            material(self.previous)?,
            material(self.turn_growth)?,
            material(self.session_growth)?,
        );
        let background = self
            .background
            .into_iter()
            .map(|live| {
                let kind = ["expert", "command", "timer", "watch"]
                    .into_iter()
                    .find(|kind| *kind == live.kind)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unknown recovered standing activity",
                        )
                    })?;
                Ok(crate::terminal_host::background_state::HistoricalRow {
                    kind,
                    key: live.key,
                    label: live.label,
                    standing: live.standing,
                    fires: live.fires,
                    ledger: live.ledger,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        if self
            .models
            .now
            .is_some_and(|index| index >= self.models.rows.len())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recovered current model is outside its catalog",
            ));
        }
        ui.title = self.title;
        ui.turns.install(
            crate::terminal_host::turns::History {
                busy: self.busy,
                waiting: self.waiting,
                done: self.done,
                number: self.turn,
            },
            clock,
        );
        ui.authorizations.restore(self.pending_auth);
        ui.unseen.restore(self.unseen);
        ui.skills = self.skills;
        ui.accounting.restore(measurements);
        ui.background.install(
            crate::terminal_host::background_state::History::new(background),
            clock,
        );
        ui.model = crate::terminal_host::ModelState::new(
            self.models,
            self.running,
            self.effort,
            self.effective_window,
        );
        Ok(())
    }
}
