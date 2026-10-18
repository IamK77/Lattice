//! Uncommitted model text. These buffers are never checkpointed; committed
//! cards supersede one channel at a time, while a stopped round clears both.

use lattice::core_events as ce;
use serde_json::Value;

#[cfg(test)]
#[path = "live_output/tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct LiveOutput {
    reply: String,
    thinking: String,
}

impl LiveOutput {
    pub fn reply(&self) -> &str {
        &self.reply
    }
    pub fn thinking(&self) -> &str {
        &self.thinking
    }

    /// A non-streaming notice remains the coordinator's input receipt. A chunk
    /// wins over a note even when its phase is unknown. Any purpose excludes it.
    pub fn notice<'a>(&mut self, payload: &'a Value) -> Option<&'a str> {
        if payload.get("purpose").is_some() {
            return None;
        }
        if let Some(chunk) = payload["chunk"].as_str() {
            if payload["phase"] == "reasoning" {
                self.thinking.push_str(chunk);
            } else if payload.get("phase").is_none() {
                self.reply.push_str(chunk);
            }
            None
        } else {
            payload["note"].as_str()
        }
    }

    /// A completion can commit both channels, or only one. Never use the last
    /// card's kind to infer what happened to the other channel.
    pub fn card_landed(&mut self, channels: lattice::view::CommittedText) {
        if channels.thinking {
            self.thinking.clear();
        }
        if channels.reply {
            self.reply.clear();
        }
    }

    /// Retire actual uncommitted text into local display cards, never the ledger.
    pub fn take_interrupted(&mut self) -> (String, String) {
        (
            std::mem::take(&mut self.thinking),
            std::mem::take(&mut self.reply),
        )
    }

    pub fn round_event(&mut self, event_type: &str) {
        if matches!(
            event_type,
            ce::TURN_COMPLETED | lattice::components::minimal_loop::WAITING
        ) {
            self.thinking.clear();
            self.reply.clear();
        }
    }

    #[cfg(test)]
    pub fn seed_reply(&mut self, value: String) {
        self.reply = value;
    }
    #[cfg(test)]
    pub fn seed_thinking(&mut self, value: String) {
        self.thinking = value;
    }
}
