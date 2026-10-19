//! Recorded input not yet carried by an ordinary model request.
use lattice::{core_events as ce, view::pending::PendingInputs, EventEnvelope};

#[cfg(test)]
#[path = "unseen/tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Unseen {
    pub pending: PendingInputs,
    lines: Vec<String>,
    // Only used when explicitly importing old state in compatibility readers.
    // On-disk terminal checkpoints are versioned and rebuilt from the ledger.
    legacy: Vec<String>,
}

impl Unseen {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    #[cfg(test)]
    pub fn restore(&mut self, lines: Vec<String>) {
        self.restore_at(lines, Default::default());
    }
    pub fn restore_at(&mut self, lines: Vec<String>, activity: lattice::view::activity::Activity) {
        self.pending = PendingInputs::with_activity(activity);
        self.legacy = lines.clone();
        self.lines = lines;
    }
    pub fn restore_pending(&mut self, pending: PendingInputs) {
        self.pending = pending;
        self.legacy.clear();
        self.lines = self.pending.lines();
    }
    pub fn snapshot(&self) -> PendingInputs {
        let mut pending = self.pending.clone();
        pending.clear_cards();
        pending
    }
    pub fn observe(
        &mut self,
        event: &EventEnvelope,
        reference: Option<&lattice::background_aliases::Reference>,
    ) {
        self.pending.observe(event);
        if event.event_type == ce::WAKE {
            if let Some(reference) = reference {
                self.pending.label(&event.id, format!("{reference} ↯"));
            }
        }
        self.lines = self
            .legacy
            .iter()
            .cloned()
            .chain(self.pending.lines())
            .collect();
    }
}
