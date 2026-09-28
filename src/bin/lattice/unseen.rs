//! Recorded input not yet carried by a model request. This is neither an
//! unsubmitted draft nor a queue owned by the turn-activity state machine.

use lattice::{core_events as ce, EventEnvelope};

#[cfg(test)]
#[path = "unseen/tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Unseen {
    lines: Vec<String>,
}

impl Unseen {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn restore(&mut self, lines: Vec<String>) {
        self.lines = lines;
    }
    pub fn observe(&mut self, event: &EventEnvelope) {
        match event.event_type.as_str() {
            ce::USER_MESSAGE if event.causes.is_empty() => {
                if let Some(text) = event.payload["text"].as_str() {
                    self.lines.push(text.to_string());
                }
            }
            ce::WAKE => {
                let source = event.payload["source"].as_str().unwrap_or("wake");
                self.lines.push(format!("{source} ↯"));
            }
            // Unlike turn activity and usage, every request clears this list,
            // including requests with a purpose. Keep these rules independent.
            ce::MODEL_CALL_STARTED => self.lines.clear(),
            _ => {}
        }
    }
}
