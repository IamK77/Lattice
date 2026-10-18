//! Correlate the footer with the current conversational model call. This is
//! display state only: the shared contract still decides call settlement.
use lattice::{core_events as ce, EventEnvelope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Outcome {
    #[default]
    Done,
    Interrupted,
    TimedOut,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Interrupted => "Interrupted",
            Self::TimedOut => "Timed out",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Completion {
    calls: ce::PendingCalls,
    active: Option<String>,
    identified: bool,
    pending: Option<Ending>,
    finished_turn: Option<usize>,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ending {
    id: String,
    outcome: Outcome,
    turn: usize,
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            calls: ce::PendingCalls::new(ce::MODEL_CALL_STARTED),
            active: None,
            identified: false,
            pending: None,
            finished_turn: None,
            outcome: Outcome::Done,
        }
    }
}

impl Completion {
    pub fn observe(&mut self, event: &EventEnvelope, turn: usize) {
        if event.event_type == ce::MODEL_CALL_STARTED {
            if event.payload.get("purpose").is_some() {
                return;
            }
            self.finished_turn = None;
            self.identified = true;
            self.active = Some(event.id.clone());
            self.pending = None;
            self.outcome = Outcome::Done;
        } else if !matches!(
            event.event_type.as_str(),
            ce::MODEL_CALL_COMPLETED | ce::INTERRUPTED
        ) {
            return;
        }
        self.calls.observe(ce::EventRelations {
            id: &event.id,
            event_type: &event.event_type,
            causes: &event.causes,
        });
        if self
            .active
            .as_ref()
            .is_some_and(|id| !self.calls.requests().contains(id))
        {
            let outcome = if event.event_type == ce::INTERRUPTED {
                if event.payload["by"] == "deadline" {
                    Outcome::TimedOut
                } else {
                    Outcome::Interrupted
                }
            } else if event.payload["status"] == "cancelled" {
                Outcome::Interrupted
            } else {
                Outcome::Done
            };
            self.active = None;
            self.pending = Some(Ending {
                id: event.id.clone(),
                outcome,
                // Mid-call input belongs to this round. Only input arriving
                // after its outcome must outlive the old turn boundary.
                turn,
            });
        }
    }

    pub fn lands_live_text(&self, event: &EventEnvelope) -> bool {
        if !matches!(
            event.event_type.as_str(),
            ce::MODEL_CALL_COMPLETED | ce::OUTPUT_REPLY
        ) {
            return false;
        }
        !self.identified
            || self
                .pending
                .as_ref()
                .is_some_and(|end| event.id == end.id || event.causes.contains(&end.id))
    }

    pub fn ends_activity(&self, turn: usize) -> bool {
        self.finished_turn.is_none_or(|finished| finished == turn)
    }

    pub fn finish(&mut self, event: &EventEnvelope) -> bool {
        if let Some(end) = &self.pending {
            if event.causes.contains(&end.id) {
                self.outcome = end.outcome;
                self.finished_turn = Some(end.turn);
                self.pending = None;
                return true;
            }
        }
        // Legacy uncorrelated boundaries remain readable. They cannot close
        // an identified active call or replace its already known outcome.
        event.causes.is_empty() && self.active.is_none() && self.pending.is_none()
    }
}
