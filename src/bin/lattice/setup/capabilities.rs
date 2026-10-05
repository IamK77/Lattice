//! Editable capability draft. Sources describe suggestions, not verified runtime support.
use super::super::{choose, Questions, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Capabilities {
    pub value: Value,
    sources: BTreeMap<String, String>,
    edited: BTreeSet<String>,
}

/// Decimal suffixes are exact arithmetic, never binary units or floating-point rounding.
pub(super) fn parse_tokens(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || text.len() > 64 {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let (number, multiplier) = if let Some(number) = lower.strip_suffix('m') {
        (number.trim(), 1_000_000u128)
    } else if let Some(number) = lower.strip_suffix('k') {
        (number.trim(), 1_000u128)
    } else {
        (lower.as_str(), 1u128)
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (number.contains('.') && fraction.is_empty())
    {
        return None;
    }
    let scale = 10u128.checked_pow(fraction.len().try_into().ok()?)?;
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u128>().ok()?
    };
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().ok()?
    };
    let scaled = whole
        .checked_mul(scale)?
        .checked_add(fraction)?
        .checked_mul(multiplier)?;
    if scaled % scale != 0 {
        return None;
    }
    u64::try_from(scaled / scale).ok().filter(|n| *n > 0)
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
                "Confirm and continue", "Edit context and output limits", "Select enabled capabilities",
                "Edit thinking effort rungs", "Advanced settings", "Back to model selection",
            ])? {
                0 if self.valid() => return Ok(true),
                0 => ui.tell("Fill in positive context/output limits; the output limit must leave room for input. No guessed limits will be saved."),
                1 => {
                    for (key, label) in [("contextWindow", "Context window in tokens"), ("maxOutputTokens", "Maximum output tokens")] {
                        loop {
                            let default = self.value[key].as_u64().map(|n| n.to_string()).unwrap_or_default();
                            let text = ui.text(&format!("{label} (1k = 1000; 1M = 1000000; decimals allowed)"), &default)?;
                            match parse_tokens(&text) {
                                Some(n) => { ui.tell(&format!("{label}: {n} tokens")); self.set(key, json!(n)); break; }
                                None => ui.tell("Enter a positive whole token count, e.g. 1000000, 1000k or 1M. k/M are decimal, not binary; fractional tokens and overflow are rejected."),
                            }
                        }
                    }
                }
                2 => self.switches(ui, adapter)?,
                3 => {
                    let old = self.value["effort"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                    let text = ui.text("Effort rungs, weakest first, comma-separated (empty = unknown)", &old)?;
                    let rungs: Vec<_> = text.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
                    if rungs.iter().any(|s| !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')) {
                        ui.tell("Use provider effort names, separated by commas.");
                    } else { self.set("effort", json!(rungs)); }
                }
                4 => self.advanced(ui)?,
                _ => return Ok(false),
            }
        }
    }

    fn switches(&mut self, ui: &mut impl Questions, adapter: &str) -> Result<()> {
        let mut fields = vec![("acceptsImages", "Image input (send images to the model)")];
        if adapter == "responses" {
            fields.extend([
                ("nativeWebSearch", "Hosted web search"),
                (
                    "nativeImageGeneration",
                    "Hosted image generation (create images)",
                ),
            ]);
        } else {
            ui.tell("Hosted search and image generation require the Responses adapter; they are not offered for this format.");
        }
        ui.tell("Select capabilities supported by BOTH this model and endpoint. Unknown capabilities remain unchecked. Unchecked means not enabled, not a claim that the model cannot support it.");
        let options = fields
            .iter()
            .map(|(_, label)| (*label).to_owned())
            .collect::<Vec<_>>();
        let checked = fields
            .iter()
            .enumerate()
            .filter_map(|(i, (key, _))| (self.value[*key] == true).then_some(i))
            .collect::<Vec<_>>();
        let selected =
            ui.multi_select("Enabled capabilities (Space to toggle)", &options, &checked)?;
        for (index, (key, _)) in fields.iter().enumerate() {
            let enabled = selected.contains(&index);
            if enabled != (self.value[*key] == true) {
                self.set(key, json!(enabled));
            }
        }
        Ok(())
    }

    fn advanced(&mut self, ui: &mut impl Questions) -> Result<()> {
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
