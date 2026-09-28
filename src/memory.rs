//! Process-wide observations, never ownership accounting or memory limits.
//! Sampling performs no collection, compaction, subprocess launch, or body read.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub at: String,
    pub pid: u32,
    pub source: String,
    pub resident_bytes: Option<u64>,
    pub footprint_bytes: Option<u64>,
    /// Process lifetime maximum, NOT a peak owned by the surrounding phase.
    pub lifetime_peak_footprint_bytes: Option<u64>,
    pub error: Option<String>,
}

impl Snapshot {
    pub fn capture() -> Self {
        let mut sample = Self {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            pid: std::process::id(),
            source: "unsupported".into(),
            resident_bytes: None,
            footprint_bytes: None,
            lifetime_peak_footprint_bytes: None,
            error: None,
        };
        sample_platform(&mut sample);
        sample
    }
}

#[cfg(target_os = "macos")]
fn sample_platform(sample: &mut Snapshot) {
    sample.source = "proc_pid_rusage_v4".into();
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // The flavor fixes the buffer ABI. The kernel writes only this initialized
    // buffer; it is read only on success. No other process is inspected.
    let result = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            usage.as_mut_ptr().cast(),
        )
    };
    if result != 0 {
        sample.error = Some(std::io::Error::last_os_error().to_string());
        return;
    }
    let usage = unsafe { usage.assume_init() };
    sample.resident_bytes = Some(usage.ri_resident_size);
    sample.footprint_bytes = Some(usage.ri_phys_footprint);
    sample.lifetime_peak_footprint_bytes = Some(usage.ri_lifetime_max_phys_footprint);
}

#[cfg(not(target_os = "macos"))]
fn sample_platform(sample: &mut Snapshot) {
    sample.error = Some("process memory sampling is not implemented on this platform".into());
}

/// O(1) ledger counters. Estimated retained bytes exclude allocator slack,
/// external Arc owners, and decoded batches currently held by readers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStats {
    pub events: usize,
    pub in_memory_bodies: usize,
    pub cache: Option<CacheStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub entries: usize,
    pub estimated_retained_bytes: usize,
    pub budget_bytes: usize,
    pub decodes: u64,
    pub hits: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub phase: String,
    pub memory: Snapshot,
}

/// Fixed vocabulary bounds the trace independently of history length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    BetweenBatches,
    Bookkeeping,
    Size,
    Usage,
    Background,
    Cards,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    pub sequence: u64,
    pub before: Snapshot,
    pub after: Snapshot,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageCost {
    pub operations: u64,
    pub samples: u64,
    pub first: Option<Interval>,
    pub last: Option<Interval>,
    pub largest_footprint_increase: Option<Interval>,
    pub largest_lifetime_peak_increase: Option<Interval>,
}

/// Sample the first operation and every 256th thereafter; inter-batch samples
/// always run. Unsampled operations have NO implied memory measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakdown {
    pub sample_every: u64,
    pub stages: std::collections::BTreeMap<Stage, StageCost>,
}
impl Default for Breakdown {
    fn default() -> Self {
        Self {
            sample_every: 256,
            stages: Default::default(),
        }
    }
}

pub struct Pending {
    stage: Stage,
    sequence: u64,
    before: Snapshot,
    began: std::time::Instant,
}

impl Breakdown {
    pub fn begin(&mut self, stage: Stage, sequence: u64) -> Option<Pending> {
        let cost = self.stages.entry(stage).or_default();
        cost.operations += 1;
        if stage != Stage::BetweenBatches
            && cost.operations != 1
            && !cost.operations.is_multiple_of(self.sample_every.max(1))
        {
            return None;
        }
        Some(Pending {
            stage,
            sequence,
            before: Snapshot::capture(),
            began: std::time::Instant::now(),
        })
    }

    pub fn end(&mut self, pending: Option<Pending>) {
        let Some(pending) = pending else {
            return;
        };
        let interval = Interval {
            sequence: pending.sequence,
            before: pending.before,
            elapsed_ms: pending.began.elapsed().as_secs_f64() * 1000.0,
            after: Snapshot::capture(),
        };
        self.record(pending.stage, interval);
    }

