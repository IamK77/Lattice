//! Chat-compatible response extensions. These remain provider data, not core rules.
use serde_json::{json, Map, Value};

/// Tool metadata contains complete values, not text deltas. In particular a
/// repeated Gemini signature must not be concatenated with its previous value.
pub(super) fn merge(target: &mut Value, delta: &Value) {
    match (target, delta) {
        (Value::Object(target), Value::Object(delta)) => {
            for (key, value) in delta {
                if let Some(old) = target
                    .get_mut(key)
                    .filter(|v| v.is_object() && value.is_object())
                {
                    merge(old, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, delta) => *target = delta.clone(),
    }
}

fn same_detail(left: &Value, right: &Value) -> bool {
    left["type"] == right["type"]
        && matches!(
            left["type"].as_str(),
            Some("reasoning.text" | "reasoning.summary")
        )
        && ["id", "format", "index", "signature"].iter().all(|key| {
            left[*key].is_null()
                || right[*key].is_null()
                || left[*key] == ""
                || right[*key] == ""
                || left[*key] == right[*key]
        })
}

fn append_detail(left: &mut Value, right: &Value) {
    for (key, value) in right.as_object().into_iter().flatten() {
        if matches!(key.as_str(), "text" | "summary") {
            if let Some(next) = value.as_str() {
                let text = format!("{}{next}", left[key].as_str().unwrap_or_default());
                left[key] = json!(text);
            }
        } else if left[key].is_null() || left[key] == "" {
            left[key] = value.clone();
        }
    }
}

#[derive(Default)]
pub(super) struct Message {
    fields: Map<String, Value>,
    details: Vec<Value>,
    content: Vec<Value>,
    typed_content: bool,
    plain_content: String,
}
impl Message {
    pub fn apply(&mut self, delta: &Value) {
        for key in ["reasoning_content", "reasoning"] {
            if let Some(text) = delta[key].as_str() {
                let value = self.fields.entry(key).or_insert_with(|| json!(""));
                *value = json!(format!("{}{text}", value.as_str().unwrap_or_default()));
            }
        }
        if let Some(details) = delta["reasoning_details"].as_array() {
            for part in details {
                // OpenRouter can reuse index 0 for successive logical blocks.
                // Join only adjacent compatible text/summary chunks; encrypted
                // blocks and type/identity transitions remain separate and ordered.
                match self
                    .details
                    .last_mut()
                    .filter(|last| same_detail(last, part))
                {
                    Some(last) => append_detail(last, part),
                    None => self.details.push(part.clone()),
                }
            }
        }
        if let Some(parts) = delta["content"].as_array() {
            self.typed_content = true;
            self.content.extend(parts.iter().cloned());
        } else if let Some(text) = delta["content"].as_str().filter(|text| !text.is_empty()) {
            self.plain_content.push_str(text);
            if let Some(last) = self
                .content
                .last_mut()
                .filter(|last| last["type"] == "text")
            {
                last["text"] = json!(format!(
                    "{}{text}",
                    last["text"].as_str().unwrap_or_default()
                ));
            } else {
                self.content.push(json!({"type":"text","text":text}));
            }
        }
    }
    pub fn value(&self) -> Value {
        let mut value = self.fields.clone();
        if !self.details.is_empty() {
            value.insert("reasoning_details".into(), json!(self.details));
        }
        if self.typed_content {
            value.insert("content".into(), json!(self.content));
        } else if self.plain_content.trim_start().starts_with("<think>") {
            // MiniMax's inline format needs the original assistant content back.
            value.insert("content".into(), json!(self.plain_content));
        }
        Value::Object(value)
    }
}

pub(super) fn content_fragments(delta: &Value) -> Vec<super::Fragment> {
    let mut fragments = Vec::new();
    for part in delta["content"].as_array().into_iter().flatten() {
        match part["type"].as_str() {
            Some("text") => {
                if let Some(text) = part["text"].as_str() {
                    fragments.push(super::Fragment::Text(text.into()));
                }
            }
            Some("thinking") => {
                for thought in part["thinking"].as_array().into_iter().flatten() {
                    if let Some(text) = thought["text"].as_str() {
                        fragments.push(super::Fragment::Reasoning(text.into()));
                    }
                }
            }
            _ => {}
        }
    }
    fragments
}

/// Existing DeepSeek/custom connections retain their established mapping. Known
/// services must not inherit DeepSeek's control field merely because they speak
/// Chat Completions. Model-specific effort words still come from the profile.
pub(super) fn configure_thinking(
    body: &mut Value,
    base: &str,
    thinking: Option<&super::Thinking>,
    rungs: &[String],
) -> Option<&'static str> {
    let url = reqwest::Url::parse(base).ok()?;
    let host = url.host_str()?;
    let known = matches!(
        host,
        "api.openai.com"
            | "api.anthropic.com"
            | "generativelanguage.googleapis.com"
            | "dashscope.aliyuncs.com"
            | "dashscope-intl.aliyuncs.com"
            | "dashscope-us.aliyuncs.com"
            | "api.moonshot.cn"
            | "api.moonshot.ai"
            | "open.bigmodel.cn"
            | "api.z.ai"
            | "api.minimax.cn"
            | "api.minimax.io"
            | "ark.cn-beijing.volces.com"
            | "api.siliconflow.cn"
            | "api.siliconflow.com"
            | "openrouter.ai"
            | "api.x.ai"
            | "us.api.x.ai"
            | "api.mistral.ai"
            | "api.eu.mistral.ai"
            | "api.us.mistral.ai"
            | "api.groq.com"
            | "api.together.ai"
    );
    if !known {
        return None;
    }
    body.as_object_mut().unwrap().remove("thinking");
    let requested = thinking?;
    let off = matches!(requested, super::Thinking::Off);
    if host == "api.anthropic.com"
        || rungs.is_empty()
        || (off && !rungs.iter().any(|r| r == "none"))
    {
        body.as_object_mut().unwrap().remove("reasoning_effort");
        return Some("This model has no configured support for the requested thinking control; using the provider default. Thinking is not guaranteed to be disabled.");
    }
    if host == "openrouter.ai" {
        if off {
            body["reasoning"] = json!({"enabled":false});
        } else if let Some(effort) = body.get("reasoning_effort").cloned() {
            body["reasoning"] = json!({"effort":effort});
        }
        body.as_object_mut().unwrap().remove("reasoning_effort");
    } else if host == "api.together.ai" && off {
        body["reasoning"] = json!({"enabled":false});
        body.as_object_mut().unwrap().remove("reasoning_effort");
    }
    None
}

pub(super) fn has_content(payload: &Value) -> bool {
    [
        "reasoning_content",
        "reasoning",
        "reasoning_details",
        "content",
    ]
    .iter()
    .any(|key| {
        let value = &payload["chatMessage"][*key];
        value.as_str().is_some_and(|s| !s.is_empty())
            || value.as_array().is_some_and(|a| !a.is_empty())
    })
}

/// Restore only fields owned by the Chat encoder, never arbitrary message keys.
pub(super) fn restore(message: &mut Value, payload: &Value) {
    if let Some(fields) = payload["chatMessage"].as_object() {
        for key in [
            "reasoning_content",
            "reasoning",
            "reasoning_details",
            "content",
        ] {
            if let Some(value) = fields.get(key) {
                message[key] = value.clone();
            }
        }
        if !fields.contains_key("reasoning_content")
            && ["reasoning", "reasoning_details", "content"]
                .iter()
                .any(|key| fields.contains_key(*key))
        {
            message.as_object_mut().unwrap().remove("reasoning_content");
        }
    }
}
