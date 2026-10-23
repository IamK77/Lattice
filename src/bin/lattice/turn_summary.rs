//! Per-turn footer measurements. This projection owns no execution decisions.
//! PendingCalls is the authority for forwarded-call settlement; provider fields
//! share the decoder used by the existing usage panels.

use super::{accounting, event_inputs::stamp_millis};
use lattice::{
    core_events as ce,
    view::activity::{TokenCount, TurnSummary},
    EventEnvelope,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Total {
    known: Option<u64>,
    complete_calls: u64,
    overflowed: bool,
}

impl Total {
    fn add(&mut self, value: Option<u64>, complete: bool) {
        if let Some(value) = value {
            let sum = self.known.unwrap_or(0).checked_add(value);
            self.overflowed |= sum.is_none();
            self.known = Some(sum.unwrap_or(u64::MAX));
            self.complete_calls += u64::from(complete);
        }
    }

    fn count(&self, calls: u64) -> TokenCount {
        TokenCount {
            known: self.known,
            complete: calls > 0 && self.complete_calls == calls && !self.overflowed,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LiveCount {
    input: TokenCount,
    output: TokenCount,
    units: u64,
    measured_at: u64,
}

impl LiveCount {
    fn estimate(&self) -> Option<u64> {
        let units = self.units.saturating_sub(self.measured_at);
        (units > 0).then(|| {
            self.output
                .known
                .unwrap_or(0)
                .saturating_add(units.div_ceil(4))
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    turn: Option<String>,
    model: String,
    dialect: String,
    counted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Summary {
    id: Option<String>,
    started_ms: Option<i64>,
    ended_ms: Option<i64>,
    running: bool,
    calls: u64,
    input: Total,
    output: Total,
    pending: ce::PendingCalls,
    /// Forwarding copies reference one logical call; only pending copies live here.
    aliases: BTreeMap<String, String>,
    roots: BTreeMap<String, Call>,
    /// Transient notifications never become historical measurements.
    #[serde(skip)]
    live: BTreeMap<String, LiveCount>,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            id: None,
            started_ms: None,
            ended_ms: None,
            running: false,
            calls: 0,
            input: Total::default(),
            output: Total::default(),
            pending: ce::PendingCalls::new(ce::MODEL_CALL_STARTED),
            aliases: BTreeMap::new(),
            roots: BTreeMap::new(),
            live: BTreeMap::new(),
        }
    }
}

impl Summary {
    pub fn observe(
        &mut self,
        event: &EventEnvelope,
        new_turn: bool,
        ended: bool,
        model: &str,
        dialect: &str,
    ) {
        if new_turn {
            self.id = Some(event.id.clone());
            self.started_ms = stamp_millis(&event.time);
            self.ended_ms = None;
            self.running = true;
            self.calls = 0;
            self.input = Total::default();
            self.output = Total::default();
            self.live.clear();
        }
        match event.event_type.as_str() {
            ce::MODEL_CALL_STARTED if event.payload.get("purpose").is_none() => {
                if self.aliases.contains_key(&event.id) {
                    return;
                }
                let root = event
                    .causes
                    .iter()
                    .find_map(|cause| self.aliases.get(cause))
                    .cloned();
                let root = root.unwrap_or_else(|| {
                    // The loop may end an older call before asking about queued
                    // input. Resume that input's summary without moving its origin
                    // or reassigning any earlier call. Forwarding skips this branch.
                    if self.id.is_some() {
                        self.running = true;
                        self.ended_ms = None;
                        self.calls += 1;
                    }
                    self.roots.insert(
                        event.id.clone(),
                        Call {
                            turn: self.running.then(|| self.id.clone()).flatten(),
                            model: event.payload["model"].as_str().unwrap_or(model).to_owned(),
                            dialect: dialect.to_owned(),
                            counted: false,
                        },
                    );
                    event.id.clone()
                });
                self.aliases.insert(event.id.clone(), root);
                self.pending.observe(ce::EventRelations {
                    id: &event.id,
                    event_type: &event.event_type,
                    causes: &event.causes,
                });
            }
            ce::MODEL_CALL_COMPLETED | ce::INTERRUPTED => {
                self.pending.observe(ce::EventRelations {
                    id: &event.id,
                    event_type: &event.event_type,
                    causes: &event.causes,
                });
                let pending = self.pending.requests();
                for (root, call) in &mut self.roots {
                    if !call.counted && !pending.contains(root) {
                        call.counted = true;
                        self.live.remove(root);
                        if call.turn.is_some() && call.turn == self.id {
                            let (input, output) = if event.event_type == ce::MODEL_CALL_COMPLETED {
                                accounting::token_counts(&event.payload, &call.model, &call.dialect)
                            } else {
                                Default::default()
                            };
                            // Cancelled/error responses can contain useful measured
                            // prefixes, but are not proof of a complete bill.
                            let complete = event.event_type == ce::MODEL_CALL_COMPLETED
                                && event.payload["status"] == "ok";
                            self.input.add(input.known, complete && input.complete);
                            self.output.add(output.known, complete && output.complete);
                        }
                    }
                }
                self.aliases.retain(|id, _| pending.contains(id));
                self.roots
                    .retain(|root, _| self.aliases.values().any(|id| id == root));
            }
            _ => {}
        }
        if ended && self.running {
            self.running = false;
            self.ended_ms = stamp_millis(&event.time);
        }
    }

    pub fn notice(&mut self, payload: &serde_json::Value) {
        if payload.get("purpose").is_some() {
            return;
        }
        let Some(root) = payload["call"].as_str().and_then(|id| self.aliases.get(id)) else {
            // Untagged third-party notices cannot safely be assigned to a turn.
            return;
        };
        let Some(call) = self.roots.get(root) else {
            return;
        };
        if call.counted || call.turn.is_none() || call.turn != self.id {
            return;
        }
        let live = self.live.entry(root.clone()).or_default();
        if let Some(chunk) = payload["chunk"].as_str() {
            if payload.get("phase").is_none() || payload["phase"] == "reasoning" {
                // A deliberately rough, chunk-boundary-independent estimate:
                // four ASCII characters or one non-ASCII scalar per token.
                // Hidden reasoning and tool arguments are not guessed.
                for ch in chunk.chars() {
                    live.units = live.units.saturating_add(if ch.is_ascii() { 1 } else { 4 });
                }
            }
        }
        if payload.get("usage").is_some() {
            let (input, output) = accounting::token_counts(payload, &call.model, &call.dialect);
            if input.known.is_some() {
                live.input = input;
            }
            if output.known.is_some() {
                live.output = output;
                live.measured_at = live.units;
            }
        }
    }

    pub fn snapshot(&self, now_ms: i64) -> Option<TurnSummary> {
        self.id.as_ref()?;
        let end = if self.running {
            Some(now_ms)
        } else {
            self.ended_ms
        };
        let elapsed_ms = self
            .started_ms
            .zip(end)
            .map(|(start, end)| end.saturating_sub(start).max(0) as u64);
        let mut input = self.input.clone();
        let mut output = self.output.clone();
        let mut estimate = self.output.known.unwrap_or(0);
        let mut estimated = false;
        for live in self.live.values() {
            input.add(live.input.known, false);
            output.add(live.output.known, false);
            if let Some(value) = live.estimate() {
                estimated = true;
                estimate = estimate.saturating_add(value);
            } else {
                estimate = estimate.saturating_add(live.output.known.unwrap_or(0));
            }
        }
        Some(TurnSummary {
            elapsed_ms,
            input: input.count(self.calls),
            output: output.count(self.calls),
            output_estimate: estimated.then_some(estimate),
            running: self.running,
            model_pending: self
                .roots
                .values()
                .any(|call| !call.counted && call.turn.is_some() && call.turn == self.id),
        })
    }
}

/// Only the live UI owns monotonic time. Checkpoints contain event timestamps,
/// never process-local Instants or optimistic keypresses.
#[derive(Default)]
pub(super) struct Clock {
    submitted: VecDeque<Instant>,
    anchor: Option<(String, Instant)>,
    frozen_ms: Option<u64>,
}

impl Clock {
    pub fn submitted(&mut self, now: Instant) {
        self.submitted.push_back(now);
    }

    pub fn observe(&mut self, summary: &Summary, event: &EventEnvelope, now: Instant) {
        let Some(id) = &summary.id else {
            return;
        };
        let echoed = if event.event_type == ce::USER_MESSAGE && event.causes.is_empty() {
            self.submitted.pop_front()
        } else {
            None
        };
        if event.id == *id
            && self
                .anchor
                .as_ref()
                .is_none_or(|(previous, _)| previous != id)
        {
            self.anchor = Some((id.clone(), echoed.unwrap_or(now)));
            self.frozen_ms = None;
        }
        // A queued input can resume after the preceding call's turn boundary.
        // Keep its submission anchor, but discard the provisional frozen value.
        if summary.running {
            self.frozen_ms = None;
        }
        if !summary.running && self.frozen_ms.is_none() {
            if let Some((previous, start)) = &self.anchor {
                if previous == id {
                    self.frozen_ms = Some(elapsed(*start, now));
                }
            }
        }
    }

    pub fn snapshot(&self, summary: &Summary, now: Instant, now_ms: i64) -> Option<TurnSummary> {
        if let Some(start) = self.submitted.back() {
            return Some(TurnSummary {
                elapsed_ms: Some(elapsed(*start, now)),
                running: true,
                ..TurnSummary::default()
            });
        }
        let mut view = summary.snapshot(now_ms)?;
        if let Some((id, start)) = &self.anchor {
            if Some(id) == summary.id.as_ref() {
                view.elapsed_ms = Some(self.frozen_ms.unwrap_or_else(|| elapsed(*start, now)));
            }
        }
        Some(view)
    }
}

fn elapsed(start: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(start)
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
#[path = "turn_summary/tests.rs"]
mod tests;
