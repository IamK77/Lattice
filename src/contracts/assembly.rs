use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Assembly manifest — the single truth of how the system runs; a pure-data document.
/// Developers generate it with a typed API; ordinary users have a UI or an agent
/// write it for them; the kernel only faithfully executes it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssemblyManifest {
    /// Instance name → which component, with what config
    pub instances: BTreeMap<String, ComponentInstance>,
    /// Every wire, in black and white
    pub wires: Vec<Wire>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentInstance {
    /// Component name, looked up in the component registry
    pub component: String,
    /// Component-private config; the kernel does not interpret it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// The slot's bound (the trait bound of assemblies): profiles whatever
    /// fills this position must claim. Inspection rejects an assembly whose
    /// component does not implement every one — so swapping in an
    /// unqualified component fails at boot, with a plain message, instead
    /// of misbehaving at runtime. Empty = no requirement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

impl ComponentInstance {
    pub fn new(component: &str, config: Option<Value>) -> Self {
        Self {
            component: component.to_string(),
            config,
            requires: Vec::new(),
        }
    }
}

/// A wire; endpoints are written as "instance.port"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wire {
    pub from: String,
    pub to: String,
}

impl Wire {
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
        }
    }
}

/// Parse "instance.port"; returns None if malformed
pub fn parse_endpoint(endpoint: &str) -> Option<(&str, &str)> {
    let dot = endpoint.find('.')?;
    if dot == 0 || dot == endpoint.len() - 1 {
        return None;
    }
    Some((&endpoint[..dot], &endpoint[dot + 1..]))
}
