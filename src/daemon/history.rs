//! A bounded current-state projection, independent of display history pages.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventDraft, EventLog};
    use serde_json::json;

    #[test]
    fn old_checkpoints_cannot_hide_operation_questions() {
        let dir = tempfile::tempdir().unwrap();
        let mut declarations = ce::core_event_decls();
        declarations.extend(crate::components::operation_policy::manifest().events);
        let mut log = EventLog::open_segmented(
            declarations,
            "operations",
            dir.path().join("history.ledger"),
            4096,
        )
        .unwrap();
        let request = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[],
                    json!({"call":"test","tool":"Run","arguments":{"command":"git status"}}),
                ),
                "loop",
            )
            .unwrap();
        let question = log.append(EventDraft::new(crate::components::operation_policy::AUTH_REQUESTED, &[&request.id],
            json!({"request":request.id,"held":request.id,"tool":"Run","summary":"test","grants":[]})), "operations").unwrap();
        // Version one did not recognize operation questions but could advance
        // its cursor past them. Reusing it would permanently lose this card.
        let old = Projection {
            through: question.seq,
            state: StreamState::default(),
        };
        log.reader()
            .save_checkpoint("daemon-current-state", 1, old.through, &old)
            .unwrap();
        let recovered = Projection::recover(&log.reader()).unwrap();
        assert_eq!(recovered.state.pending_auth.len(), 1);
        assert_eq!(recovered.state.pending_auth[0].request, question.id);
        log.append(
            EventDraft::new(
                crate::components::operation_policy::DECISION,
                &[&request.id],
                json!({"held":request.id,"verdict":"denied"}),
            )
            .with_reason("The user refused"),
            "operations",
        )
        .unwrap();
        assert!(Projection::recover(&log.reader())
            .unwrap()
            .state
            .pending_auth
            .is_empty());
    }

    #[test]
    fn recovery_matches_full_fold_and_pages_obey_both_limits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("history.ledger");
        let mut log =
            EventLog::open_segmented(ce::core_event_decls(), "history", root, 4096).unwrap();
        for index in 0..300 {
            log.append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": index.to_string()})),
                "ui",
            )
            .unwrap();
        }
        let reader = log.reader();
        let cold = Projection::recover(&reader).unwrap();
        let before = reader.memory_stats().unwrap().cache.unwrap();
        let warm = Projection::recover(&reader).unwrap();
        let after = reader.memory_stats().unwrap().cache.unwrap();
        assert_eq!(
            before.hits + before.decodes,
            after.hits + after.decodes,
            "warm state recovery must not revisit old bodies"
        );
        assert_eq!(cold.through, 300);
        assert_eq!(cold.state, warm.state);
        assert!(warm.state.busy);
        let page = reader.page_before(301, 3, u64::MAX).unwrap();
        assert_eq!(
            page.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![298, 299, 300]
        );
        assert_eq!(
            reader.page_before(301, 128, 1).unwrap().len(),
            1,
            "one oversized record travels alone"
        );
        assert!(reader.page_before(1, 128, 1024).unwrap().is_empty());
        assert!(reader.page_before(302, 128, 1024).is_err());
        log.append(EventDraft::new(ce::TURN_COMPLETED, &[], json!({})), "loop")
            .unwrap();
        let tail = Projection::recover(&reader).unwrap();
        assert_eq!(tail.through, 301);
        assert!(!tail.state.busy);
    }
}

use super::protocol::{PendingAuthorization, StreamState};
use crate::{core_events as ce, EventEnvelope, LogReader};
use serde::{Deserialize, Serialize};
use std::io;

pub const CAPABILITY: &str = "history-pages";
pub const PAGE_EVENTS: usize = 128;
pub const PAGE_BYTES: u64 = 128 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub through: u64,
    pub state: StreamState,
}

impl Projection {
    pub fn recover(reader: &LogReader) -> io::Result<Self> {
        let end = reader.snapshot_end();
        let checkpoint = reader.load_checkpoint::<Self>("daemon-current-state", 2, end)?;
        let mut projection = checkpoint
            .state
            .filter(|state| state.through == checkpoint.through)
            .unwrap_or_default();
        if let Some(reason) = checkpoint.cold_reason {
            eprintln!("slow recovery for daemon current state: {reason}");
        }
        reader.visit_range(projection.through + 1, end, |events| {
            for event in events {
                projection.observe(event)?;
            }
            Ok(())
        })?;
        projection.save(reader);
        Ok(projection)
    }

    pub fn observe(&mut self, event: &EventEnvelope) -> io::Result<()> {
        if event.seq != self.through + 1 {
            return Err(io::Error::other(
                "daemon state event is outside its next prefix",
            ));
        }
        let phase = match event.event_type.as_str() {
            ce::USER_MESSAGE if event.causes.is_empty() => Some(true),
            ce::WAKE => Some(true),
            ce::MODEL_CALL_STARTED if event.payload.get("purpose").is_none() => Some(true),
            "loop.waiting" | ce::TURN_COMPLETED => Some(false),
            _ => None,
        };
        if let Some(busy) = phase {
            self.state.busy = busy;
            self.state.waiting = false;
        }
        if event.event_type == "loop.waiting" {
            self.state.waiting = true;
        }
        match event.event_type.as_str() {
            "operation.authorization_requested"
            | "trust.authorization_requested"
            | "browser.authorization_requested"
            | "experts.authorization_requested" => {
                if !self
                    .state
                    .pending_auth
                    .iter()
                    .any(|card| card.request == event.id)
                {
                    self.state.pending_auth.push(PendingAuthorization {
                        request: event.id.clone(),
                        held: event.payload["held"]
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| event.causes.first().cloned()),
                    });
                }
            }
            "operation.authorization_decided"
            | "trust.gate.decision"
            | "browser.authorization_decided"
            | "experts.authorization_decided"
            | ce::INTERRUPTED
            | ce::TOOL_EXEC_COMPLETED => {
                self.state.pending_auth.retain(|card| {
                    !card.held.as_ref().is_some_and(|held| {
                        event.causes.contains(held) || event.payload["held"].as_str() == Some(held)
                    })
                });
            }
            "skill.listing" => {
                self.state.skills = event.payload["skills"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            _ => {}
        }
        self.through = event.seq;
        Ok(())
    }

    pub fn save(&self, reader: &LogReader) {
        if let Err(error) = reader.save_checkpoint("daemon-current-state", 2, self.through, self) {
            eprintln!("cannot save daemon current state: {error}");
        }
    }
}
