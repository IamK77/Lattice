//! Explicit, bounded model discovery at the selected service, never a third-party catalog.
//! Field/path references (optional metadata remains optional):
//! https://developers.openai.com/api/reference/resources/models/methods/list
//! https://platform.claude.com/docs/en/api/models/list
//! https://api-docs.deepseek.com/api/list-models/
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Model {
    pub id: String,
    pub profile: Value,
    // Anthropic reports an input ceiling, not a combined context window.
    pub input_limit: Option<u64>,
}

pub(super) fn endpoint(spec: &Value) -> Result<(reqwest::Url, bool), String> {
    super::validate_target(
        &json!({"adapter":spec["adapter"],"model":"discovery","baseUrl":spec["baseUrl"]}),
    )?;
    let base = spec["baseUrl"]
        .as_str()
        .ok_or("missing endpoint")?
        .trim_end_matches('/');
    let mut url = reqwest::Url::parse(base).map_err(|_| "invalid endpoint")?;
    // DeepSeek exposes its catalog at its common API root, including when
    // Messages calls use the provider's /anthropic compatibility prefix.
    let deepseek = url.host_str() == Some("api.deepseek.com") && url.path() == "/anthropic";
    let anthropic = spec["adapter"] == "anthropic" && !deepseek;
    if deepseek {
        url.set_path("/models");
    } else {
        url = reqwest::Url::parse(&format!(
            "{base}{}",
            if anthropic { "/v1/models" } else { "/models" }
        ))
        .map_err(|_| "invalid model-list endpoint")?;
    }
    Ok((url, anthropic))
}

pub(super) fn key(spec: &Value) -> Result<String, String> {
    spec["apiKey"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            spec["apiKeyEnv"]
                .as_str()
                .and_then(|name| std::env::var(name).ok())
        })
        .filter(|key| !key.is_empty())
        .ok_or_else(|| "the configured credential is missing or empty".into())
}

pub(super) fn valid_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
}

fn parse_page(value: &Value, anthropic: bool) -> Result<Vec<Model>, String> {
    let data = value["data"]
        .as_array()
        .ok_or("model list has no data array")?;
    if data.len() > 2000 {
        return Err("model list exceeds the entry limit".into());
    }
    data.iter()
        .map(|item| {
            let id = item["id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or("model list contains an invalid identifier")?;
            let mut profile = json!({});
            if anthropic {
                if let Some(n) = item["max_tokens"].as_u64().filter(|n| *n > 0) {
                    profile["maxOutputTokens"] = json!(n);
                }
                if let Some(images) = item["capabilities"]["image_input"]["supported"].as_bool() {
                    profile["acceptsImages"] = json!(images);
                }
                let effort = &item["capabilities"]["effort"];
                if let Some(supported) = effort["supported"].as_bool() {
                    let rungs: Vec<_> = ["minimal", "low", "medium", "high", "xhigh", "max"]
                        .into_iter()
                        .filter(|rung| supported && effort[*rung]["supported"] == true)
                        .collect();
                    profile["effort"] = json!(rungs);
                }
            } else {
                // Documented DeepSeek metadata; absent fields remain unknown.
                for (remote, local) in [
                    ("context_window", "contextWindow"),
                    ("max_output_tokens", "maxOutputTokens"),
                ] {
                    if let Some(n) = item[remote].as_u64().filter(|n| *n > 0) {
                        profile[local] = json!(n);
                    }
                }
                if let Some(modalities) = item["input_modalities"]
                    .as_array()
                    .filter(|a| a.iter().all(Value::is_string))
                {
                    profile["acceptsImages"] = json!(modalities.iter().any(|m| m == "image"));
                }
                if let Some(rungs) = item["effort"]["supported_levels"]
                    .as_array()
                    .filter(|a| a.iter().all(|v| v.as_str().is_some_and(valid_id)))
                {
                    profile["effort"] = json!(rungs);
                }
            }
            let input_limit = if anthropic {
                item["max_input_tokens"].as_u64().filter(|n| *n > 0)
            } else {
                None
            };
            Ok(Model {
                id: id.into(),
                profile,
                input_limit,
            })
        })
        .collect()
}

pub(super) async fn fetch(spec: &Value, key: &str) -> Result<Vec<Model>, String> {
    let (base, anthropic) = endpoint(spec)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "cannot initialize model-list client")?;
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut ids = HashSet::new();
    let mut models = Vec::new();
    for _ in 0..10 {
        let mut url = base.clone();
        if anthropic {
            url.query_pairs_mut().append_pair("limit", "1000");
            if let Some(cursor) = &cursor {
                url.query_pairs_mut().append_pair("after_id", cursor);
            }
        }
        let request = client.get(url);
        let request = if anthropic {
            request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
        } else {
            request.bearer_auth(key)
        };
        let mut response = request
            .send()
            .await
            .map_err(|_| "model-list request failed or timed out; it was not retried")?;
        if !response.status().is_success() {
            return Err(format!(
                "model-list endpoint returned HTTP {}; manual entry is still available",
                response.status().as_u16()
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "model-list response was interrupted")?
        {
            if bytes.len() + chunk.len() > 1_048_576 {
                return Err("model-list response exceeds the size limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let page: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "model-list endpoint returned invalid JSON")?;
        for model in parse_page(&page, anthropic)? {
            if ids.insert(model.id.clone()) {
                models.push(model);
            }
            if models.len() > 2000 {
                return Err("model list exceeds the entry limit".into());
            }
        }
        if page["has_more"] != true {
            models.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(models);
        }
        if !anthropic {
            return Err("unsupported model-list pagination; enter a model manually".into());
        }
        let next = page["last_id"]
            .as_str()
            .filter(|id| ids.contains(*id))
            .ok_or("invalid model-list pagination cursor")?;
        if !cursors.insert(next.to_owned()) {
            return Err("model-list pagination did not advance".into());
        }
        cursor = Some(next.to_owned());
    }
    Err("model list exceeds the page limit; enter a model manually".into())
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
