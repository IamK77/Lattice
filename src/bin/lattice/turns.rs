//! Recorded turn boundaries and local activity have separate lifetimes.
//! Unrelated events never overwrite optimistic activity or a live quiet notice.

use lattice::{components::minimal_loop, core_events as ce, EventEnvelope};

#[path = "turns/completion.rs"]
mod completion;
pub(super) use completion::Completion;

#[cfg(test)]
#[path = "turns/tests.rs"]
mod tests;

/// Pure data for the checkpoint adapter; not a mutable view into the owner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct History {
    pub busy: bool,
    pub waiting: bool,
    pub done: bool,
    pub number: usize,
    pub completion: Completion,
}

impl History {
    /// None leaves local activity untouched. Some reports a recorded boundary
    /// and whether it starts a new turn, without introducing a display clock.
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

#[derive(Default)]
pub(super) struct Turns {
    history: History,
    busy: bool,
    done_at: Option<usize>,
    optimistic: bool,
    ended: bool,
}

impl Turns {
    pub fn busy(&self) -> bool {
        self.busy
    }
    pub fn waiting(&self) -> bool {
        self.history.waiting
    }
    pub fn done_at(&self) -> Option<usize> {
        self.done_at
    }
    pub fn number(&self) -> usize {
        self.history.number
    }
    pub fn history(&self) -> History {
        self.history.clone()
    }
    pub fn status(&self) -> &'static str {
        self.history.completion.outcome.label()
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    pub fn lands_live_text(&self, event: &EventEnvelope) -> bool {
        self.history.completion.lands_live_text(event)
    }
    pub fn interrupted(&self) -> bool {
        self.history.completion.outcome != completion::Outcome::Done
    }
    pub fn restore(&mut self, history: History, settled_tick: usize) {
        self.busy = history.busy;
        self.done_at = history.done.then_some(settled_tick);
        self.history = history;
        self.optimistic = false;
        self.ended = false;
    }
    /// Side-conversation submission only marks activity; it has never reset Done.
    pub fn activate(&mut self) {
        self.busy = true;
    }
    /// Local operations such as model switching finish on a quiet notice.
    pub fn optimistic_activity(&mut self) {
        self.activate();
        self.done_at = None;
    }
    /// Chat input must return through the ledger before old notices can settle it.
    pub fn submitted_input(&mut self) {
        self.optimistic_activity();
        self.optimistic = true;
    }
    pub fn quiescent(&mut self, tick: usize) {
        // An uncorrelated quiet notice cannot settle an audited active turn,
        // nor the next input optimistically submitted ahead of the event queue.
        if self.history.busy || self.optimistic {
            return;
        }
        if self.busy {
            self.done_at = Some(tick);
        }
        self.busy = false;
    }

    /// Returns whether the coordinator must clear the receipt and turn tally.
    /// Only an accepted turn boundary retires live text, not each interruption.
    pub fn observe(&mut self, event: &EventEnvelope, tick: usize) -> bool {
        self.ended = false;
        let Some(new_turn) = self.history.observe(event) else {
            return false;
        };
        self.ended = event.event_type == ce::TURN_COMPLETED;
        let input_echo = event.event_type == ce::USER_MESSAGE && event.causes.is_empty();
        if self.optimistic && !input_echo {
            // A queued wake is a recorded boundary, not a chat-input echo.
            // Keep its bookkeeping without letting old events settle the input.
            return new_turn;
        }
        self.optimistic = false;
        self.busy = self.history.busy;
        self.done_at = self.history.done.then_some(tick);
        new_turn
    }

    #[cfg(test)]
    pub fn seed_busy(&mut self, busy: bool) {
        self.busy = busy;
    }
    #[cfg(test)]
    pub fn seed_done_at(&mut self, done_at: Option<usize>) {
        self.done_at = done_at;
    }
    #[cfg(test)]
    pub fn seed_number(&mut self, number: usize) {
        self.history.number = number;
    }
}
