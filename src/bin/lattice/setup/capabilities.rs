//! Editable capability draft. Sources describe suggestions, not verified runtime support.
#[cfg(test)]
pub(super) use super::super::input::parse_tokens;
use super::super::{
    choose,
    input::{format_tokens, Field},
    Error, Questions, Result, M,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Source {
    label: M,
    input_ceiling: Option<u64>,
}
impl From<M> for Source {
    fn from(label: M) -> Self {
        Self {
            label,
            input_ceiling: None,
        }
    }
}
pub(super) struct Capabilities {
    pub value: Value,
    sources: BTreeMap<String, Source>,
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
        let mut sources: BTreeMap<String, Source> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), M::SourceBundled.into()))
            .collect();
        if let Some(remote) = remote.as_object() {
            for (key, val) in remote {
                let source = if value.get(key).is_some_and(|old| old != val) {
                    M::SourceConflict
                } else {
                    M::SourceRemote
                };
                sources.insert(key.clone(), source.into());
                value[key] = val.clone();
            }
        }
        if let Some(limit) = input_limit {
            // An input ceiling is not a total window. Never add output tokens
            // and pretend that the sum was reported by the service.
            value["contextWindow"] =
                json!(value["contextWindow"].as_u64().unwrap_or(limit).min(limit));
            sources.insert(
                "contextWindow".into(),
                Source {
                    label: M::SourceInputCeiling,
                    input_ceiling: Some(limit),
                },
            );
        }
        // Usage names describe the selected dialect, not the model's training lab.
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
        sources.insert("usageFields".into(), M::SourceProtocol.into());
        for (key, fallback, source) in [
            ("acceptsImages", json!(false), M::SourceUnknownImage),
            ("effort", json!([]), M::SourceUnknownEffort),
        ] {
            if value.get(key).is_none() {
                value[key] = fallback;
                sources.insert(key.into(), source.into());
            }
        }
        // Hosted tools require endpoint support; a bundled model profile is not enough.
        for key in ["nativeWebSearch", "nativeImageGeneration"] {
            value[key] = json!(false);
            sources.insert(key.into(), M::SourceHosted.into());
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
        self.sources.insert(key.into(), M::SourceEdited.into());
    }
    pub fn show(&self, ui: &mut impl Questions) {
        ui.say(M::CapabilitiesTitle, &[]);
        for (key, label) in [
            ("contextWindow", M::ContextWindow),
            ("maxOutputTokens", M::OutputLimit),
            ("acceptsImages", M::ImageInput),
            ("effort", M::EffortRungs),
            ("nativeWebSearch", M::WebSearch),
            ("nativeImageGeneration", M::ImageGeneration),
        ] {
            let value = match self.value.get(key) {
                Some(Value::Number(n)) => n
                    .as_u64()
                    .map(format_tokens)
                    .unwrap_or_else(|| n.to_string()),
                Some(Value::Bool(b)) => ui.label(if *b { M::Enabled } else { M::Disabled }),
                Some(value) => value.to_string(),
                None => ui.label(M::Unknown),
            };
            let source = self
                .sources
                .get(key)
                .map(|s| {
                    if let Some(limit) = s.input_ceiling {
                        ui.message(s.label, &[&limit.to_string()])
                    } else {
                        ui.label(s.label)
                    }
                })
                .unwrap_or_else(|| ui.label(M::SourceMissing));
            ui.say(M::FieldSummary, &[&ui.label(label), &value, &source]);
        }
    }
    pub fn valid(&self) -> bool {
        matches!((self.value["contextWindow"].as_u64(), self.value["maxOutputTokens"].as_u64()), (Some(c), Some(o)) if o > 0 && c > o)
    }
    pub fn limits(&mut self, ui: &mut impl Questions, missing_only: bool) -> Result<()> {
        for (key, label) in [
            ("contextWindow", M::ContextInput),
            ("maxOutputTokens", M::OutputInput),
        ] {
            let default = self.value[key]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_default();
            let field = if key == "maxOutputTokens" {
                Field::OutputTokens(self.value["contextWindow"].as_u64().unwrap_or(0))
            } else {
                Field::Tokens
            };
            if missing_only && field.validate(&default).is_ok() {
                continue;
            }
            let text = ui.input(&ui.label(label), &default, field, &[])?;
            self.set(
                key,
                json!(super::super::input::parse_tokens(&text).expect("validated tokens")),
            );
        }
        Ok(())
    }
    pub fn edit(&mut self, ui: &mut impl Questions, adapter: &str) -> Result<bool> {
        loop {
            self.show(ui);
            let choice = match choose(
                ui,
                M::CapabilitiesMenu,
                &[
                    M::Back,
                    M::EditLimits,
                    M::SelectCapabilities,
                    M::EditEffort,
                    M::Advanced,
                ],
            ) {
                Err(Error::Back) => return Ok(false),
                other => other?,
            };
            let result = match choice {
                1 => self.limits(ui, false),
                2 => self.switches(ui, adapter),
                3 => self.effort_rungs(ui),
                4 => self.advanced(ui),
                _ => return Ok(true),
            };
            match result {
                Ok(()) | Err(Error::Back) => {}
                Err(e) => return Err(e),
            }
        }
    }
    fn switches(&mut self, ui: &mut impl Questions, adapter: &str) -> Result<()> {
        let mut fields = vec![("acceptsImages", M::ImageInputChoice)];
        if adapter == "responses" {
            fields.extend([
                ("nativeWebSearch", M::WebSearch),
                ("nativeImageGeneration", M::ImageGenerationChoice),
            ]);
        } else {
            ui.say(M::HostedUnavailable, &[]);
        }
        ui.say(M::SwitchHelp, &[]);
        let options = fields
            .iter()
            .map(|(_, label)| ui.label(*label))
            .collect::<Vec<_>>();
        let checked = fields
            .iter()
            .enumerate()
            .filter_map(|(i, (key, _))| (self.value[*key] == true).then_some(i))
            .collect::<Vec<_>>();
        let selected = ui.multi_select(&ui.label(M::SwitchMenu), &options, &checked)?;
        for (index, (key, _)) in fields.iter().enumerate() {
            let enabled = selected.contains(&index);
            if enabled != (self.value[*key] == true) {
                self.set(key, json!(enabled));
            }
        }
        Ok(())
    }
    fn effort_rungs(&mut self, ui: &mut impl Questions) -> Result<()> {
        let current = self.value["effort"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let standard = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
        let nonstandard = current.iter().any(|s| !standard.contains(&s.as_str()));
        // Never guess where a provider-specific name belongs on its ladder.
        let rungs = if nonstandard {
            current.clone()
        } else {
            standard.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()
        };
        if nonstandard {
            ui.say(M::EffortCustomNotice, &[]);
        }
        ui.say(M::EffortHelp, &[]);
        let options = rungs
            .iter()
            .map(|s| {
                if s == "none" {
                    ui.label(M::EffortNone)
                } else {
                    s.clone()
                }
            })
            .collect::<Vec<_>>();
        let defaults = rungs
            .iter()
            .enumerate()
            .filter_map(|(i, rung)| current.contains(rung).then_some(i))
            .collect::<Vec<_>>();
        let selected = ui.multi_select(&ui.label(M::EffortMenu), &options, &defaults)?;
        let chosen = rungs
            .into_iter()
            .enumerate()
            .filter_map(|(i, rung)| selected.contains(&i).then_some(rung))
            .collect::<Vec<_>>();
        if chosen != current {
            self.set("effort", json!(chosen));
        }
        Ok(())
    }
    fn custom_effort(&mut self, ui: &mut impl Questions) -> Result<()> {
        let old = self.value["effort"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let text = ui.input(&ui.label(M::EffortInput), &old, Field::Effort, &[])?;
        let rungs: Vec<_> = text
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.set("effort", json!(rungs));
        Ok(())
    }
    fn advanced(&mut self, ui: &mut impl Questions) -> Result<()> {
        match choose(
            ui,
            M::AdvancedMenu,
            &[M::UsageMapping, M::CustomEffort, M::Back],
        )? {
            1 => return self.custom_effort(ui),
            2 => return Ok(()),
            _ => {}
        }
        for key in ["input", "output", "cacheRead", "cacheWrite"] {
            let text = ui.input(
                &ui.message(M::UsageInput, &[key]),
                self.value["usageFields"][key].as_str().unwrap_or(""),
                Field::Usage {
                    required: ["input", "output"].contains(&key),
                },
                &[],
            )?;
            let mut fields = self.value["usageFields"].clone();
            if text.is_empty() {
                fields.as_object_mut().unwrap().remove(key);
            } else {
                fields[key] = json!(text);
            }
            self.set("usageFields", fields);
        }
        Ok(())
    }
}
