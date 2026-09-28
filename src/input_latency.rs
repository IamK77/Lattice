//! Bounded TUI submission-to-frame observations. No input text is retained.
//! Correlation is FIFO for this frontend's own root user messages in one stream.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::Instant;

const LIMIT: usize = 64;
pub fn clock_ns() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub event: String,
    pub pid: u32,
    pub submitted_at_ns: u64,
    pub notified_ms: f64,
    pub ui_received_ms: f64,
    pub absorbed_ms: f64,
    pub first_frame_ms: f64,
    pub draw_ms: f64,
    pub dropped_observations: u64,
}

struct Receipt {
    event: String,
    submitted: u64,
    notified: u64,
    received: u64,
    absorbed: Option<u64>,
}

#[derive(Default)]
pub struct Tracker {
    pending: VecDeque<u64>,
    skipped: u64,
    awaiting_frame: VecDeque<Receipt>,
    dropped: u64,
}
impl Tracker {
    pub fn submitted(&mut self, at: u64) {
        if self.pending.len() == LIMIT {
            self.pending.pop_front();
            self.skipped += 1;
            self.dropped += 1;
        }
        self.pending.push_back(at);
    }

    pub fn notified(&mut self, event: String, pid: u32, at: u64, received: u64) {
        if self.skipped > 0 {
            self.skipped -= 1;
            return;
        }
        let Some(submitted) = self.pending.pop_front() else {
            return;
        };
        if pid != std::process::id() || at < submitted || received < at {
            self.dropped += 1;
            return;
        }
        if self.awaiting_frame.len() == LIMIT {
            self.awaiting_frame.pop_front();
            self.dropped += 1;
        }
        self.awaiting_frame.push_back(Receipt {
            event,
            submitted,
            notified: at,
            received,
            absorbed: None,
        });
    }

    pub fn absorbed(&mut self, event: &str, at: u64) {
        if let Some(index) = self.awaiting_frame.iter().position(|r| r.event == event) {
            if at < self.awaiting_frame[index].received {
                self.awaiting_frame.remove(index);
                self.dropped += 1;
            } else {
                self.awaiting_frame[index].absorbed = Some(at);
            }
        }
    }

    /// Called only after a successful frame. A failed draw produces no receipt.
    pub fn drawn(&mut self, began: u64, ended: u64) -> Vec<Note> {
        if ended < began {
            return Vec::new();
        }
        let mut notes = Vec::new();
        self.awaiting_frame.retain(|r| {
            let Some(absorbed) = r.absorbed.filter(|at| *at <= began) else {
                return true;
            };
            let ms = |at: u64| at.saturating_sub(r.submitted) as f64 / 1_000_000.0;
            notes.push(Note {
                event: r.event.clone(),
                pid: std::process::id(),
                submitted_at_ns: r.submitted,
                notified_ms: ms(r.notified),
                ui_received_ms: ms(r.received),
                absorbed_ms: ms(absorbed),
                first_frame_ms: ms(ended),
                draw_ms: ended.saturating_sub(began) as f64 / 1_000_000.0,
                dropped_observations: self.dropped,
            });
            false
        });
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separates_notification_queue_absorption_and_first_frame_without_sleeping() {
        let mut trace = Tracker::default();
        trace.submitted(1_000_000);
        trace.notified("root".into(), std::process::id(), 3_000_000, 8_000_000);
        assert!(trace.drawn(9_000_000, 10_000_000).is_empty());
        trace.absorbed("forwarded", 10_000_000);
        assert!(trace.drawn(11_000_000, 12_000_000).is_empty());
        trace.absorbed("root", 13_000_000);
        let notes = trace.drawn(15_000_000, 20_000_000);
        assert_eq!(notes.len(), 1);
        let n = &notes[0];
        assert_eq!(
            (
                n.notified_ms,
                n.ui_received_ms,
                n.absorbed_ms,
                n.first_frame_ms,
                n.draw_ms
            ),
            (2.0, 7.0, 12.0, 19.0, 5.0)
        );
        assert!(trace.drawn(21_000_000, 22_000_000).is_empty());
    }

    #[test]
    fn invalid_clocks_are_not_reported_as_zero_latency_or_paired_again() {
        let mut trace = Tracker::default();
        trace.submitted(10);
        trace.notified("backwards".into(), std::process::id(), 9, 20);
        trace.submitted(30);
        trace.notified("foreign".into(), 0, 40, 50);
        trace.submitted(60);
        trace.notified("valid".into(), std::process::id(), 70, 80);
        trace.absorbed("valid", 90);
        assert!(trace.drawn(110, 100).is_empty());
        let notes = trace.drawn(110, 120);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].event, "valid");
        assert_eq!(notes[0].submitted_at_ns, 60);
        assert_eq!(notes[0].dropped_observations, 2);
    }

    #[test]
    fn overflow_does_not_pair_old_notifications_with_new_submissions() {
        let mut trace = Tracker::default();
        for at in 0..1000 {
            trace.submitted(at);
        }
        assert_eq!(trace.pending.len(), LIMIT);
        for at in 0..1000 {
            trace.notified(at.to_string(), std::process::id(), 1000, 1000);
            trace.absorbed(&at.to_string(), 1000);
        }
        assert!(trace.pending.is_empty());
        let notes = trace.drawn(1001, 1002);
        assert_eq!(notes.len(), LIMIT);
        assert_eq!(notes[0].event, "936");
        assert_eq!(notes[0].submitted_at_ns, 936);
        assert_eq!(notes[0].dropped_observations, 936);
    }
}
