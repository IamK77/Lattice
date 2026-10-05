//! Editable capability draft. Sources describe suggestions, not verified runtime support.
use super::super::{choose, Questions, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Capabilities {
    pub value: Value,
    sources: BTreeMap<String, String>,
    edited: BTreeSet<String>,
}

impl Capabilities {
    pub fn new(
        model: &str,
        adapter: &str,
        remote: &Value,
        input_limit: Option<u64>,
        base_url: &str,
    ) -> Self {
        let template: Value =
            serde_json::from_str(include_str!("deepseek.json")).expect("tested template");
        let mut value = lattice::profile::lookup(model).unwrap_or_else(|| json!({}));
        if template["entry"]["model"] == model {
            value = template["entry"]["profile"].clone();
        }
        value
            .as_object_mut()
            .expect("profile object")
            .remove("model");
        let mut sources: BTreeMap<String, String> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), "bundled profile".into()))
            .collect();
        if let Some(remote) = remote.as_object() {
            for (key, val) in remote {
                if value.get(key).is_some_and(|old| old != val) {
                    sources.insert(
                        key.clone(),
                        "service metadata (differs from bundled profile)".into(),
                    );
                } else {
                    sources.insert(key.clone(), "service metadata".into());
                }
                value[key] = val.clone();
            }
        }
        if let Some(limit) = input_limit {
            // An input ceiling is not a total window. Use it only as a
            // conservative draft budget, visibly labelled for confirmation;
            // never add output tokens and pretend the sum was reported.
            value["contextWindow"] =
                json!(value["contextWindow"].as_u64().unwrap_or(limit).min(limit));
            sources.insert("contextWindow".into(), format!("service input ceiling {limit}, used as a conservative total budget; confirm or edit"));
        }
        // Usage names describe the selected dialect, not the model's training lab.
        // In particular a DeepSeek Messages endpoint does not use Chat usage names.
        let entry = lattice::models::Entry {
            id: String::new(),
            adapter: adapter.into(),
            model: String::new(),
            base_url: String::new(),
            key_env: String::new(),
            profile: None,
        };
        value["usageFields"] = json!(entry.usage_fields());
        if adapter == "openai"
            && reqwest::Url::parse(base_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .as_deref()
                != Some("api.deepseek.com")
        {
            value["usageFields"]["cacheRead"] = json!("prompt_tokens_details.cached_tokens");
        }
        sources.insert(
            "usageFields".into(),
            "protocol defaults; editable for this service".into(),
        );
        for (key, fallback) in [("acceptsImages", json!(false)), ("effort", json!([]))] {
            if value.get(key).is_none() {
                value[key] = fallback;
                sources.insert(
                    key.into(),
                    if key == "effort" {
                        "unknown; no declared rungs (not a thinking-mode switch)"
                    } else {
                        "unknown; not enabled"
                    }
                    .into(),
                );
            }
        }
        // Hosted tools require both model and serving endpoint support. Never
        // enable them just because a global model profile advertises them.
        for key in ["nativeWebSearch", "nativeImageGeneration"] {
            value[key] = json!(false);
            sources.insert(
                key.into(),
                "not enabled; confirm endpoint support in advanced settings".into(),
            );
        }
        Self {
            value,
            sources,
            edited: BTreeSet::new(),
        }
    }

    pub fn refresh(&mut self, mut incoming: Self) {
        for key in &self.edited {
            incoming.value[key] = self.value[key].clone();
            if let Some(source) = self.sources.get(key) {
                incoming.sources.insert(key.clone(), source.clone());
            }
        }
        incoming.edited = self.edited.clone();
        *self = incoming;
    }

    fn set(&mut self, key: &str, value: Value) {
        self.edited.insert(key.into());
        self.value[key] = value;
        self.sources.insert(key.into(), "your selection".into());
    }

    pub fn show(&self, ui: &mut impl Questions) {
        ui.tell("Model capabilities (metadata is not a live compatibility test):");
        for (key, label) in [
            ("contextWindow", "Context window"),
            ("maxOutputTokens", "Maximum output tokens"),
            ("acceptsImages", "Image input"),
            ("effort", "Thinking effort rungs"),
            ("nativeWebSearch", "Hosted web search"),
            ("nativeImageGeneration", "Hosted image generation"),
            ("usageFields", "Usage field mapping"),
        ] {
            let value = self
                .value
                .get(key)
                .map(Value::to_string)
                .unwrap_or_else(|| "unknown".into());
            let source = self
                .sources
                .get(key)
                .map(String::as_str)
                .unwrap_or("unknown; fill in before saving");
            ui.tell(&format!("{label}: {value} — {source}"));
        }
    }

    pub fn valid(&self) -> bool {
        matches!((self.value["contextWindow"].as_u64(), self.value["maxOutputTokens"].as_u64()), (Some(c), Some(o)) if o > 0 && c > o)
    }

    pub fn edit(&mut self, ui: &mut impl Questions, adapter: &str) -> Result<bool> {
        loop {
            self.show(ui);
            match choose(ui, "Confirm model capabilities", &[
                "Confirm and continue", "Edit context and output limits", "Edit image input support",
                "Edit thinking effort rungs", "Advanced settings", "Back to model selection",
            ])? {
                0 if self.valid() => return Ok(true),
                0 => ui.tell("Fill in positive context/output limits; the output limit must leave room for input. No guessed limits will be saved."),
                1 => {
                    for (key, label) in [("contextWindow", "Context window in tokens"), ("maxOutputTokens", "Maximum output tokens")] {
                        loop {
                            let default = self.value[key].as_u64().map(|n| n.to_string()).unwrap_or_default();
                            let text = ui.text(label, &default)?;
                            match text.trim().parse::<u64>() {
                                Ok(n) if n > 0 => { self.set(key, json!(n)); break; }
                                _ => ui.tell("Enter a positive whole number."),
                            }
                        }
                    }
                }
                2 => {
                    let selected = choose(ui, "Can this model receive images?", &["Supported", "Not supported", "Unknown — keep image input disabled"])?;
                    self.set("acceptsImages", json!(selected == 0));
                    if selected == 2 { self.sources.insert("acceptsImages".into(), "unknown; not enabled".into()); }
                }
                3 => {
                    let old = self.value["effort"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    let text = ui.text("Effort rungs, weakest first, comma-separated (empty = unknown)", &old)?;
                    let rungs: Vec<_> = text.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
                    if rungs.iter().any(|s| !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')) {
                        ui.tell("Use provider effort names, separated by commas.");
                    } else { self.set("effort", json!(rungs)); }
                }
                4 => self.advanced(ui, adapter)?,
                _ => return Ok(false),
            }
        }
    }

    fn advanced(&mut self, ui: &mut impl Questions, adapter: &str) -> Result<()> {
        if adapter == "responses" {
            ui.tell("Enable hosted tools only if BOTH this model and this serving endpoint support them.");
            for (key, label) in [
                ("nativeWebSearch", "Enable hosted web search?"),
                (
                    "nativeImageGeneration",
                    "Enable hosted image generation (not image input)?",
                ),
            ] {
                let selected = ui.confirm(label, self.value[key] == true)?;
                self.set(key, json!(selected));
            }
        } else {
            ui.tell(
                "Hosted search and image generation settings apply only to the Responses adapter.",
            );
        }
        if ui.confirm("Edit usage field mapping?", false)? {
            let mut fields = self.value["usageFields"].clone();
            for key in ["input", "output", "cacheRead", "cacheWrite"] {
                loop {
                    let text = ui.text(
                        &format!("Usage field for {key} (dot-separated path)"),
                        fields[key].as_str().unwrap_or(""),
                    )?;
                    let text = text.trim();
                    if (text.is_empty() && ["input", "output"].contains(&key))
                        || text.chars().any(char::is_control)
                    {
                        ui.tell("Input/output mappings must not be empty; control characters are not allowed.");
                        continue;
                    }
                    if text.is_empty() {
                        fields.as_object_mut().unwrap().remove(key);
                    } else {
                        fields[key] = json!(text);
                    }
                    break;
                }
            }
            self.set("usageFields", fields);
        }
        Ok(())
    }
}
