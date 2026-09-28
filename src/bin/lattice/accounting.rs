//! Historical call accounting and context measurements. The coordinator owns
//! event order; fallible peak writes remain separate from pure measurement updates.
use super::event_inputs::{stamp_millis, EventInputs};
use super::material::accumulates;
use lattice::view::{
    facts::json_bytes,
    peaks::{GrowthSummary, Peaks, State as PeakState},
};
use lattice::{core_events as ce, EventEnvelope, LogReader, Material, Usage, UsageReport};
use serde_json::Value;
use std::{collections::HashMap, io};

#[cfg(test)]
#[path = "accounting/tests.rs"]
mod tests;

type Amounts = HashMap<&'static str, u64>;

/// Validated checkpoint measurements, without live controls or index resources.
#[derive(Default)]
pub(super) struct Measurements {
    usage: Option<Usage>,
    turn_usage: Usage,
    session_usage: Usage,
    parts: Material,
    growth: Growth,
}

impl Measurements {
    pub fn restored(
        usage: Option<Usage>,
        turn_usage: Usage,
        session_usage: Usage,
        parts: Material,
        previous: Amounts,
        turn_growth: Amounts,
        session_growth: Amounts,
    ) -> Self {
        Self {
            usage,
            turn_usage,
            session_usage,
            parts,
            growth: Growth {
                previous,
                turn: turn_growth,
                session: session_growth,
            },
        }
    }
}

#[derive(Default)]
pub(super) struct Accounting {
    measurements: Measurements,
    history: Vec<(u64, u64)>,
    peaks: Option<Peaks>,
}

impl Accounting {
    pub fn last_call(&self) -> Option<Usage> {
        self.measurements.usage
    }
    pub fn turn_total(&self) -> Usage {
        self.measurements.turn_usage
    }
    pub fn session_total(&self) -> Usage {
        self.measurements.session_usage
    }
    pub fn parts(&self) -> &Material {
        &self.measurements.parts
    }
    pub fn previous(&self) -> &Amounts {
        &self.measurements.growth.previous
    }
    pub fn turn_growth(&self) -> &Amounts {
        &self.measurements.growth.turn
    }
    pub fn session_growth(&self) -> &Amounts {
        &self.measurements.growth.session
    }
    pub fn report(&self) -> Option<UsageReport> {
        self.last_call().map(|call| UsageReport {
            call,
            turn: self.turn_total(),
            session: self.session_total(),
        })
    }
    pub fn growth(&self) -> (Material, Material) {
        (
            Growth::sorted(self.turn_growth()),
            Growth::sorted(self.session_growth()),
        )
    }
    /// The indexed path intentionally does not load all old peaks for a frame.
    pub fn history(&self) -> Vec<u64> {
        self.history.iter().map(|(_, peak)| *peak).collect()
    }
    pub fn history_growth(&self) -> GrowthSummary {
        self.peaks.as_ref().map_or_else(
            || GrowthSummary::from_values(&self.history()),
            Peaks::growth,
        )
    }
    pub fn has_peaks(&self) -> bool {
        self.peaks.is_some()
    }

    pub fn new_turn(&mut self) {
        self.measurements.turn_usage = Usage::default();
        self.measurements.growth.new_turn();
    }
    pub fn invalidate_context_measurement(&mut self) {
        self.measurements.usage = None;
    }
    /// Install only validated measurements. Existing history resources survive.
    pub fn restore(&mut self, measurements: Measurements) {
        self.measurements = measurements;
    }

    pub fn bind_peaks(&mut self, reader: &LogReader) -> io::Result<()> {
        if self.peaks.is_none() {
            let mut peaks = Peaks::open(reader, PeakState::default())?;
            for (turn, value) in &self.history {
                peaks.record(*turn, *value)?;
            }
            self.peaks = Some(peaks);
            self.history = Vec::new();
        }
        Ok(())
    }
    /// The caller has already bound the persistent index before this stage.
    pub fn record_indexed_peak(&mut self, turn: u64, prompt: u64) -> io::Result<()> {
        self.peaks
            .as_mut()
            .expect("peak index must be bound before recording")
            .record(turn, prompt)
    }
    pub fn snapshot_peaks(&mut self) -> io::Result<PeakState> {
        self.peaks
            .as_mut()
            .expect("peak index must be bound before saving")
            .snapshot()
    }
    pub fn fork_peaks(&mut self, reader: &LogReader) -> io::Result<Peaks> {
        self.peaks
            .as_mut()
            .expect("peak index must be bound before copying")
            .fork(reader)
    }
    /// Used after checkpoint and index validation; never replays old measurements.
    pub fn install_peaks(&mut self, peaks: Peaks) {
        self.peaks = Some(peaks);
    }
    pub fn discard_reference_history(&mut self) {
        self.history = Vec::new();
    }

    pub fn observe_prompt(&mut self, event: &EventEnvelope, inputs: &EventInputs) {
        let mut by_kind: Amounts = HashMap::new();
        let sized = |v: Option<&Value>| -> u64 {
            let Some(v) = v else { return 0 };
            if let Some(reference) = lattice::contracts::document::DocRef::of(v) {
                return reference.bytes;
            }
            json_bytes(v)
        };
        *by_kind.entry("system prompt").or_default() += sized(event.payload.get("system"));
        *by_kind.entry("tool declarations").or_default() += sized(event.payload.get("tools"));
        let parts = event.payload["input"]["parts"]
            .as_array()
            .or_else(|| event.payload["input"].as_array());
        for part in parts.unwrap_or(&Vec::new()) {
            if let Some(id) = part.get("event").and_then(Value::as_str) {
                for key in [id.to_string(), format!("{id}#thinking")] {
                    if let Some((kind, bytes)) = inputs.size(key.as_str()) {
                        *by_kind.entry(kind).or_default() += bytes;
                    }
                }
            } else if part.get("digest").is_some() {
                *by_kind.entry("condensed summaries").or_default() += json_bytes(part);
            } else {
                *by_kind.entry("other events").or_default() += json_bytes(part);
            }
        }
        let mut parts: Material = by_kind.into_iter().filter(|(_, b)| *b > 0).collect();
        parts.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
        self.measurements.growth.absorb(&parts);
        self.measurements.parts = parts;
    }

