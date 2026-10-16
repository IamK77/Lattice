//! Resolve display references before authorization; execution still uses the
//! original identifier. Providers check the retained creation identity locally.
use super::{Kind, OriginalId, Projection, Reference};
use crate::LogReader;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const REFERENCE: &str = "backgroundReference";
pub const RESOLUTION_ERROR: &str = "backgroundReferenceError";

fn target(tool: &str) -> Option<(Kind, &'static str)> {
    match tool {
        "Unschedule" => Some((Kind::Timer, "timer")),
        "Unwatch" => Some((Kind::Watch, "watch")),
        "CancelExpert" => Some((Kind::Expert, "job")),
        _ => None,
    }
}

#[derive(Default)]
pub struct Resolver {
    projection: Option<Projection>,
}

impl Resolver {
    /// Legacy numbers are untouched. A rejected string remains a string, so
    /// the existing provider error path settles the call without cancellation.
    /// Diagnostic metadata is not authorization and never replaces arguments.
    pub fn normalize(&mut self, reader: &LogReader, request: &mut Value) {
        let Some((kind, field)) = request["tool"].as_str().and_then(target) else {
            return;
        };
        let Some(alias) = request["arguments"][field].as_str() else {
            return;
        };
        match self.resolve(reader, kind, alias) {
            Ok(reference) => {
                request["arguments"][field] = json!(reference.identity.original_id);
                request[REFERENCE] = json!(reference);
            }
            Err(message) => request[RESOLUTION_ERROR] = json!(message),
        }
    }

    fn resolve(
        &mut self,
        reader: &LogReader,
        kind: Kind,
        alias: &str,
    ) -> Result<Reference, String> {
        let projection = match &mut self.projection {
            Some(projection) => {
                projection
                    .advance(reader, reader.snapshot_end())
                    .map_err(|e| e.to_string())?;
                projection
            }
            slot @ None => slot.insert(
                Projection::recover(reader, reader.snapshot_end()).map_err(|e| e.to_string())?,
            ),
        };
        let identity = projection.resolve(alias).cloned().ok_or_else(|| {
            format!(
                "unknown background reference '{alias}' in stream '{}'",
                reader.stream()
            )
        })?;
        if identity.kind != kind {
            return Err(format!(
                "background reference '{alias}' identifies {:?}, not {kind:?}",
                identity.kind
            ));
        }
        if !projection
            .is_current(reader, &identity)
            .map_err(|e| e.to_string())?
        {
            return Err(format!(
                "stale background reference '{alias}': its original identifier has been reused"
            ));
        }
        projection.save(reader).map_err(|e| e.to_string())?;
        Ok(Reference {
            alias: alias.to_owned(),
            identity,
        })
    }
}

pub fn reference(payload: &Value) -> Result<Option<Reference>, String> {
    payload
        .get(REFERENCE)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|e| format!("invalid background reference metadata: {e}"))
        })
        .transpose()
}

/// Read the target of an operation receipt without treating it as a creation.
/// Invalid metadata is an operation error, not a new alias reservation.
pub fn receipt_identity(
    reader: &LogReader,
    event: &crate::EventEnvelope,
) -> std::io::Result<Option<super::Identity>> {
    if event.event_type != crate::core_events::TOOL_EXEC_COMPLETED {
        return Ok(None);
    }
    for cause in &event.causes {
        let Some(header) = reader.header(cause)? else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("background operation cause is absent: {cause}"),
            ));
        };
        if header.event_type != crate::core_events::TOOL_EXEC_STARTED {
            continue;
        }
        let Some((kind, field)) = header.tool.as_deref().and_then(target) else {
            continue;
        };
        let Some(request) = reader.get(cause)? else {
            return Err(std::io::Error::other(
                "background operation request is absent",
            ));
        };
        let Ok(Some(reference)) = reference(&request.payload) else {
            continue;
        };
        if validate(
            &request.payload,
            reader.stream(),
            kind,
            request.payload["arguments"][field].as_u64(),
            Some(&reference.identity.created_by),
        )
        .is_ok()
        {
            return Ok(Some(reference.identity));
        }
    }
    Ok(None)
}

/// The final guard runs against provider-owned live state, not a parent log.
/// This also works in a bridge child whose history reader is intentionally empty.
pub fn validate(
    payload: &Value,
    stream: &str,
    kind: Kind,
    id: Option<u64>,
    live_creation: Option<&str>,
) -> Result<(), String> {
    if let Some(message) = payload[RESOLUTION_ERROR].as_str() {
        return Err(message.to_owned());
    }
    let Some(reference) = reference(payload)? else {
        return Ok(());
    };
    if reference.identity.stream != stream
        || reference.identity.kind != kind
        || id.map(OriginalId::Number).as_ref() != Some(&reference.identity.original_id)
        || live_creation != Some(reference.identity.created_by.as_str())
    {
        return Err(format!("stale or mismatched background reference '{}': that invocation is not the current live target", reference.alias));
    }
    Ok(())
}

pub fn failure(message: impl Into<String>) -> Value {
    json!({"status":"error","error":{
        "code":"background.invalid_reference", "message":message.into(),
        "blame":"request", "retryable":false, "transient":false
    }})
}

/// Local incarnation witnesses only; cancellation and worker behavior remain
/// owned by the existing provider. Dropping a worker retires its witness.
#[derive(Default)]
pub(crate) struct LiveTargets(Arc<Mutex<HashMap<u64, String>>>);

impl LiveTargets {
    pub(crate) fn track(&self, id: u64, creation: &str) -> TargetLease {
        self.0.lock().unwrap().insert(id, creation.to_owned());
        TargetLease {
            targets: Arc::clone(&self.0),
            id,
            creation: creation.to_owned(),
        }
    }

    pub(crate) fn creation(&self, id: Option<u64>) -> Option<String> {
        id.and_then(|id| self.0.lock().unwrap().get(&id).cloned())
    }
}

pub(crate) struct TargetLease {
    targets: Arc<Mutex<HashMap<u64, String>>>,
    id: u64,
    creation: String,
}

impl Drop for TargetLease {
    fn drop(&mut self) {
        let mut targets = self.targets.lock().unwrap();
        if targets.get(&self.id) == Some(&self.creation) {
            targets.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests;
