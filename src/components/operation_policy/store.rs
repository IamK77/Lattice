//! Durable grants are flow data. Live interface permission is a separate service.
use std::collections::BTreeMap;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{EventEnvelope, LogReader};

use super::{Decision, GrantMatcher, Invocation, INSTANCE, STATE};
use crate::components::interface_permissions;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FlowGrant {
    pub matchers: Vec<GrantMatcher>,
    pub question: String,
    pub interface: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct GrantState {
    pub grants: BTreeMap<String, FlowGrant>,
}

pub fn read_grants(reader: &LogReader, source: &str) -> io::Result<GrantState> {
    reader
        .scan_back_types(&[STATE], |event, _| {
            if event.source != source {
                return Ok(None);
            }
            serde_json::from_value(event.payload.clone())
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })
        .map(Option::unwrap_or_default)
}

impl GrantState {
    pub fn matchers(&self) -> Vec<GrantMatcher> {
        self.grants
            .values()
            .flat_map(|grant| grant.matchers.iter().cloned())
            .collect()
    }

    pub fn allows(&self, request: &EventEnvelope, effects: Option<&Value>) -> bool {
        let tool = request.payload["tool"].as_str().unwrap_or_default();
        let arguments = &request.payload["arguments"];
        let matchers = self.matchers();
        if matchers.iter().any(|matcher| matches!(matcher,
            GrantMatcher::ExactArguments { tool: expected_tool, arguments: expected, effects: expected_effects }
                if expected_tool == tool && expected == arguments
                    && expected_effects.as_ref().is_none_or(|expected| Some(expected) == effects))) {
            return true;
        }
        arguments["command"].as_str().is_some_and(|command| {
            Invocation::parse(command).decision(&[], &matchers, tool, arguments) == Decision::Allow
        })
    }
}

/// A consumer reads authoritative JSON snapshots, never a UI-maintained allow
/// list. Custom assemblies can rename both service instances through config.
#[derive(Clone, Debug)]
pub struct AuthorizationSources {
    pub operations: String,
    pub interfaces: String,
}

impl Default for AuthorizationSources {
    fn default() -> Self {
        Self {
            operations: INSTANCE.into(),
            interfaces: interface_permissions::INSTANCE.into(),
        }
    }
}

impl AuthorizationSources {
    pub fn from_config(config: Option<&Value>) -> Self {
        let mut sources = Self::default();
        if let Some(value) = config.and_then(|v| v["operationPolicy"].as_str()) {
            sources.operations = value.into();
        }
        if let Some(value) = config.and_then(|v| v["interfacePermissions"].as_str()) {
            sources.interfaces = value.into();
        }
        sources
    }

    pub fn allowance(
        &self,
        reader: &LogReader,
        request: &EventEnvelope,
    ) -> io::Result<Option<Value>> {
        self.allowance_with_effects(reader, request, None)
    }

    pub fn allowance_with_effects(
        &self,
        reader: &LogReader,
        request: &EventEnvelope,
        effects: Option<&Value>,
    ) -> io::Result<Option<Value>> {
        let grants = read_grants(reader, &self.operations)?;
        if grants.allows(request, effects) {
            return Ok(Some(json!({"scope":"flow", "policy":self.operations})));
        }
        interface_permissions::allowance(reader, request, &self.interfaces)?
            .map(|evidence| {
                serde_json::to_value(evidence)
                    .map(|evidence| json!({"scope":"interface", "evidence":evidence}))
                    .map_err(io::Error::other)
            })
            .transpose()
    }
}
