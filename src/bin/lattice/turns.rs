//! Recorded turn boundaries and local activity have separate lifetimes.
//! Unrelated events never overwrite optimistic activity or a live quiet notice.

use lattice::{components::minimal_loop, core_events as ce, EventEnvelope};

#[cfg(test)]
#[path = "turns/tests.rs"]
mod tests;

/// Pure data for the checkpoint adapter; not a mutable view into the owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct History {
    pub busy: bool,
    pub waiting: bool,
    pub done: bool,
    pub number: usize,
}

impl History {
    /// None leaves local activity untouched. Some reports a recorded boundary
    /// and whether it starts a new turn, without introducing a display clock.
    pub fn observe(&mut self, event: &EventEnvelope) -> Option<bool> {
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
            ce::TURN_COMPLETED => (false, false, true, false),
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
        self.history
    }
    pub fn restore(&mut self, history: History, settled_tick: usize) {
        self.history = history;
        self.busy = history.busy;
        self.done_at = history.done.then_some(settled_tick);
    }
    /// Side-conversation submission only marks activity; it has never reset Done.
    pub fn activate(&mut self) {
        self.busy = true;
    }
    pub fn optimistic_activity(&mut self) {
        self.activate();
        self.done_at = None;
    }
    pub fn quiescent(&mut self, tick: usize) {
        if self.busy {
            self.done_at = Some(tick);
        }
        self.busy = false;
    }

    /// Returns whether the coordinator must clear the receipt and turn tally.
    /// Interruptions clear live text elsewhere, but are not a turn boundary.
    pub fn observe(&mut self, event: &EventEnvelope, tick: usize) -> bool {
        let Some(new_turn) = self.history.observe(event) else {
            return false;
        };
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
