//! Correlate an outcome with the one model request this live loop emitted.
use std::collections::HashSet;
use std::io;

use crate::{core_events as ce, EventEnvelope, LogReader};

pub(super) struct PendingModel {
    source: String,
    after: u64,
    causes: Vec<String>,
}

impl PendingModel {
    pub(super) fn new(source: &str, after: u64, causes: &[&str]) -> Self {
        Self {
            source: source.into(),
            after,
            causes: causes.iter().map(|id| (*id).into()).collect(),
        }
    }

    pub(super) fn ends(&self, reader: &LogReader, outcome: &EventEnvelope) -> io::Result<bool> {
        if !ce::is_outcome(&outcome.event_type) {
            return Ok(false);
        }
        // Emission is buffered: the loop cannot know its request's assigned id
        // inside ask(). Match its own audited emission after that watermark,
        // with the exact causes it emitted. Do not infer identity from payloads
        // that a context gate may legitimately rewrite.
        let mut pending = outcome.causes.clone();
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let request = reader.header(&id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("missing model outcome ancestor {id}"),
                )
            })?;
            if request.seq <= self.after
                || request.event_type != ce::MODEL_CALL_STARTED
                || request.has_purpose
            {
                continue;
            }
            if request.source == self.source && request.causes == self.causes {
                // Ancestry locates the emission; the shared chronological
                // settlement rule decides whether this outcome still ends it.
                let mut calls = ce::PendingCalls::new(ce::MODEL_CALL_STARTED);
                reader.try_visit_header_range(
                    request.seq,
                    outcome.seq.saturating_sub(1),
                    |batch| {
                        for header in batch {
                            observe(&mut calls, header);
                        }
                        Ok(())
                    },
                )?;
                if !calls.requests().contains(&request.id) {
                    return Ok(false);
                }
                calls.observe(ce::EventRelations {
                    id: &outcome.id,
                    event_type: &outcome.event_type,
                    causes: &outcome.causes,
                });
                return Ok(!calls.requests().contains(&request.id));
            }
            // Only follow request copies. A completion/tool/input that caused
            // a later independent request is a boundary, not a forwarding hop.
            pending.extend(request.causes);
        }
        Ok(false)
    }
}

fn observe(calls: &mut ce::PendingCalls, header: &crate::kernel::log::Header) {
    if header.event_type != ce::MODEL_CALL_STARTED || !header.has_purpose {
        calls.observe(ce::EventRelations {
            id: &header.id,
            event_type: &header.event_type,
            causes: &header.causes,
        });
    }
}

/// Only unfinished calls are retained; completed identities do not accumulate.
/// This projection is checkpointed with material, never restored as live work.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelOutcomes {
    calls: ce::PendingCalls,
}

impl Default for ModelOutcomes {
    fn default() -> Self {
        Self {
            calls: ce::PendingCalls::new(ce::MODEL_CALL_STARTED),
        }
    }
}

impl ModelOutcomes {
    pub(super) fn accept(
        &mut self,
        reader: &LogReader,
        header: &crate::kernel::log::Header,
    ) -> io::Result<bool> {
        let mut model_target = false;
        let mut other_target = false;
        if matches!(
            header.event_type.as_str(),
            ce::MODEL_CALL_COMPLETED | ce::INTERRUPTED
        ) {
            for cause in &header.causes {
                if let Some(target) = reader.header(cause)? {
                    if target.event_type == ce::MODEL_CALL_STARTED {
                        model_target = true;
                    } else if header.event_type == ce::INTERRUPTED
                        && target.event_type == ce::TOOL_EXEC_STARTED
                    {
                        other_target = true;
                    }
                }
            }
        }
        let first = model_target
            && self.calls.requests().iter().any(|id| {
                ce::relation_ends_call(
                    ce::EventRelations {
                        id: &header.id,
                        event_type: &header.event_type,
                        causes: &header.causes,
                    },
                    id,
                )
            });
        observe(&mut self.calls, header);
        // Preserve legacy uncorrelated material and the tool portion of mixed
        // interruptions. Pure duplicate model outcomes cannot reenter history.
        Ok(!model_target || other_target || first)
    }
}
