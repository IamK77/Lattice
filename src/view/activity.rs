//! Recorded activity shared by live and recovered display projections.
use crate::{components::minimal_loop, core_events as ce, EventEnvelope};
use serde::{Deserialize, Serialize};

mod completion;
pub use completion::{Completion, Outcome};

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
