//! Startup measurements. Durations use a monotonic clock; wall time is only a label.
use std::collections::BTreeMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Consecutive, non-overlapping phases within one measured span.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    pub total_ms: f64,
    pub phases_ms: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<crate::memory::Checkpoint>,
}

pub struct PhaseTimer {
    began: Instant,
    previous: Instant,
    phases: BTreeMap<String, f64>,
    memory: Vec<crate::memory::Checkpoint>,
}

impl PhaseTimer {
    pub fn start() -> Self {
        Self::since(Instant::now())
    }

    /// Start at a previously captured boundary, such as a stop request.
    pub fn since(began: Instant) -> Self {
        Self {
            began,
            previous: began,
            phases: BTreeMap::new(),
            memory: vec![crate::memory::Checkpoint {
                phase: "start".into(),
                memory: crate::memory::Snapshot::capture(),
            }],
        }
    }

    pub fn checkpoint(&mut self, phase: &str) {
        self.checkpoint_at(phase, Instant::now());
        self.memory.push(crate::memory::Checkpoint {
            phase: phase.into(),
            memory: crate::memory::Snapshot::capture(),
        });
        self.checkpoint_at("memory_sampling", Instant::now());
    }

    fn checkpoint_at(&mut self, phase: &str, now: Instant) {
        *self.phases.entry(phase.to_string()).or_default() +=
            now.duration_since(self.previous).as_secs_f64() * 1000.0;
        self.previous = now;
    }

    pub fn finish(mut self, last_phase: &str) -> Timings {
        self.checkpoint(last_phase);
        self.measured()
    }

    fn measured(self) -> Timings {
        Timings {
            total_ms: self.previous.duration_since(self.began).as_secs_f64() * 1000.0,
            phases_ms: self.phases,
            memory: self.memory,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn old_timings_remain_readable_and_new_boundaries_include_memory() {
        let old: Timings =
            serde_json::from_value(serde_json::json!({"totalMs": 1.0, "phasesMs": {"open": 1.0}}))
                .unwrap();
        assert!(old.memory.is_empty());
        let mut timer = PhaseTimer::start();
        timer.checkpoint("open");
        let measured = timer.finish("ready");
        assert_eq!(
            measured
                .memory
                .iter()
                .map(|point| point.phase.as_str())
                .collect::<Vec<_>>(),
            ["start", "open", "ready"]
        );
        let sum: f64 = measured.phases_ms.values().sum();
        assert!((sum - measured.total_ms).abs() < 1e-6);
    }

    #[test]
    fn phases_partition_monotonic_elapsed_time_without_sleeping() {
        let mut timer = PhaseTimer::start();
        let began = timer.began;
        timer.checkpoint_at("open", began + Duration::from_millis(2));
        timer.checkpoint_at("restore", began + Duration::from_millis(7));
        timer.checkpoint_at("restore", began + Duration::from_millis(10));
        let measured = timer.measured();
        assert_eq!(measured.total_ms, 10.0);
        assert_eq!(measured.phases_ms["open"], 2.0);
        assert_eq!(measured.phases_ms["restore"], 8.0);
        let json = serde_json::to_value(&measured).unwrap();
        assert_eq!(json["totalMs"], 10.0);
    }
}
