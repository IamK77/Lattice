//! Live grant management. Sending a revoke never removes an authoritative row.
use super::Session;
use lattice::{components::operation_policy::GrantState, view::GrantPanel};
use ratatui::crossterm::event::KeyCode;

#[derive(Default)]
pub(super) struct GrantControls {
    panel: GrantPanel,
}

impl GrantControls {
    pub fn display(&self) -> &GrantPanel {
        &self.panel
    }

    fn apply(&mut self, state: Result<GrantState, String>) {
        match state {
            Ok(state) => {
                self.panel.problem = None;
                if !self
                    .panel
                    .selected
                    .as_ref()
                    .is_some_and(|id| state.grants.contains_key(id))
                {
                    self.panel.selected = state.grants.keys().next().cloned();
                }
                if self
                    .panel
                    .confirming
                    .as_ref()
                    .is_some_and(|id| !state.grants.contains_key(id))
                {
                    self.panel.confirming = None;
                }
                if self
                    .panel
                    .pending
                    .as_ref()
                    .is_some_and(|id| !state.grants.contains_key(id))
                {
                    self.panel.pending = None;
                }
                self.panel.state = state;
            }
            Err(error) => {
                self.panel = GrantPanel {
                    problem: Some(error),
                    ..GrantPanel::default()
                };
            }
        }
    }

    pub fn sync(&mut self, session: Option<&Session>) {
        self.apply(match session {
            Some(session) => session.flow_grants().map_err(|error| error.to_string()),
            None => Err("No live session; grants cannot be read".into()),
        });
    }

    pub fn refresh(&mut self, session: Option<&Session>) {
        self.panel.confirming = None;
        self.panel.pending = None;
        self.sync(session);
    }

    fn sent(&mut self, id: String, result: Result<(), String>) {
        self.panel.confirming = None;
        match result {
            Ok(()) => self.panel.pending = Some(id),
            Err(error) => self.panel.problem = Some(error),
        }
    }

    /// False leaves closing and page scrolling to the common panel controls.
    pub fn key(&mut self, key: KeyCode, session: Option<&Session>) -> bool {
        if let Some(id) = self.panel.confirming.clone() {
            match key {
                KeyCode::Esc => self.panel.confirming = None,
                KeyCode::Enter => {
                    // Revalidate the ID the person confirmed, never whichever
                    // row happens to occupy its former position after refresh.
                    self.sync(session);
                    if self.panel.problem.is_none() && self.panel.state.grants.contains_key(&id) {
                        let result = session.expect("successful live read").revoke_grant(&id);
                        self.sent(id, result);
                    }
                }
                _ => {}
            }
            return !matches!(key, KeyCode::PageUp | KeyCode::PageDown);
        }
        match key {
            KeyCode::Up | KeyCode::Down => {
                let ids: Vec<_> = self.panel.state.grants.keys().collect();
                if let Some(at) = ids
                    .iter()
                    .position(|id| Some(*id) == self.panel.selected.as_ref())
                {
                    let next = if key == KeyCode::Up {
                        at.saturating_sub(1)
                    } else {
                        (at + 1).min(ids.len() - 1)
                    };
                    self.panel.selected = Some(ids[next].clone());
                }
            }
            KeyCode::Char('d') if self.panel.problem.is_none() && self.panel.pending.is_none() => {
                self.panel.confirming = self.panel.selected.clone();
            }
            KeyCode::Char('r') => self.refresh(session),
            KeyCode::Esc | KeyCode::PageUp | KeyCode::PageDown => return false,
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice::components::operation_policy::FlowGrant;
    fn state(ids: &[&str]) -> GrantState {
        GrantState {
            grants: ids
                .iter()
                .map(|id| {
                    (
                        (*id).into(),
                        FlowGrant {
                            matchers: vec![],
                            question: "question".into(),
                            interface: None,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn failed_send_preserves_the_grant_and_releases_confirmation_for_refresh() {
        let mut controls = GrantControls::default();
        controls.apply(Ok(state(&["grant"])));
        controls.key(KeyCode::Char('d'), None);
        controls.sent("grant".into(), Err("interface is closing".into()));
        assert!(controls.display().confirming.is_none());
        assert!(controls.display().pending.is_none());
        assert_eq!(controls.display().state.grants.len(), 1);
        assert_eq!(
            controls.display().problem.as_deref(),
            Some("interface is closing")
        );
        controls.key(KeyCode::Char('r'), None);
        assert!(controls
            .display()
            .problem
            .as_deref()
            .unwrap()
            .contains("No live session"));
    }

    #[test]
    fn selection_and_confirmation_follow_ids_not_changing_row_numbers() {
        let mut controls = GrantControls::default();
        controls.apply(Ok(state(&["b", "c"])));
        controls.key(KeyCode::Down, None);
        controls.key(KeyCode::Char('d'), None);
        assert_eq!(controls.display().confirming.as_deref(), Some("c"));
        controls.apply(Ok(state(&["a", "b", "c"])));
        assert_eq!(controls.display().selected.as_deref(), Some("c"));
        controls.apply(Ok(state(&["a", "b"])));
        assert!(controls.display().confirming.is_none());
        assert_eq!(controls.display().selected.as_deref(), Some("a"));
        controls.key(KeyCode::Char('d'), None);
        assert!(controls.key(KeyCode::Esc, None));
        assert!(!controls.key(KeyCode::Esc, None));
        assert_eq!(controls.display().state.grants.len(), 2);
    }

    #[test]
    fn a_pending_revoke_waits_for_recorded_removal_and_read_failure_is_not_empty_success() {
        let mut controls = GrantControls::default();
        controls.apply(Ok(state(&["grant"])));
        controls.sent("grant".into(), Ok(()));
        assert_eq!(
            controls.display().state.grants.len(),
            1,
            "sending is not removal"
        );
        controls.apply(Ok(state(&["grant"])));
        assert_eq!(controls.display().state.grants.len(), 1);
        assert!(controls.display().pending.is_some());
        controls.apply(Ok(state(&[])));
        assert!(controls.display().pending.is_none());
        controls.apply(Err("unreadable ledger".into()));
        assert_eq!(
            controls.display().problem.as_deref(),
            Some("unreadable ledger")
        );
        controls.key(KeyCode::Char('d'), None);
        assert!(controls.display().confirming.is_none());
    }
}
