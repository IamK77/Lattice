//! Frontend controls send decisions; only ledger state confirms their effect.
use super::*;

pub(super) fn observe(ui: &mut Ui, render: &RenderEvent, session: &Session) {
    ui.domain
        .authorizations
        .set_scoped(session.supports_operation_authorization());
    let RenderEvent::Appended(event) = render else {
        return;
    };
    if ui.panel.active() == Some(panels::AT_GRANTS)
        && event.event_type == lattice::components::operation_policy::STATE
    {
        let selected = ui.grants.display().selected.clone();
        ui.grants.sync(Some(session));
        if selected != ui.grants.display().selected {
            ui.panel.reset_scroll();
        }
    }
    if !matches!(
        event.event_type.as_str(),
        lattice::components::interface_permissions::STATE
            | core_events::COMPONENT_CRASHED
            | core_events::COMPONENT_REMOVED
            | core_events::ERROR
            | core_events::STREAM_OPENED
            | core_events::STREAM_RESUMED
    ) {
        return;
    }
    // Query the current binding's authority only at state/lifecycle changes,
    // never on a draw or ordinary keystroke, and never infer from replay text.
    match session.permission_enabled() {
        Ok(enabled) => ui.interface_permission = enabled == Some(true),
        Err(error) => {
            ui.interface_permission = false;
            ack(ui, format!("Cannot read interface permission: {error}"));
        }
    }
}

pub(super) fn is_shortcut(key: ratatui::crossterm::event::KeyEvent) -> bool {
    match key.code {
        KeyCode::BackTab => key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT,
        KeyCode::Tab => key.modifiers == KeyModifiers::SHIFT,
        _ => false,
    }
}

pub(super) fn permission(session: Option<&Session>, argument: &str) -> String {
    let Some(session) = session else {
        return "No live session".into();
    };
    match argument {
        "" => match session.permission_enabled() {
            Ok(Some(enabled)) => format!("Interface permission: {}. Applies only to this interface's contributing work; closing it ends permission. Shift+Tab toggles; /permission on or /permission off also work.", if enabled { "ON" } else { "OFF" }),
            Ok(None) => "Interface permission is unavailable or has not been acknowledged yet".into(),
            Err(error) => format!("Cannot read interface permission: {error}"),
        },
        "on" | "off" => match session.set_permission(argument == "on") {
            Ok(()) => String::new(),
            Err(error) => error,
        },
        _ => "usage: /permission [on|off]".into(),
    }
}

pub(super) fn toggle_permission(session: Option<&Session>) -> String {
    let Some(session) = session else {
        return "No live session".into();
    };
    match session.permission_enabled() {
        Ok(Some(enabled)) => permission(Some(session), if enabled { "off" } else { "on" }),
        Ok(None) => "Interface permission is unavailable or has not been acknowledged yet".into(),
        Err(error) => format!("Cannot read interface permission: {error}"),
    }
}

pub(super) fn revoke(session: Option<&Session>, id: &str) -> String {
    if id.is_empty() {
        return "usage: /revoke <grant-id> (see /grants)".into();
    }
    let Some(session) = session else {
        return "No live session".into();
    };
    match session.flow_grants() {
        Err(error) => format!("Cannot read flow grants: {error}"),
        Ok(state) if !state.grants.contains_key(id) => "No such flow grant; see /grants".into(),
        Ok(_) => match session.revoke_grant(id) {
            Ok(()) => "Revocation requested; see /grants for authoritative state".into(),
            Err(error) => error,
        },
    }
}

pub(super) fn answer_selected(ui: &mut Ui, session: Option<&Session>) {
    use lattice::view::AuthorizationChoice as Choice;
    let prompt = match ui.authorization_prompt() {
        Ok(Some(prompt)) => prompt,
        Ok(None) => return,
        Err(error) => {
            ack(ui, error.to_string());
            return;
        }
    };
    if session.is_none() {
        ui.domain.authorizations.answer_oldest();
        return;
    }
    match prompt.selected {
        Choice::Flow | Choice::Permanent => {
            scoped_answer(ui, session, prompt.selected == Choice::Permanent);
            return;
        }
        _ => {}
    }
    let allow = prompt.selected == Choice::Once;
    let result = match session {
        Some(session) if allow && session.supports_operation_authorization() => {
            session.authorize_once(&prompt.request)
        }
        Some(session) => {
            session.authorize(&prompt.request, allow);
            Ok(())
        }
        None => Ok(()),
    };
    match result {
        Ok(()) => {
            ui.domain.authorizations.answer_oldest();
        }
        Err(error) => ack(ui, error),
    }
}

pub(super) fn scoped_answer(ui: &mut Ui, session: Option<&Session>, permanent: bool) {
    let Some(session) = session else {
        return;
    };
    let Some(id) = ui.domain.authorizations.next().map(str::to_owned) else {
        return;
    };
    let result = (|| -> Result<(), String> {
        let event = session
            .log_reader()
            .get(&id)
            .map_err(|e| e.to_string())?
            .ok_or("Missing authorization question")?;
        if permanent {
            if event.event_type != lattice::components::trust_policy::AUTH_REQUESTED {
                return Err("Permanent trust is available only for admission requests".into());
            }
            session.authorize(&id, true);
            Ok(())
        } else {
            if event.event_type == lattice::components::operation_policy::AUTH_REQUESTED
                && event.payload["grants"]
                    .as_array()
                    .is_none_or(|grants| grants.is_empty())
            {
                return Err(
                    "This operation requires approval each time; no flow grant is available".into(),
                );
            }
            session.authorize_flow(&id)
        }
    })();
    match result {
        Ok(()) => {
            ui.domain.authorizations.answer_oldest();
        }
        Err(error) => ack(ui, &error),
    }
}
