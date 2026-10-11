//! Endpoint-specific differences shared by setup and the model adapters.
//! These are wire defaults, not claims about every model served by an endpoint.
use serde_json::Value;

fn host(base: &str) -> Option<String> {
    reqwest::Url::parse(base)
        .ok()?
        .host_str()
        .map(|host| host.strip_suffix('.').unwrap_or(host).to_owned())
}

pub(super) fn qwen_host(host: &str) -> bool {
    matches!(
        host,
        "dashscope.aliyuncs.com" | "dashscope-intl.aliyuncs.com" | "dashscope-us.aliyuncs.com"
    ) || [
        ".cn-beijing.maas.aliyuncs.com",
        ".ap-southeast-1.maas.aliyuncs.com",
        ".us-east-1.maas.aliyuncs.com",
    ]
    .iter()
    .any(|suffix| host.ends_with(suffix))
}

pub fn compatible_messages(base: &str) -> bool {
    let host = host(base).unwrap_or_default();
    qwen_host(&host)
        || matches!(
            host.as_str(),
            "api.minimax.cn"
                | "api.minimax.io"
                | "openrouter.ai"
                | "api.siliconflow.cn"
                | "api.siliconflow.com"
                | "ark.cn-beijing.volces.com"
                | "api.moonshot.cn"
                | "api.moonshot.ai"
        )
}

pub fn messages_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/v1/messages")
}

pub fn messages_auth(
    request: reqwest::RequestBuilder,
    base: &str,
    key: &str,
) -> reqwest::RequestBuilder {
    let request = if matches!(
        host(base).as_deref(),
        Some(
            "api.minimax.cn"
                | "api.minimax.io"
                | "openrouter.ai"
                | "api.siliconflow.cn"
                | "api.siliconflow.com"
                | "api.moonshot.cn"
                | "api.moonshot.ai"
        )
    ) {
        request.bearer_auth(key)
    } else {
        request.header("x-api-key", key)
    };
    request.header("anthropic-version", "2023-06-01")
}

pub fn messages_request(
    base: &str,
    body: &mut Value,
    rungs: &[String],
    raw_effort: bool,
) -> Option<&'static str> {
    if !compatible_messages(base) {
        return None;
    }
    let fields = body.as_object_mut()?;
    let off = fields
        .get("thinking")
        .is_some_and(|t| t["type"] == "disabled");
    let effort = fields.contains_key("output_config");
    let budget_only = matches!(
        host(base).as_deref(),
        Some("api.siliconflow.cn" | "api.siliconflow.com")
    );
    if (effort && budget_only)
        || ((off || effort)
            && !raw_effort
            && (rungs.is_empty() || (off && !rungs.iter().any(|r| r == "none"))))
    {
        fields.remove("thinking");
        fields.remove("output_config");
        return Some("requested thinking control is not available in this endpoint/model configuration; using the service default (thinking may remain enabled)");
    }
    if effort
        && matches!(
            host(base).as_deref(),
            Some(
                "api.minimax.cn" | "api.minimax.io" | "openrouter.ai" | "ark.cn-beijing.volces.com"
            )
        )
    {
        fields.insert("thinking".into(), serde_json::json!({"type":"adaptive"}));
    }
    if effort && qwen_host(&host(base).unwrap_or_default()) {
        fields.insert("thinking".into(), serde_json::json!({"type":"enabled"}));
    }
    None
}

pub fn compatible_responses(base: &str) -> bool {
    let host = host(base).unwrap_or_default();
    qwen_host(&host)
        || matches!(
            host.as_str(),
            "api.deepseek.com"
                | "api.groq.com"
                | "openrouter.ai"
                | "api.x.ai"
                | "us.api.x.ai"
                | "api.minimax.cn"
                | "api.minimax.io"
                | "ark.cn-beijing.volces.com"
                | "api.moonshot.cn"
                | "api.moonshot.ai"
                | "open.bigmodel.cn"
        )
}

