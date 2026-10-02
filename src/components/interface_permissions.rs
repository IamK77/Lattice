//! Interface-instance permission state. The interface expresses intent; this
//! component publishes the authoritative, flow-local state as audited data.
//! A restore always starts a new lifetime, never revives historical permission.

mod provenance;
pub use provenance::{allowance, PermissionEvidence};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "interface-permissions";
pub const INSTANCE: &str = "permissions";
pub const CHANNEL: &str = "interface.permission";
pub const STATE: &str = "interface.permission.state";

/// A host-assigned identity, scoped to this stream and runtime lifetime. The
/// ledger position advances on reopen; the counter separates concurrent
/// attachments without relying on wall-clock precision or a client-supplied id.
pub fn new_instance_id(reader: &crate::LogReader) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "interface:{}:{}:{serial}",
        reader.stream(),
        reader.snapshot_end()
    )
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct InterfaceState {
    pub interfaces: BTreeMap<String, Interface>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Interface {
    pub owner: String,
    pub open: bool,
    pub enabled: bool,
}

impl InterfaceState {
    pub fn permits(&self, id: &str) -> bool {
        self.interfaces
            .get(id)
            .is_some_and(|interface| interface.open && interface.enabled)
    }

    fn apply(
        &mut self,
        owner: &str,
        id: &str,
        action: &str,
        enabled: Option<bool>,
    ) -> Result<(), String> {
        if id.is_empty() {
            return Err("an interface instance id is required".into());
        }
        if action == "open" {
            if self.interfaces.contains_key(id) {
                return Err("an interface instance id cannot be opened twice".into());
            }
            self.interfaces.insert(
                id.to_owned(),
                Interface {
                    owner: owner.to_owned(),
                    open: true,
                    enabled: false,
                },
            );
            return Ok(());
        }
        let interface = self
            .interfaces
            .get_mut(id)
            .ok_or("the interface instance is not registered")?;
        if interface.owner != owner || !interface.open {
            return Err("the interface instance is closed or belongs to another controller".into());
        }
        match action {
            "set" => interface.enabled = enabled.ok_or("enabled must be a boolean")?,
            "close" => {
                interface.open = false;
                interface.enabled = false;
            }
            _ => return Err("unknown interface permission action".into()),
        }
        Ok(())
    }
}

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("control", &[ce::EXTERNAL_INPUT])],
        outputs: vec![PortDecl::new("state", &[STATE])],
        events: vec![
            EventTypeDecl::decision(STATE, "An interface permission state transition").with_schema(
                json!({
                    "type":"object", "required":["interfaces", "accepted"],
                    "properties": {
                        "interfaces":{"type":"object", "additionalProperties":{
                            "type":"object", "required":["owner", "open", "enabled"],
                            "properties":{
                                "owner":{"type":"string"}, "open":{"type":"boolean"},
                                "enabled":{"type":"boolean"}
                            }, "additionalProperties":false
                        }},
                        "accepted":{"type":"boolean"}, "interface":{"type":"string"},
                        "action":{"type":"string"}, "error":{"type":"string"}
                    }, "additionalProperties":false
                }),
            ),
        ],
        default_wiring: vec![],
        capabilities: None,
        implements: vec![],
        tools: vec![],
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

/// Read the service's latest published snapshot, not raw frontend intent.
/// Consumers exchange audited JSON data; no mutable component state is shared.
pub fn read_state(
    log: &crate::kernel::log::LogReader,
    source: &str,
) -> std::io::Result<Option<InterfaceState>> {
    Ok(current_snapshot(log, source)?.map(|(_, state)| state))
}

