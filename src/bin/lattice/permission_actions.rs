//! Frontend controls send decisions; only ledger state confirms their effect.
use super::*;

pub(super) fn permission(session: Option<&Session>, argument: &str) -> String {
    let Some(session) = session else {
        return "No live session".into();
    };
    match argument {
        "" => match session.permission_enabled() {
            Ok(Some(enabled)) => format!("Interface permission: {}. Applies only to this interface's contributing work; closing it ends permission. Use /permission on or /permission off.", if enabled { "ON" } else { "OFF" }),
            Ok(None) => "Interface permission is unavailable or has not been acknowledged yet".into(),
            Err(error) => format!("Cannot read interface permission: {error}"),
        },
        "on" | "off" => match session.set_permission(argument == "on") {
            Ok(()) => format!("Permission {argument} requested; waiting for the authority's state event"),
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
        Ok(Some(enabled)) => format!("{}. Answer the current question separately; the switch applies to later permission checks.",
            permission(Some(session), if enabled { "off" } else { "on" })),
        Ok(None) => "Interface permission is unavailable or has not been acknowledged yet".into(),
        Err(error) => format!("Cannot read interface permission: {error}"),
    }
}

pub(super) fn grants(session: Option<&Session>) -> String {
    let Some(session) = session else {
        return "No live session".into();
    };
    match session.flow_grants() {
        Err(error) => format!("Cannot read flow grants: {error}"),
        Ok(state) if state.grants.is_empty() => {
            "No flow grants. Permanent trust is separate.".into()
        }
        Ok(state) => {
            let mut lines = vec!["Flow grants survive reopening. /revoke <id> removes a grant, not completed effects or permanent trust.".to_owned()];
            for (id, grant) in state.grants {
                lines.push(format!(
                    "{id}: {}",
                    serde_json::to_string(&grant.matchers).expect("serializable matchers")
                ));
            }
            lines.join("\n")
        }
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