/// Compatibility endpoints do not inherit the Codex trigger-compaction protocol.
/// Preserve existing custom gateway behavior; no provider identity is inferred
/// from a substring in an unknown gateway's URL.
pub fn native_compaction(adapter: &str, base: &str) -> bool {
    adapter == "responses" && !compatible_responses(base)
}

/// Remove unsupported optional OpenAI fields, never history or tool results.
/// The caller has already selected an effort from the model's declared rungs.
pub fn responses_request(
    base: &str,
    body: &mut Value,
    rungs: &[String],
    raw_effort: bool,
) -> Option<&'static str> {
    if !compatible_responses(base) {
        return None;
    }
    let fields = body.as_object_mut()?;
    let host = host(base).unwrap_or_default();
    if !matches!(
        host.as_str(),
        "openrouter.ai"
            | "api.minimax.cn"
            | "api.minimax.io"
            | "ark.cn-beijing.volces.com"
            | "api.moonshot.cn"
            | "api.moonshot.ai"
            | "open.bigmodel.cn"
    ) {
        fields.remove("prompt_cache_key");
    }
    if qwen_host(&host)
        || matches!(
            host.as_str(),
            "api.groq.com"
                | "api.deepseek.com"
                | "api.minimax.cn"
                | "api.minimax.io"
                | "open.bigmodel.cn"
        )
    {
        fields.remove("include");
    }
    if matches!(host.as_str(), "api.moonshot.cn" | "api.moonshot.ai") {
        if let Some(include) = fields.get_mut("include").and_then(Value::as_array_mut) {
            include.retain(|field| field != "reasoning.encrypted_content");
            if include.is_empty() {
                fields.remove("include");
            }
        }
    }
    // These request schemas do not declare a storage control. Do not infer one
    // from store:false in their response examples or claim a retention policy.
    if matches!(
        host.as_str(),
        "api.minimax.cn" | "api.minimax.io" | "api.moonshot.cn" | "api.moonshot.ai"
    ) {
        fields.remove("store");
    }
    if let Some(reasoning) = fields.get_mut("reasoning").and_then(Value::as_object_mut) {
        if host != "openrouter.ai" {
            reasoning.remove("summary");
        }
        let off = reasoning.get("effort").and_then(Value::as_str) == Some("none");
        if !raw_effort && (rungs.is_empty() || (off && !rungs.iter().any(|r| r == "none"))) {
            fields.remove("reasoning");
            return Some("requested thinking control is not available in this endpoint/model configuration; using the service default (thinking may remain enabled)");
        }
    }
    None
}

