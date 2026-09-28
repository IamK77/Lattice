//! Exact turn peaks with bounded display statistics. The only mutable peak is
//! the tail; all preceding positive growth is represented by its exact top eight,
//! count, and sum. Drawing does not read peak pages or sort the entire history.

use super::pages::{Item, Pages, Slot};
use crate::LogReader;
use serde::{Deserialize, Serialize};
use std::{io, ops::Range};

const SHOWN: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Peak {
    turn: u64,
    value: u64,
}

impl Item for Peak {
    fn lookup_key(&self) -> Option<&str> {
        None
    }
    fn same_identity(&self, other: &Self) -> bool {
        self.turn == other.turn
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Aggregate {
    positive: usize,
    total: u128,
    leaders: Vec<(usize, u64)>,
}

impl Aggregate {
    fn add(&mut self, ordinal: usize, delta: u64) {
        if delta == 0 {
            return;
        }
        self.positive += 1;
        self.total += u128::from(delta);
        self.leaders.push((ordinal, delta));
        self.leaders
            .sort_by_key(|(ordinal, delta)| (std::cmp::Reverse(*delta), *ordinal));
        self.leaders.truncate(SHOWN);
    }

    fn summary(self, records: usize) -> GrowthSummary {
        GrowthSummary {
            records,
            smaller_count: self.positive - self.leaders.len(),
            smaller_sum: self.total
                - self
                    .leaders
                    .iter()
                    .map(|(_, delta)| u128::from(*delta))
                    .sum::<u128>(),
            leaders: self.leaders,
        }
    }

    fn valid(&self, records: usize) -> bool {
        self.positive <= records
            && self.leaders.len() == self.positive.min(SHOWN)
            && self
                .leaders
                .iter()
                .all(|(ordinal, delta)| *ordinal > 0 && *ordinal <= records && *delta > 0)
            && self
                .leaders
                .iter()
                .enumerate()
                .all(|(index, (ordinal, _))| {
                    self.leaders[..index]
                        .iter()
                        .all(|(before, _)| before != ordinal)
                })
            && self.leaders.windows(2).all(|pair| {
                (std::cmp::Reverse(pair[0].1), pair[0].0)
                    <= (std::cmp::Reverse(pair[1].1), pair[1].0)
            })
            && self.total
                >= self
                    .leaders
                    .iter()
                    .map(|(_, delta)| u128::from(*delta))
                    .sum::<u128>()
            && (self.positive > 0 || self.total == 0)
    }
}

/// Display ordinals are one-based peak-record numbers, not source turn IDs.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrowthSummary {
    pub records: usize,
    pub leaders: Vec<(usize, u64)>,
    pub smaller_count: usize,
    pub smaller_sum: u128,
}

impl GrowthSummary {
    /// Reference path for static views without a persistent peak reader.
    pub fn from_values(values: &[u64]) -> Self {
        let mut aggregate = Aggregate::default();
        let mut previous = 0;
        for (index, value) in values.iter().enumerate() {
            aggregate.add(index + 1, value.saturating_sub(previous));
            previous = *value;
        }
        aggregate.summary(values.len())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tail {
    peak: Peak,
    previous: u64,
}

/// Serialized together with the owning UI's exact prefix checkpoint.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pages: Vec<Slot>,
    count: usize,
    prefix: Aggregate,
    tail: Option<Tail>,
}

pub struct Peaks {
    pages: Pages<Peak>,
    prefix: Aggregate,
    tail: Option<Tail>,
}

impl Peaks {
    pub fn open(reader: &LogReader, state: State) -> io::Result<Self> {
        if state.tail.is_some() != (state.count > 0)
            || !state.prefix.valid(state.count.saturating_sub(1))
            || (state.count == 1 && state.tail.as_ref().is_some_and(|tail| tail.previous != 0))
        {
            return Err(invalid("invalid turn peak directory"));
        }
        let pages = Pages::open(
            reader
                .path()
                .filter(|path| path.is_dir())
                .map(ToOwned::to_owned),
            state.pages,
            state.count,
        )?;
        Ok(Self {
            pages,
            prefix: state.prefix,
            tail: state.tail,
        })
    }

    pub fn record(&mut self, turn: u64, value: u64) -> io::Result<()> {
        if let Some(tail) = &mut self.tail {
            if tail.peak.turn == turn {
                let peak = Peak {
                    turn,
                    value: tail.peak.value.max(value),
                };
                self.pages.replace(self.pages.len() - 1, peak.clone())?;
                tail.peak = peak;
                return Ok(());
            }
        }
        let peak = Peak { turn, value };
        let index = self.pages.push(peak.clone())?;
        let previous = match self.tail.take() {
            Some(tail) => {
                self.prefix
                    .add(index, tail.peak.value.saturating_sub(tail.previous));
                tail.peak.value
            }
            None => 0,
        };
        self.tail = Some(Tail { peak, previous });
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.len() == 0
    }
    pub fn last(&self) -> Option<(u64, u64)> {
        self.tail
            .as_ref()
            .map(|tail| (tail.peak.turn, tail.peak.value))
    }