// The first relevant record wins. An authority that cannot process `close`
// must not keep granting permission from its last enabled snapshot. A new
// runtime is also a boundary, even before its restore snapshot reaches disk.
fn current_snapshot(
    log: &crate::kernel::log::LogReader,
    source: &str,
) -> std::io::Result<Option<(String, InterfaceState)>> {
    use crate::core_events as ce;
    Ok(log
        .scan_back_types(
            &[
                STATE,
                ce::COMPONENT_CRASHED,
                ce::COMPONENT_REMOVED,
                ce::ERROR,
                ce::STREAM_OPENED,
                ce::STREAM_RESUMED,
            ],
            |event, _| {
                if event.event_type == STATE && event.source == source {
                    let state = serde_json::from_value(event.payload.clone()).map_err(|error| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                    })?;
                    return Ok(Some(Some((event.id.clone(), state))));
                }
                if event.source == "core"
                    && match event.event_type.as_str() {
                        ce::COMPONENT_CRASHED => event.payload["component"] == source,
                        ce::COMPONENT_REMOVED => event.payload["instance"] == source,
                        ce::ERROR => {
                            event.payload["code"] == "core.component_failed"
                                && event.payload["detail"]["component"] == source
                        }
                        ce::STREAM_OPENED | ce::STREAM_RESUMED => true,
                        _ => false,
                    }
                {
                    return Ok(Some(None));
                }
                Ok(None)
            },
        )?
        .flatten())
}

pub struct InterfacePermissions {
    state: InterfaceState,
    controllers: Vec<String>,
}

impl InterfacePermissions {
    pub fn from_config(config: Option<&Value>) -> Self {
        let controllers = config
            .and_then(|c| c.get("controllers"))
            .map(|v| {
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| vec!["ui".into()]);
        Self {
            state: InterfaceState::default(),
            controllers,
        }
    }

    fn publish(&self, cause: Option<&EventEnvelope>, error: Option<String>, ctx: &mut Ctx) {
        let mut payload = serde_json::to_value(&self.state).expect("interface state is JSON data");
        payload["accepted"] = json!(error.is_none());
        let reason = if let Some(error) = error {
            payload["error"] = json!(error);
            "The interface permission change was rejected"
        } else if cause.is_some() {
            "The interface controller changed its live permission state"
        } else {
            "A new runtime lifetime starts with no active interface permissions"
        };
        if let Some(event) = cause {
            if let Some(id) = event.payload["interface"].as_str() {
                payload["interface"] = json!(id);
            }
            if let Some(action) = event.payload["action"].as_str() {
                payload["action"] = json!(action);
            }
        }
        let causes: Vec<&str> = cause.into_iter().map(|e| e.id.as_str()).collect();
        ctx.emit(
            "state",
            EventDraft::new(STATE, &causes, payload).with_reason(reason),
        );
    }
}

impl Component for InterfacePermissions {
    fn restore(&mut self, ctx: &mut Ctx) {
        self.state = InterfaceState::default();
        self.publish(None, None, ctx);
    }

    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if event.event_type != ce::EXTERNAL_INPUT || event.payload["channel"] != CHANNEL {
            return;
        }
        let result = if self.controllers.contains(&event.source) {
            self.state.apply(
                &event.source,
                event.payload["interface"].as_str().unwrap_or_default(),
                event.payload["action"].as_str().unwrap_or_default(),
                event.payload["enabled"].as_bool(),
            )
        } else {
            Err("the sender is not an interface controller".into())
        };
        self.publish(Some(event), result.err(), ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interface_starts_disabled_and_close_ends_its_permission() {
        let mut state = InterfaceState::default();
        state.apply("ui", "a", "open", None).unwrap();
        assert!(!state.permits("a"));
        state.apply("ui", "a", "set", Some(true)).unwrap();
        assert!(state.permits("a"));
        state.apply("ui", "a", "close", None).unwrap();
        assert!(!state.permits("a"));
        assert!(state.apply("ui", "a", "set", Some(true)).is_err());
        assert!(state.apply("ui", "a", "open", None).is_err());
    }

    #[test]
    fn another_controller_cannot_change_an_interface() {
        let mut state = InterfaceState::default();
        state.apply("ui-a", "a", "open", None).unwrap();
        let before = state.clone();
        assert!(state.apply("ui-b", "a", "set", Some(true)).is_err());
        assert!(state.apply("ui-b", "a", "close", None).is_err());
        assert_eq!(state, before);
    }

    #[test]
    fn invalid_changes_preserve_existing_permission() {
        let mut state = InterfaceState::default();
        state.apply("ui", "a", "open", None).unwrap();
        state.apply("ui", "a", "set", Some(true)).unwrap();
        let before = state.clone();
        assert!(state.apply("ui", "a", "set", None).is_err());
        assert!(state.apply("ui", "a", "unknown", None).is_err());
        assert!(state.apply("ui", "", "open", None).is_err());
        assert_eq!(state, before);
    }
}
