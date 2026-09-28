//! Persistent cards plus the two local display operations: clear and startup errors.

use std::io;

use lattice::view::{history::History, Entry, TranscriptGroup, TranscriptKind};
use lattice::LogReader;

#[cfg(test)]
#[path = "cards/tests.rs"]
pub(super) mod tests;

pub(super) struct Cards {
    history: History,
    cleared: Option<History>,
    // Insertions are ordered by the source-card boundary at which they appeared.
    local: Vec<(usize, Entry)>,
}

impl Cards {
    pub fn recover(reader: LogReader, through: u64) -> io::Result<Self> {
        Ok(Self {
            history: History::recover(reader, through)?,
            cleared: None,
            local: Vec::new(),
        })
    }

    fn displayed(&self) -> &History {
        self.cleared.as_ref().unwrap_or(&self.history)
    }

    pub fn len(&self) -> usize {
        self.displayed().len() + self.local.len()
    }

    pub fn has_user(&self) -> bool {
        self.history.has_user()
    }

    pub fn clear(&mut self) -> io::Result<()> {
        self.cleared = Some(self.history.empty_tail()?);
        self.local.clear();
        Ok(())
    }

    pub fn push_local(&mut self, entry: Entry) {
        self.local.push((self.displayed().len(), entry));
    }

    fn local_position(&self, index: usize) -> usize {
        self.local[index].0 + index
    }

    fn resolve(&self, index: usize) -> io::Result<Result<usize, usize>> {
        if index >= self.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "display card is outside history",
            ));
        }
        // Local positions include earlier insertions; binary search those positions.
        let mut low = 0;
        let mut high = self.local.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.local_position(middle) < index {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low < self.local.len() && self.local_position(low) == index {
            Ok(Err(low))
        } else {
            Ok(Ok(index - low))
        }
    }

    fn kind(&self, index: usize) -> io::Result<TranscriptKind> {
        match self.resolve(index)? {
            Ok(source) => self.displayed().transcript_kind(source),
            Err(local) => Ok(TranscriptKind::of(&self.local[local].1)),
        }
    }

    fn group_once(&self, index: usize) -> io::Result<TranscriptGroup> {
        let total = self.len();
        if index == total {
            return Ok(TranscriptGroup {
                first: total,
                total,
                previous: total.checked_sub(1).map(|at| self.kind(at)).transpose()?,
                entries: Vec::new(),
            });
        }
        let (first, entries) = match self.resolve(index)? {
            Err(local) => (index, vec![self.local[local].1.clone()]),
            Ok(source) => {
                let mut range = self.displayed().group(source)?;
                let preceding = self
                    .local
                    .partition_point(|(boundary, _)| *boundary <= source);
                if preceding > 0 {
                    range.start = range.start.max(self.local[preceding - 1].0);
                }
                if let Some((boundary, _)) = self.local.get(preceding) {
                    range.end = range.end.min(*boundary);
                }
                let first = range.start
                    + self
                        .local
                        .partition_point(|(boundary, _)| *boundary <= range.start);
                (first, self.displayed().load(range)?)
            }
        };
        Ok(TranscriptGroup {
            first,
            total,
            previous: first.checked_sub(1).map(|at| self.kind(at)).transpose()?,
            entries,
        })
    }

    pub fn group(&mut self, index: usize) -> io::Result<TranscriptGroup> {
        match self.group_once(index) {
            Ok(group) => Ok(group),
            Err(initial) => {
                let history = self.cleared.as_mut().unwrap_or(&mut self.history);
                *history = history.rebuild(history.through(), initial.to_string())?;
                self.group_once(index)
            }
        }
    }

    pub fn last_tool_running(&self) -> bool {
        self.len().checked_sub(1).is_some_and(|last| {
            matches!(self.resolve(last), Ok(Ok(_))) && self.displayed().last_tool_running()
        })
    }

    fn last_is_thinking(&self) -> io::Result<bool> {
        match self.len().checked_sub(1) {
            Some(last) if self.resolve(last)?.is_ok() => self.displayed().last_is_thinking(),
            _ => Ok(false),
        }
    }

    pub fn advance(&mut self, through: u64) -> io::Result<(bool, bool)> {
        fn advance(history: &mut History, through: u64) -> io::Result<bool> {
            if through <= history.through() {
                return Ok(false);
            }
            match history.catch_up(through).and_then(|handled| {
                history.last_is_thinking()?;
                Ok(handled)
            }) {
                Ok(handled) => Ok(handled),
                Err(initial) => {
                    *history = history.rebuild(through, initial.to_string())?;
                    Ok(history.last_handled())
                }
            }
        }
        let mut handled = advance(&mut self.history, through)?;
        if let Some(cleared) = &mut self.cleared {
            handled = advance(cleared, through)?;
        }
        Ok((handled, handled && self.last_is_thinking()?))
    }
}