    pub fn growth(&self) -> GrowthSummary {
        let mut aggregate = self.prefix.clone();
        if let Some(tail) = &self.tail {
            aggregate.add(self.len(), tail.peak.value.saturating_sub(tail.previous));
        }
        aggregate.summary(self.len())
    }

    pub fn load(&self, range: Range<usize>) -> io::Result<Vec<(u64, u64)>> {
        if range.start > range.end || range.end > self.len() {
            return Err(invalid("turn peak range is outside history"));
        }
        range
            .map(|index| self.pages.get(index).map(|peak| (peak.turn, peak.value)))
            .collect()
    }

    pub fn snapshot(&mut self) -> io::Result<State> {
        Ok(State {
            pages: self.pages.directory()?,
            count: self.len(),
            prefix: self.prefix.clone(),
            tail: self.tail.clone(),
        })
    }

    pub fn fork(&mut self, reader: &LogReader) -> io::Result<Self> {
        if reader.path().is_some_and(|path| path.is_dir()) {
            return Self::open(reader, self.snapshot()?);
        }
        // Legacy histories have no persistent pages; retain their cold path.
        let mut copy = Self::open(reader, State::default())?;
        for index in 0..self.len() {
            let peak = self.pages.get(index)?;
            copy.record(peak.turn, peak.value)?;
        }
        Ok(copy)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventLog;

    fn reference(values: &[u64]) -> GrowthSummary {
        let mut added: Vec<_> = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                (
                    index + 1,
                    value.saturating_sub(if index == 0 { 0 } else { values[index - 1] }),
                )
            })
            .filter(|(_, delta)| *delta > 0)
            .collect();
        added.sort_by_key(|(_, delta)| std::cmp::Reverse(*delta));
        GrowthSummary {
            records: values.len(),
            smaller_count: added.len().saturating_sub(8),
            smaller_sum: added
                .iter()
                .skip(8)
                .map(|(_, delta)| u128::from(*delta))
                .sum(),
            leaders: added.into_iter().take(8).collect(),
        }
    }

    #[test]
    fn peaks_match_every_prefix_and_warm_tail_without_reading_pages_to_draw() {
        let dir = tempfile::tempdir().unwrap();
        let log =
            EventLog::open_segmented(Vec::new(), "peaks", dir.path().join("peaks.ledger"), 4096)
                .unwrap();
        let mut peaks = Peaks::open(&log.reader(), State::default()).unwrap();
        let mut values = Vec::new();
        assert_eq!(peaks.growth(), reference(&values));
        for turn in 0..1025 {
            let value = if turn % 11 == 0 {
                0
            } else {
                ((turn * 17) % 251) as u64
            };
            peaks.record(turn as u64 * 3, value).unwrap();
            values.push(value);
            assert_eq!(peaks.growth(), reference(&values));
            peaks.record(turn as u64 * 3, value / 2).unwrap();
            assert_eq!(peaks.growth(), reference(&values));
            if turn % 7 == 0 {
                peaks.record(turn as u64 * 3, value + 500).unwrap();
                *values.last_mut().unwrap() = value + 500;
                assert_eq!(peaks.growth(), reference(&values));
            }
        }
        let state = peaks.snapshot().unwrap();
        let mut warm = Peaks::open(&log.reader(), state.clone()).unwrap();
        for _ in 0..10 {
            assert_eq!(warm.growth(), reference(&values));
        }
        assert_eq!(
            warm.pages.read_count(),
            0,
            "warm open and drawing must not read old peak pages"
        );
        warm.record(1024 * 3, u64::MAX).unwrap();
        *values.last_mut().unwrap() = u64::MAX;
        assert_eq!(warm.growth(), reference(&values));
        assert_eq!(warm.len(), 1025);
        assert_eq!(warm.last(), Some((1024 * 3, u64::MAX)));
        let old = Peaks::open(&log.reader(), state).unwrap();
        assert_ne!(
            old.load(1024..1025).unwrap(),
            warm.load(1024..1025).unwrap(),
            "published old pages are immutable"
        );
        assert!(warm.load(0..1026).is_err());
    }

    #[test]
    fn ties_tail_promotions_and_large_sums_keep_exact_top_eight() {
        let mut values = Vec::new();
        for _ in 0..20 {
            values.extend([u64::MAX, 0]);
        }
        assert_eq!(GrowthSummary::from_values(&values), reference(&values));
        let summary = GrowthSummary::from_values(&values);
        assert!(summary.smaller_sum > u128::from(u64::MAX));
        assert_eq!(
            serde_json::from_str::<GrowthSummary>(&serde_json::to_string(&summary).unwrap())
                .unwrap(),
            summary
        );
        assert_eq!(
            summary
                .leaders
                .iter()
                .map(|(ordinal, _)| *ordinal)
                .collect::<Vec<_>>(),
            vec![1, 3, 5, 7, 9, 11, 13, 15]
        );
    }
}
