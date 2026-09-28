//! Exact conditional setting updates shared by delivery and recovery.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ContextGate, DEFAULT_USAGE_CACHE_READ, DEFAULT_USAGE_INPUT, EFFORT_CHANNEL, MODEL_CHANNEL,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Dials {
    // A present null is different from an absent setting (notably thinking
    // and nativeTarget). A JSON map preserves that distinction on roundtrip.
    values: BTreeMap<String, Value>,
}

impl Dials {
    pub fn observe(&mut self, payload: &Value) {
        match payload["channel"].as_str() {
            Some(EFFORT_CHANNEL) => {
                self.values
                    .insert("thinking".into(), payload["value"].clone());
            }
            Some(MODEL_CHANNEL) => {
                if payload["nativeCompaction"].is_boolean() {
                    self.values.insert(
                        "nativeCompaction".into(),
                        payload["nativeCompaction"].clone(),
                    );
                    self.values.remove("nativeTarget");
                    if let Some(target) = payload.get("nativeTarget") {
                        self.values.insert("nativeTarget".into(), target.clone());
                    }
                }
                if payload["model"].is_string() {
                    self.values.insert("model".into(), payload["model"].clone());
                }
                if payload["contextWindow"].as_u64().is_some() {
                    self.values
                        .insert("contextWindow".into(), payload["contextWindow"].clone());
                }
                if let Some(fields) = payload["usageFields"]
                    .as_object()
                    .filter(|fields| !fields.is_empty())
                {
                    self.values.insert(
                        "input".into(),
                        Value::String(
                            fields
                                .get("input")
                                .and_then(Value::as_str)
                                .unwrap_or(DEFAULT_USAGE_INPUT)
                                .into(),
                        ),
                    );
                    self.values.insert(
                        "cacheRead".into(),
                        Value::String(
                            fields
                                .get("cacheRead")
                                .and_then(Value::as_str)
                                .unwrap_or(DEFAULT_USAGE_CACHE_READ)
                                .into(),
                        ),
                    );
                }
            }
            _ => {}
        }
    }

    pub fn apply(&self, gate: &mut ContextGate) {
        if let Some(thinking) = self.values.get("thinking") {
            gate.thinking = Some(thinking.clone());
        }
        if let Some(enabled) = self.values.get("nativeCompaction").and_then(Value::as_bool) {
            gate.native_compaction = enabled;
            gate.native_target = self.values.get("nativeTarget").cloned();
        }
        if let Some(name) = self.values.get("model").and_then(Value::as_str) {
            gate.model_name = Some(name.into());
        }
        if let Some(window) = self.values.get("contextWindow").and_then(Value::as_u64) {
            gate.context_window = Some(window);
        }
        if let Some(field) = self.values.get("input").and_then(Value::as_str) {
            gate.usage_input_field = field.into();
        }
        if let Some(field) = self.values.get("cacheRead").and_then(Value::as_str) {
            gate.usage_cache_field = field.into();
        }
    }
}
