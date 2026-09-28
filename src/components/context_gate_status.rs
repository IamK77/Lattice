//! Read-only presentation of the same committed observation used by the gate.
//! This reader never writes the gate's checkpoint or changes an active component.
use std::io;

use serde::{Deserialize, Serialize};

use super::{recovery, DECISION};
use crate::LogReader;

#[cfg(test)]
#[path = "context_gate/status_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionFailure {
    pub event: String,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionStatus {
    pub in_flight: bool,
    pub failure: Option<CompactionFailure>,
}

#[derive(Default)]
pub struct CompactionObserver {
    recovery: recovery::Recovery,
    failure: Option<CompactionFailure>,
}

impl CompactionObserver {
    pub fn status_at(
        &mut self,
        reader: &LogReader,
        through: u64,
    ) -> io::Result<Option<CompactionStatus>> {
        let same_source = self.recovery.matches(reader);
        if !same_source {
            self.failure = None;
        }
        let (reported, in_flight, failure) =
            self.recovery.read(reader, through, false, |state| {
                (
                    state.condense_completion.is_some() || state.in_flight(),
                    state.in_flight(),
                    state.condense_failure.clone(),
                )
            })?;
        if !same_source || self.failure.as_ref().map(|failure| &failure.event) != failure.as_ref() {
            let detail = if let Some(id) = failure {
                let event = reader.get(&id)?.ok_or_else(|| {
                    io::Error::other(format!("compaction failure event missing: {id}"))
                })?;
                let (code, message) = if event.event_type == DECISION {
                    (
                        "summary_rejected",
                        event
                            .reason
                            .as_deref()
                            .unwrap_or("Compaction result was not adopted"),
                    )
                } else {
                    (
                        event.payload["error"]["code"]
                            .as_str()
                            .unwrap_or("compaction_failed"),
                        event.payload["error"]["message"]
                            .as_str()
                            .unwrap_or("Compaction failed; inspect the recorded event for details"),
                    )
                };
                Some(CompactionFailure {
                    event: id,
                    code: bounded(code, 100),
                    message: bounded(message, 2000),
                })
            } else {
                None
            };
            self.failure = detail;
        }
        Ok(
            (reported || self.failure.is_some()).then(|| CompactionStatus {
                in_flight,
                failure: self.failure.clone(),
            }),
        )
    }
}

fn bounded(text: &str, limit: usize) -> String {
    let mut chars = text.chars().filter(|c| !c.is_control() || *c == '\n');
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push_str("… [see recorded event for the full error]");
    }
    result
}
