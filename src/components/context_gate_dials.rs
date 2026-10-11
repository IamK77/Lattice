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
            // Historical dials are settings, not proof that today's adapter
            // supports the target's native-compaction extension.
            if let Some(target) = &gate.native_target {
                if !crate::components::model_compat::native_compaction(
                    "responses",
                    target["baseUrl"].as_str().unwrap_or_default(),
                ) {
                    gate.native_compaction = false;
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn historical_dials_cannot_enable_native_compaction_on_an_incompatible_target() {
        for (base, expected) in [
            ("https://api.groq.com/openai/v1", false),
            ("https://openrouter.ai/api/v1", false),
            ("https://api.openai.com/v1", true),
            ("https://custom.invalid/v1", true),
        ] {
            let mut dials = Dials::default();
            dials.observe(
                &json!({"channel":MODEL_CHANNEL,"model":"synthetic", "nativeCompaction":true,
                "nativeTarget":{"baseUrl":base,"model":"synthetic"}}),
            );
            // Check both replayed events and serialized checkpoint recovery.
            let restored: Dials =
                serde_json::from_value(serde_json::to_value(&dials).unwrap()).unwrap();
            for state in [dials, restored] {
                let mut gate = ContextGate::from_config(Some(&json!({"nativeCompaction":false})));
                state.apply(&mut gate);
                assert_eq!(gate.native_compaction, expected, "{base}");
            }
        }
    }
}
