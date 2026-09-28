//! Transcript reading position and explicit folds, local to one frontend seat.
//! Geometry is supplied by the frame; this state never reads cards or draws.
use lattice::view::TranscriptPosition;
use std::collections::HashSet;

type Anchor = (TranscriptPosition, isize);

#[derive(Default)]
pub(super) struct Browsing {
    offset: usize,
    anchor: Option<Anchor>,
    expanded: HashSet<String>,
}

impl Browsing {
    pub fn offset(&self) -> usize {
        self.offset
    }
    pub fn position(&self) -> Option<Anchor> {
        self.anchor.filter(|_| self.offset > 0)
    }
    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    /// Pinning hides the previous anchor; the next drawn frame settles it.
    /// Do not merge this with forgetting an anchor: callers have different
    /// timing, including a hidden transcript while a panel is open.
    pub fn pin(&mut self) {
        self.offset = 0;
    }
    pub fn forget_position(&mut self) {
        self.anchor = None;
    }
    pub fn toggle(&mut self, id: String) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
    }

    pub fn drawn(&mut self, top: TranscriptPosition, more_below: bool) {
        if self.offset == 0 || !more_below {
            self.offset = 0;
            self.anchor = None;
        } else {
            self.anchor = Some((top, 0));
        }
    }

    /// Positive input moves toward older rows. Keep the content-addressed
    /// path distinct from the legacy row-count fallback.
    pub fn scroll(
        &mut self,
        delta: isize,
        top: Option<TranscriptPosition>,
        more_above: bool,
        more_below: bool,
        maximum: Option<usize>,
    ) {
        if let Some(top) = top {
            if delta >= 0 && !more_above {
                return;
            }
            if delta < 0 && !more_below {
                self.offset = 0;
                self.anchor = None;
                return;
            }
            self.offset = 1;
            self.anchor = Some((top, delta.saturating_neg()));
            return;
        }
        self.offset = if delta >= 0 {
            let requested = self.offset.saturating_add(delta as usize);
            maximum.map_or(requested, |maximum| requested.min(maximum))
        } else {
            self.offset.saturating_sub((-delta) as usize)
        };
    }

    #[cfg(test)]
    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }
    #[cfg(test)]
    pub fn expand(&mut self, id: String) {
        self.expanded.insert(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice::view::TranscriptBlock;

    fn top(index: usize) -> TranscriptPosition {
        TranscriptPosition {
            block: TranscriptBlock::Entries(index),
            line: 2,
            byte: 8,
        }
    }

    #[test]
    fn anchored_scroll_uses_visible_edges_and_settles_after_drawing() {
        let mut state = Browsing::default();
        state.scroll(10, Some(top(4)), true, false, Some(0));
        assert_eq!(state.offset(), 1);
        assert_eq!(state.position(), Some((top(4), -10)));
        state.drawn(top(2), true);
        assert_eq!(state.position(), Some((top(2), 0)));
        state.scroll(10, Some(top(0)), false, true, None);
        assert_eq!(
            state.position(),
            Some((top(2), 0)),
            "no older rows means no movement"
        );
        state.scroll(-4, Some(top(2)), true, true, None);
        assert_eq!(state.position(), Some((top(2), 4)));
        state.drawn(top(6), false);
        assert_eq!(state.offset(), 0);
        assert_eq!(state.anchor, None);
        state.scroll(3, Some(top(5)), true, true, None);
        state.scroll(-1, Some(top(6)), true, false, None);
        assert_eq!(state.offset(), 0);
        assert_eq!(state.anchor, None);
    }

    #[test]
    fn pinning_hides_anchor_until_a_frame_settles_it_but_forgetting_does_not_pin() {
        let mut state = Browsing::default();
        state.scroll(2, Some(top(3)), true, true, None);
        state.pin();
        assert_eq!(state.position(), None);
        assert_eq!(state.anchor, Some((top(3), -2)));
        state.drawn(top(8), true);
        assert_eq!(state.anchor, None);
        state.scroll(2, Some(top(3)), true, true, None);
        state.forget_position();
        assert_eq!(state.offset(), 1);
        assert_eq!(state.position(), None);
    }

    #[test]
    fn row_fallback_clamps_without_rewriting_the_anchor() {
        let mut state = Browsing::default();
        state.scroll(10, None, false, false, Some(4));
        assert_eq!(state.offset(), 4);
        state.scroll(-20, None, true, true, None);
        assert_eq!(state.offset(), 0);
        state.scroll(2, Some(top(3)), true, true, None);
        let anchor = state.anchor;
        state.scroll(20, None, false, false, None);
        assert_eq!(state.offset(), 21);
        assert_eq!(state.anchor, anchor);
        state.offset = usize::MAX;
        state.scroll(20, None, false, false, None);
        assert_eq!(state.offset(), usize::MAX);
        state.scroll(20, None, false, false, Some(4));
        assert_eq!(state.offset(), 4);
    }

    #[test]
    fn folds_are_local_and_independent_of_the_reading_position() {
        let mut first = Browsing::default();
        let second = Browsing::default();
        first.toggle("call".into());
        first.pin();
        first.forget_position();
        first.drawn(top(0), false);
        assert!(first.is_expanded("call"));
        assert!(!second.is_expanded("call"));
        first.toggle("call".into());
        assert!(!first.is_expanded("call"));
    }
}
