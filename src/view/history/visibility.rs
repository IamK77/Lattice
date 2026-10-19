//! Visible ordinals are a projection over stable source-card ordinals.
use super::*;

impl History {
    pub fn visible_len(&self) -> usize {
        self.visible_boundary(self.len())
    }

    pub fn visible_boundary(&self, source: usize) -> usize {
        self.pending.before(source)
    }

    pub fn source_ordinal(&self, visible: usize) -> usize {
        self.pending.source(visible)
    }

    pub fn visible_kind(&self, index: usize) -> io::Result<super::super::TranscriptKind> {
        self.transcript_kind(self.pending.source(index))
    }

    pub fn visible_group(&self, index: usize) -> io::Result<Range<usize>> {
        self.healthy()?;
        self.pending.group(index, self.visible_len(), |source| {
            Ok(self.pages.get(source)?.identity.kind)
        })
    }

    pub fn visible_load(&self, range: Range<usize>) -> io::Result<Vec<Entry>> {
        self.healthy()?;
        if range.start > range.end || range.end > self.visible_len() {
            return Err(invalid("visible card page is outside history"));
        }
        range
            .map(|index| self.get(self.pending.source(index)))
            .collect()
    }

    pub fn visible_last_tool_running(&self) -> bool {
        self.visible_len()
            .checked_sub(1)
            .is_some_and(|index| self.running.contains_key(&self.pending.source(index)))
    }

    pub fn visible_last_is_thinking(&self) -> io::Result<bool> {
        self.healthy()?;
        match self.visible_len().checked_sub(1) {
            Some(index) => {
                Ok(self.pages.get(self.pending.source(index))?.identity.kind == Kind::Thinking)
            }
            None => Ok(false),
        }
    }
}