    fn record(&mut self, stage: Stage, interval: Interval) {
        let cost = self.stages.entry(stage).or_default();
        cost.samples += 1;
        cost.first.get_or_insert_with(|| interval.clone());
        let increase = |i: &Interval, peak: bool| -> Option<i128> {
            let bytes = |s: &Snapshot| {
                if peak {
                    s.lifetime_peak_footprint_bytes
                } else {
                    s.footprint_bytes
                }
            };
            Some(i128::from(bytes(&i.after)?) - i128::from(bytes(&i.before)?))
        };
        for (best, peak) in [
            (&mut cost.largest_footprint_increase, false),
            (&mut cost.largest_lifetime_peak_increase, true),
        ] {
            if let Some(delta) = increase(&interval, peak) {
                if best
                    .as_ref()
                    .and_then(|i| increase(i, peak))
                    .is_none_or(|old| delta > old)
                {
                    *best = Some(interval.clone());
                }
            }
        }
        cost.last = Some(interval);
    }

    pub fn measure<T>(&mut self, stage: Stage, sequence: u64, operation: impl FnOnce() -> T) -> T {
        let pending = self.begin(stage, sequence);
        let result = operation();
        self.end(pending);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_and_storage_are_bounded_without_skipping_work() {
        let mut trace = Breakdown::default();
        let mut executed = 0;
        for seq in 1..=4096 {
            trace.measure(Stage::Cards, seq, || executed += 1);
        }
        assert_eq!(executed, 4096);
        let cost = &trace.stages[&Stage::Cards];
        assert_eq!(cost.operations, 4096);
        assert_eq!(cost.samples, 17);
        assert_eq!(cost.first.as_ref().unwrap().sequence, 1);
        assert_eq!(cost.last.as_ref().unwrap().sequence, 4096);
        assert!(serde_json::to_vec(&trace).unwrap().len() < 8192);
    }

    #[test]
    fn footprint_decreases_and_lifetime_peak_increases_are_separate() {
        let point = |footprint, peak| Snapshot {
            at: "fixture".into(),
            pid: 1,
            source: "fixture".into(),
            resident_bytes: None,
            footprint_bytes: footprint,
            lifetime_peak_footprint_bytes: peak,
            error: None,
        };
        let interval = |sequence, before, after, old_peak, new_peak| Interval {
            sequence,
            before: point(before, old_peak),
            after: point(after, new_peak),
            elapsed_ms: 0.0,
        };
        let mut trace = Breakdown::default();
        trace.record(
            Stage::Cards,
            interval(1, Some(100), Some(300), Some(1000), Some(1000)),
        );
        trace.record(
            Stage::Cards,
            interval(2, Some(300), Some(50), Some(1000), Some(2000)),
        );
        trace.record(Stage::Cards, interval(3, None, None, None, None));
        let cost = &trace.stages[&Stage::Cards];
        assert_eq!(
            cost.largest_footprint_increase.as_ref().unwrap().sequence,
            1
        );
        assert_eq!(
            cost.largest_lifetime_peak_increase
                .as_ref()
                .unwrap()
                .sequence,
            2
        );
        assert_eq!(cost.last.as_ref().unwrap().sequence, 3);
        assert!(cost.last.as_ref().unwrap().after.footprint_bytes.is_none());
    }

    #[test]
    fn process_sample_is_serializable_and_names_its_scope() {
        let sample = Snapshot::capture();
        assert_eq!(sample.pid, std::process::id());
        assert!(!sample.at.is_empty());
        #[cfg(target_os = "macos")]
        {
            assert!(sample.error.is_none(), "{sample:?}");
            assert!(sample.resident_bytes.unwrap() > 0);
            assert!(sample.footprint_bytes.unwrap() > 0);
            assert!(sample.lifetime_peak_footprint_bytes.unwrap() > 0);
        }
        let value = serde_json::to_value(sample).unwrap();
        assert!(value.get("lifetimePeakFootprintBytes").is_some());
        assert!(value.get("phasePeakBytes").is_none());
    }
}
