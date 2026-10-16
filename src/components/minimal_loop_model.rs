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
    owner: String,
    calls: ce::PendingCalls,
    owned: std::collections::BTreeSet<String>,
}

impl Default for ModelOutcomes {
    fn default() -> Self {
        Self::new("")
    }
}

impl ModelOutcomes {
    pub(super) fn new(owner: &str) -> Self {
        Self {
            owner: owner.into(),
            calls: ce::PendingCalls::new(ce::MODEL_CALL_STARTED),
            owned: Default::default(),
        }
    }

    pub(super) fn owner(&self) -> &str {
        &self.owner
    }

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
        if header.event_type == ce::MODEL_CALL_STARTED
            && !header.has_purpose
            && header.source == self.owner
        {
            self.owned.insert(header.id.clone());
        }
        observe(&mut self.calls, header);
        // A sibling copy can remain pending after the owned original ended.
        // Keep the shared settlement graph intact, but admit material only
        // when this outcome actually settles one of THIS loop's originals.
        let first = if ce::is_outcome(&header.event_type) {
            let pending: HashSet<_> = self.calls.requests().into_iter().collect();
            let before = self.owned.len();
            self.owned.retain(|id| pending.contains(id));
            self.owned.len() < before
        } else {
            false
        };
        // Preserve legacy uncorrelated material and the tool portion of mixed
        // interruptions. Pure duplicate model outcomes cannot reenter history.
        Ok(!model_target || other_target || first)
    }
}