/// BigModel permits one content object where OpenAI uses an array.
/// Keep the original value intact for replay; this is only a reading view.
pub fn response_content_parts(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Array(parts) => Some(parts),
        Value::Object(_) => Some(std::slice::from_ref(value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absolute_dns_names_keep_the_same_wire_policy_without_rewriting_the_address() {
        let client = reqwest::Client::new();
        for base in [
            "https://api.minimax.io/anthropic",
            "https://api.minimax.io./anthropic",
        ] {
            let request = messages_auth(client.post(messages_url(base)), base, "SYNTHETIC_KEY")
                .build()
                .unwrap();
            assert_eq!(request.headers()["authorization"], "Bearer SYNTHETIC_KEY");
            assert!(request.url().as_str().starts_with(base));
        }
        let original = json!({"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":"stable"});
        let mut plain = original.clone();
        let mut absolute = original;
        responses_request("https://api.groq.com/openai/v1", &mut plain, &[], false);
        responses_request("https://api.groq.com./openai/v1", &mut absolute, &[], false);
        assert_eq!(plain, absolute);
        assert!(!native_compaction(
            "responses",
            "https://api.groq.com./openai/v1"
        ));
        assert!(native_compaction(
            "responses",
            "https://api.groq.com.evil.invalid/openai/v1"
        ));
    }

    #[test]
    fn messages_headers_and_paths_match_the_selected_endpoint() {
        let client = reqwest::Client::new();
        for (base, expected, bearer) in [
            (
                "https://api.minimax.io/anthropic/",
                "https://api.minimax.io/anthropic/v1/messages",
                true,
            ),
            (
                "https://openrouter.ai/api",
                "https://openrouter.ai/api/v1/messages",
                true,
            ),
            (
                "https://api.deepseek.com/anthropic",
                "https://api.deepseek.com/anthropic/v1/messages",
                false,
            ),
            (
                "https://api.anthropic.com",
                "https://api.anthropic.com/v1/messages",
                false,
            ),
            (
                "https://openrouter.ai.evil.invalid/api",
                "https://openrouter.ai.evil.invalid/api/v1/messages",
                false,
            ),
        ] {
            let url = messages_url(base);
            assert_eq!(url, expected);
            let request = messages_auth(client.post(&url), base, "SYNTHETIC_KEY")
                .build()
                .unwrap();
            assert_eq!(request.headers().contains_key("authorization"), bearer);
            assert_eq!(request.headers().contains_key("x-api-key"), !bearer);
            assert_eq!(request.headers()["anthropic-version"], "2023-06-01");
            if bearer {
                assert_eq!(request.headers()["authorization"], "Bearer SYNTHETIC_KEY");
            }
        }
    }

    #[test]
    fn compatible_messages_do_not_guess_thinking_controls() {
        for base in [
            "https://api.minimax.io/anthropic",
            "https://openrouter.ai/api",
        ] {
            let mut body = json!({"thinking":{"type":"disabled"}});
            assert!(messages_request(base, &mut body, &[], false).is_some());
            assert!(body.get("thinking").is_none());
            body["output_config"] = json!({"effort":"high"});
            assert!(messages_request(base, &mut body, &[], false).is_some());
            assert!(body.get("output_config").is_none());
            body["output_config"] = json!({"effort":"high"});
            assert!(messages_request(base, &mut body, &["high".into()], false).is_none());
            assert_eq!(body["output_config"]["effort"], "high");
        }
        let mut body = json!({"thinking":{"type":"disabled"}});
        assert!(messages_request("https://api.anthropic.com", &mut body, &[], false).is_none());
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn compatible_responses_do_not_inherit_openai_optional_fields_or_undeclared_effort() {
        let original = json!({"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":"stream","reasoning":{"effort":"high","summary":"auto"},"input":[{"type":"reasoning","content":[{"type":"reasoning_text","text":"original"}]}]});
        for host in [
            "api.groq.com",
            "api.deepseek.com",
            "openrouter.ai",
            "api.x.ai",
            "us.api.x.ai",
        ] {
            let mut body = original.clone();
            assert!(
                responses_request(&format!("https://{host}/v1"), &mut body, &[], false).is_some()
            );
            assert_eq!(
                body.get("prompt_cache_key").is_some(),
                host == "openrouter.ai"
            );
            assert!(body.get("reasoning").is_none());
            assert_eq!(body["store"], false);
            assert_eq!(body["input"], original["input"]);
            assert_eq!(
                body.get("include").is_some(),
                !matches!(host, "api.groq.com" | "api.deepseek.com")
            );
        }
        for base in [
            "https://api.openai.com/v1",
            "https://api.groq.com.evil.invalid/v1",
            "http://localhost:1234",
        ] {
            let mut body = original.clone();
            assert!(responses_request(base, &mut body, &[], false).is_none());
            assert_eq!(body, original);
        }
    }

    #[test]
    fn declared_controls_are_preserved_without_claiming_unsupported_off() {
        let base = "https://api.groq.com/openai/v1";
        let mut body = json!({"reasoning":{"effort":"high","summary":"auto"}});
        assert!(responses_request(base, &mut body, &["high".into()], false).is_none());
        assert_eq!(body["reasoning"], json!({"effort":"high"}));
        body["reasoning"] = json!({"effort":"none"});
        assert!(responses_request(base, &mut body, &["high".into()], false).is_some());
        body["reasoning"] = json!({"effort":"none"});
        assert!(responses_request(base, &mut body, &["none".into()], false).is_none());
        assert_eq!(body["reasoning"]["effort"], "none");
        body["reasoning"] = json!({"effort":"custom"});
        assert!(responses_request(base, &mut body, &[], true).is_none());
        assert_eq!(body["reasoning"]["effort"], "custom");
    }

    #[test]
    fn messages_effort_uses_the_documented_switch_and_never_invents_a_budget() {
        for (base, mode) in [
            ("https://api.minimax.cn/anthropic", Some("adaptive")),
            ("https://api.minimax.io/anthropic", Some("adaptive")),
            (
                "https://ark.cn-beijing.volces.com/api/compatible",
                Some("adaptive"),
            ),
            ("https://openrouter.ai/api", Some("adaptive")),
            (
                "https://dashscope.aliyuncs.com/apps/anthropic",
                Some("enabled"),
            ),
            (
                "https://workspace.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
                Some("enabled"),
            ),
            ("https://api.moonshot.ai/anthropic", None),
        ] {
            let mut body = json!({"output_config":{"effort":"high"}});
            assert!(messages_request(base, &mut body, &["high".into()], false).is_none());
            assert_eq!(body["thinking"]["type"].as_str(), mode, "{base}");
            assert_eq!(body["output_config"]["effort"], "high");
            assert!(body["thinking"].get("budget_tokens").is_none());
        }
        for base in ["https://api.siliconflow.cn", "https://api.siliconflow.com"] {
            let mut body = json!({"output_config":{"effort":"high"}});
            assert!(messages_request(base, &mut body, &["high".into()], true).is_some());
            assert!(body.get("output_config").is_none());
            assert!(body.get("thinking").is_none());
        }
    }

    #[test]
    fn supported_response_options_survive_and_workspace_matching_is_bounded() {
        for (base, summary) in [
            ("https://openrouter.ai/api/v1", true),
            ("https://ark.cn-beijing.volces.com/api/v3", false),
        ] {
            let mut body = json!({"store":false,"prompt_cache_key":"stable","include":["reasoning.encrypted_content"],"reasoning":{"effort":"high","summary":"auto"}});
            assert!(responses_request(base, &mut body, &["high".into()], false).is_none());
            assert_eq!(body["prompt_cache_key"], "stable");
            assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
            assert_eq!(body["reasoning"].get("summary").is_some(), summary);
        }
        let mut body = json!({"include":["reasoning.encrypted_content","web_search_call.action.sources"],"store":false});
        responses_request("https://api.moonshot.ai/v1", &mut body, &[], false);
        assert_eq!(body["include"], json!(["web_search_call.action.sources"]));
        assert!(body.get("store").is_none());
        for region in ["cn-beijing", "ap-southeast-1", "us-east-1"] {
            let base = format!("https://workspace.{region}.maas.aliyuncs.com");
            assert!(compatible_messages(&base));
            assert!(compatible_responses(&base));
            assert!(!native_compaction("responses", &base));
            assert!(!compatible_responses(&format!("{base}.evil.invalid")));
        }
    }

    #[test]
    fn compaction_is_not_implied_by_compatible_responses() {
        for base in [
            "https://api.groq.com/openai/v1",
            "https://api.deepseek.com",
            "https://openrouter.ai/api/v1",
            "https://api.x.ai/v1",
            "https://us.api.x.ai/v1",
        ] {
            assert!(!native_compaction("responses", base));
        }
        assert!(native_compaction("responses", "https://api.openai.com/v1"));
        assert!(native_compaction("responses", "https://gateway.invalid/v1"));
        assert!(!native_compaction("anthropic", "https://api.anthropic.com"));
    }
}
