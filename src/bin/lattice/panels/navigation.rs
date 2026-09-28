//! Per-seat panel navigation. Opening, hiding and closing deliberately have
//! different reset rules; neither rendering nor model catalog I/O owns these.
use super::{panel_tabs, AT_COMMANDS, AT_MODELS};

type Location = (usize, usize);

#[derive(Default)]
pub(crate) struct PanelNavigation {
    active: Option<Location>,
    details: bool,
    scroll: usize,
    selected: usize,
}

impl PanelNavigation {
    pub fn active(&self) -> Option<Location> {
        self.active
    }
    pub fn is_visible(&self) -> bool {
        self.active.is_some()
    }
    pub fn details_expanded(&self) -> bool {
        self.details
    }
    pub fn scroll_offset(&self) -> usize {
        self.scroll
    }
    pub fn selected_row(&self) -> usize {
        self.selected
    }

    pub fn show(&mut self, at: Location) {
        self.active = Some(at);
    }
    pub fn show_models(&mut self, current: Option<usize>) {
        self.show(AT_MODELS);
        self.select_row(current.unwrap_or(0));
    }
    pub fn close(&mut self) {
        self.dismiss_preserving_details();
        self.collapse_details();
    }
    pub fn dismiss_preserving_details(&mut self) {
        self.active = None;
    }
    pub fn toggle_details(&mut self) {
        self.details = !self.details;
    }
    pub fn collapse_details(&mut self) {
        self.details = false;
    }
    pub fn reset_scroll(&mut self) {
        self.scroll = 0;
    }
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
    }
    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll += amount;
    }
    pub fn select_row(&mut self, index: usize) {
        self.selected = index;
    }
    pub fn clamp_selection(&mut self, row_count: usize) {
        self.selected = self.selected.min(row_count.saturating_sub(1));
    }
    pub fn previous_row(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub fn next_row(&mut self, row_count: usize) {
        self.selected = (self.selected + 1).min(row_count.saturating_sub(1));
    }
    pub fn previous_tab(&mut self) {
        let (panel, tab) = self.active.unwrap_or(AT_COMMANDS);
        let count = panel_tabs(panel).len().max(1);
        self.active = Some((panel, (tab + count - 1) % count));
        self.reset_scroll();
    }
    pub fn next_tab(&mut self) {
        let (panel, tab) = self.active.unwrap_or(AT_COMMANDS);
        let count = panel_tabs(panel).len().max(1);
        self.active = Some((panel, (tab + 1) % count));
        self.reset_scroll();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_host::panels::{AT_BACKGROUND, AT_COMPONENTS, AT_CONTEXT, PANELS};

    fn populated() -> PanelNavigation {
        let mut state = PanelNavigation::default();
        state.show(AT_COMPONENTS);
        state.select_row(3);
        state.scroll_down(17);
        state.toggle_details();
        state
    }

    #[test]
    fn opening_preserves_navigation_but_models_select_the_current_row() {
        let mut state = PanelNavigation::default();
        assert_eq!(state.active(), None);
        assert!(!state.is_visible() && !state.details_expanded());
        assert_eq!((state.scroll_offset(), state.selected_row()), (0, 0));
        state = populated();
        state.show(AT_CONTEXT);
        assert_eq!(state.active(), Some(AT_CONTEXT));
        assert_eq!((state.scroll_offset(), state.selected_row()), (17, 3));
        assert!(state.details_expanded());
        state.show_models(Some(1));
        assert_eq!(state.active(), Some(AT_MODELS));
        assert_eq!(state.selected_row(), 1);
        state.show_models(None);
        assert_eq!(state.selected_row(), 0);
        assert_eq!(state.scroll_offset(), 17);
        assert!(state.details_expanded());
    }

    #[test]
    fn closing_and_dismissing_have_distinct_detail_lifetimes() {
        let mut state = populated();
        state.dismiss_preserving_details();
        assert!(!state.is_visible());
        assert!(state.details_expanded());
        state.show(AT_MODELS);
        state.close();
        assert!(!state.is_visible() && !state.details_expanded());
        assert_eq!((state.scroll_offset(), state.selected_row()), (17, 3));
        state.show(AT_COMPONENTS);
        assert_eq!((state.scroll_offset(), state.selected_row()), (17, 3));
    }

    #[test]
    fn tabs_wrap_locally_and_only_reset_scroll() {
        for (panel, (_, tabs)) in PANELS.iter().enumerate() {
            let mut state = populated();
            state.show((panel, 0));
            state.previous_tab();
            assert_eq!(state.active(), Some((panel, tabs.len() - 1)));
            state.next_tab();
            assert_eq!(state.active(), Some((panel, 0)));
            assert_eq!(state.scroll_offset(), 0);
            assert_eq!(state.selected_row(), 3);
            assert!(state.details_expanded());
        }
        let mut state = populated();
        state.show(AT_BACKGROUND);
        state.next_tab();
        assert_eq!(state.active(), Some(AT_BACKGROUND));
    }

    #[test]
    fn selection_is_not_clamped_by_rendering_or_upward_movement() {
        let mut state = populated();
        state.previous_row();
        assert_eq!(state.selected_row(), 2);
        state.next_row(2);
        assert_eq!(state.selected_row(), 1);
        state.clamp_selection(0);
        state.previous_row();
        state.next_row(0);
        assert_eq!(state.selected_row(), 0);
        state.next_row(2);
        state.next_row(2);
        assert_eq!(state.selected_row(), 1);
        assert_eq!(state.scroll_offset(), 17);
        assert!(state.details_expanded());
    }

    #[test]
    fn scrolling_is_unbounded_downward_and_saturating_upward() {
        let mut state = populated();
        for step in [1, 3, 10] {
            state.reset_scroll();
            state.scroll_down(step);
            state.scroll_down(step);
            assert_eq!(state.scroll_offset(), step * 2);
            state.scroll_up(step * 3);
            assert_eq!(state.scroll_offset(), 0);
        }
        assert_eq!(state.selected_row(), 3);
        assert!(state.details_expanded());
    }
}