    pub fn record_completion(&mut self, turn: u64, usage: Usage) {
        if self.peaks.is_none() {
            match self.history.last_mut() {
                Some((previous, peak)) if *previous == turn => *peak = (*peak).max(usage.prompt),
                _ => self.history.push((turn, usage.prompt)),
            }
        }
        self.measurements.usage = Some(usage);
        self.measurements.turn_usage.add(&usage);
        self.measurements.session_usage.add(&usage);
    }

    #[cfg(test)]
    pub fn reference_history(&self) -> &[(u64, u64)] {
        &self.history
    }
    #[cfg(test)]
    pub fn indexed_history(&self) -> io::Result<Vec<(u64, u64)>> {
        let peaks = self.peaks.as_ref().expect("test requires a peak index");
        peaks.load(0..peaks.len())
    }
    #[cfg(test)]
    pub fn seed_last_call(&mut self, usage: Option<Usage>) {
        self.measurements.usage = usage;
    }
    #[cfg(test)]
    pub fn seed_turn_total(&mut self, usage: Usage) {
        self.measurements.turn_usage = usage;
    }
    #[cfg(test)]
    pub fn seed_session_total(&mut self, usage: Usage) {
        self.measurements.session_usage = usage;
    }
    #[cfg(test)]
    pub fn seed_parts(&mut self, parts: Material) {
        self.measurements.parts = parts;
    }
    #[cfg(test)]
    pub fn seed_history(&mut self, history: Vec<(u64, u64)>) {
        self.history = history;
    }
}

/// Preflight is read-only: the request pairing is consumed by the later fold,
/// including a completion without usage. Dialect still comes from the current model.
pub(super) fn completed_usage(
    event: &EventEnvelope,
    inputs: &EventInputs,
    fallback_model: &str,
    dialect: &str,
) -> Option<Usage> {
    if event.event_type != ce::MODEL_CALL_COMPLETED || event.payload.get("purpose").is_some() {
        return None;
    }
    let begun = inputs.model_start(&event.causes);
    let model = begun
        .map(|(model, _)| model.as_str())
        .unwrap_or(fallback_model);
    let mut usage = read_usage(&event.payload, model, dialect)?;
    if let (Some((_, began)), Some(ended)) = (begun, stamp_millis(&event.time)) {
        usage.millis = (ended - began).max(0) as u64;
    }
    Some(usage)
}

/// Provider count parsing is also used for expert progress, without adding that
/// progress to this conversation's accumulated usage.
pub(super) fn read_usage(payload: &Value, model: &str, dialect: &str) -> Option<Usage> {
    let usage = payload.get("usage")?;
    let named = lattice::profile::usage_fields(model);
    let at = |path: &str| -> u64 {
        let mut cur = usage;
        for step in path.split('.') {
            match cur.get(step) {
                Some(next) => cur = next,
                None => return 0,
            }
        }
        cur.as_u64().unwrap_or(0)
    };
    let get = |role: &str, fallbacks: &[&str]| -> u64 {
        if let Some(declared) = named.get(role).and_then(Value::as_str) {
            let found = at(declared);
            if found > 0 {
                return found;
            }
        }
        fallbacks
            .iter()
            .map(|f| at(f))
            .find(|n| *n > 0)
            .unwrap_or(0)
    };
    let input = get("input", &["input_tokens", "prompt_tokens"]);
    let cached = get(
        "cacheRead",
        &[
            "cache_read_input_tokens",
            "prompt_cache_hit_tokens",
            "prompt_tokens_details.cached_tokens",
            "input_tokens_details.cached_tokens",
        ],
    );
    let written = get("cacheWrite", &["cache_creation_input_tokens"]);
    let prompt = if dialect == "anthropic" {
        input + cached + written
    } else {
        input.max(cached)
    };
    Some(Usage {
        prompt,
        cached,
        written,
        output: get("output", &["output_tokens", "completion_tokens"]),
        reasoning: get(
            "reasoning",
            &[
                "completion_tokens_details.reasoning_tokens",
                "output_tokens_details.reasoning_tokens",
            ],
        ),
        millis: 0,
        calls: 1,
    })
}

/// Growth counts positive deltas per material kind, not repeated snapshots.
/// Missing kinds retain their last measurement; static instructions do not accrue.
#[derive(Debug, Clone, Default)]
struct Growth {
    previous: Amounts,
    turn: Amounts,
    session: Amounts,
}

impl Growth {
    fn absorb(&mut self, parts: &[(&'static str, u64)]) {
        for (kind, now) in parts {
            if !accumulates(kind) {
                continue;
            }
            let before = self.previous.get(kind).copied().unwrap_or(0);
            let gained = now.saturating_sub(before);
            if gained > 0 {
                *self.turn.entry(kind).or_default() += gained;
                *self.session.entry(kind).or_default() += gained;
            }
        }
        for (kind, now) in parts {
            self.previous.insert(kind, *now);
        }
    }
    fn new_turn(&mut self) {
        self.turn.clear();
    }
    fn sorted(map: &Amounts) -> Material {
        let mut out: Material = map.iter().map(|(k, v)| (*k, *v)).collect();
        out.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
        out
    }
}
