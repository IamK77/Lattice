//! Recorded activity and read-only progress for frontend projections.
//! Provider measurements and provisional output estimates remain distinct.
use crate::{components::minimal_loop, core_events as ce, EventEnvelope};
use serde::{Deserialize, Serialize};

mod completion;
pub use completion::{Completion, Outcome};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenCount {
    /// None means no measurement is available, not zero consumption.
    pub known: Option<u64>,
    /// False means `known` is only a subtotal / lower bound.
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnSummary {
    pub elapsed_ms: Option<u64>,
    pub input: TokenCount,
    pub output: TokenCount,
    /// Provisional total including streamed text, displayed with `~`, never `≥`.
    pub output_estimate: Option<u64>,
    pub running: bool,
    pub model_pending: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub busy: bool,
    pub waiting: bool,
    pub done: bool,
    pub number: usize,
    pub completion: Completion,
}

impl Activity {
    /// Return the recorded activity boundary, without local optimistic state.
    pub fn observe(&mut self, event: &EventEnvelope) -> Option<bool> {
        self.completion.observe(event, self.number);
        let (busy, waiting, done, new_turn) = match event.event_type.as_str() {
            ce::USER_MESSAGE if !event.causes.is_empty() => return None,
            ce::USER_MESSAGE | ce::WAKE => {
                self.number = self.number.wrapping_add(1);
                (true, false, false, true)
            }
            minimal_loop::WAITING => (false, true, false, false),
            ce::MODEL_CALL_STARTED if event.payload.get("purpose").is_none() => {
                (true, false, false, false)
            }
            ce::TURN_COMPLETED if self.completion.finish(event) => {
                let current = self.completion.ends_activity(self.number);
                (!current, false, current, false)
            }
            _ => return None,
        };
        self.busy = busy;
        self.waiting = waiting;
        self.done = done;
        Some(new_turn)
    }
}
