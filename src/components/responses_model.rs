//! Responses API transport; the neutral model-adapter ports stay unchanged.
use crate::components::{model_common, openai_model, responses_wire};
use crate::contracts::component::{ComponentManifest, PortDecl};
use crate::contracts::core_events as ce;
use crate::contracts::event::EventTypeDecl;
use crate::startup::PhaseTimer;
use crate::{Component, Ctx, EventDraft, EventEnvelope};
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const NAME: &str = "responses-model";
pub const CALL_COST: &str = "model.call_cost";

pub fn manifest() -> ComponentManifest {
    let mut manifest = openai_model::manifest();
    manifest.name = NAME.into();
    manifest.entry = format!("builtin:{NAME}");
    // An unwired diagnostic output: recorded, but not part of model dialogue.
    manifest.outputs.push(PortDecl::new("stats", &[CALL_COST]));
    manifest.events.push(EventTypeDecl::new(
        CALL_COST,
        "Completed adapter call timing and process-wide memory observations",
    ));
    manifest
}

pub fn materialize(
    parts: &[Value],
    log: &crate::kernel::log::LogReader,
    docs: Option<&std::path::Path>,
    model: &str,
    base_url: &str,
) -> Result<Vec<Value>, String> {
    materialize_observed(parts, log, docs, model, base_url, None)
}

fn materialize_observed(
    parts: &[Value],
    log: &crate::kernel::log::LogReader,
    docs: Option<&std::path::Path>,
    model: &str,
    base_url: &str,
    mut trace: Option<&mut PhaseTimer>,
) -> Result<Vec<Value>, String> {
    let messages = openai_model::materialize_with_reasoning(parts, log, docs, true)?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.checkpoint("messages_materialized");
    }
    for message in &messages {
        if let Some(native) = message.get("_native_compaction") {
            if native["model"] != model || native["baseUrl"] != base_url {
                return Err("native compaction belongs to a different model or endpoint".into());
            }
        }
    }
    let input = responses_wire::input_items(&messages)?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.checkpoint("dialect_encoded");
    }
    let restored = super::responses_media::restore_input(input, docs);
    if let Some(trace) = trace {
        trace.checkpoint("media_restored");
    }
    restored
}

pub struct ResponsesModel {
    config: Value,
    key: Option<String>,
    client: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

impl ResponsesModel {
    pub fn from_config(config: Option<&Value>) -> Self {
        let config = config.cloned().unwrap_or_else(|| json!({}));
        let key = config["apiKeyEnv"]
            .as_str()
            .and_then(|name| std::env::var(name).ok());
        Self {
            config,
            key,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("HTTP client builds"),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("adapter runtime builds"),
        }
    }

    fn body(
        &self,
        event: &EventEnvelope,
        ctx: &Ctx,
        trace: &mut PhaseTimer,
    ) -> Result<Value, String> {
        let parts = event.payload["input"]["parts"]
            .as_array()
            .ok_or("missing input parts")?;
        model_common::verify_fingerprint(parts, event.payload["input"]["fingerprint"].as_str())?;
        trace.checkpoint("fingerprint_verified");
        let documents = ctx
            .ledger_path()
            .map(crate::contracts::document::documents_dir);
        let input = materialize_observed(
            parts,
            ctx.log(),
            documents.as_deref(),
            self.config["model"].as_str().ok_or("missing model")?,
            self.config["baseUrl"].as_str().ok_or("missing base URL")?,
            Some(trace),
        )?;
        trace.checkpoint("material_temporaries_released");
        let tools = ctx.document(&event.payload["tools"])?;
        let mut body = responses_wire::request(
            self.config["model"].as_str().ok_or("missing model")?,
            input,
            tools.as_array().map(Vec::as_slice).unwrap_or(&[]),
            self.config["maxTokens"].as_u64().unwrap_or(4096),
        );
        if event.payload.get("purpose").is_none() {
            // Routing affinity belongs to the persistent stream, not a call,
            // prompt fingerprint, or adapter lifetime. Hash the identifier so
            // custom stream names are not sent in the clear. Compaction is a
            // separate operation and does not inherit the chat's routing key.
            let mut hash = Sha256::new();
            hash.update(b"lattice:responses:prompt-cache:v1\0");
            hash.update(event.stream.as_bytes());
            body["prompt_cache_key"] = json!(format!("{:x}", hash.finalize()));
        }
        if self.config["nativeWebSearch"].as_bool() == Some(true)
            && event.payload.get("purpose").is_none()
        {
            responses_wire::enable_web_search(&mut body);
        }
        if self.config["nativeImageGeneration"].as_bool() == Some(true)
            && event.payload.get("purpose").is_none()
        {
            body["tools"]
                .as_array_mut()
                .expect("request tools")
                .push(json!({"type":"image_generation","output_format":"png"}));
        }
        let system = ctx.document(&event.payload["system"])?;
        if let Some(system) = system.as_str().or(self.config["system"].as_str()) {
            body["instructions"] = json!(system);
        }
        let fallback = model_common::thinking_from_config(self.config.get("thinking"));
        if let Some(thinking) = model_common::thinking_for_call(&event.payload, fallback.as_ref()) {
            let effort = match thinking {
                model_common::Thinking::Off => "none".to_string(),
                model_common::Thinking::Rung(rung) => {
                    let available: Vec<String> = self.config["effort"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    model_common::place(rung, &available)
                        .unwrap_or(rung.name())
                        .to_string()
                }
                model_common::Thinking::Raw(word) => word,
            };
            body["reasoning"] = json!({"effort": effort, "summary": "auto"});
        }
        Ok(body)
    }
}

/// Trigger compaction consumes retained hosted items even when new searches
/// are disabled in the current profile. Declare only what the input requires;
/// requesting search sources is unnecessary for this provider operation.
fn declare_compaction_web_history(body: &mut Value) {
    let has_web_history = body["input"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["type"] == "web_search_call"));
    if !has_web_history {
        return;
    }
    let tools = body["tools"].as_array_mut().expect("request tools array");
    let mut declared = false;
    tools.retain(|tool| {
        if tool["type"] != "web_search" {
            return true;
        }
        let first = !declared;
        declared = true;
        first
    });
    if !declared {
        tools.push(json!({"type":"web_search"}));
    }
}

fn history_observation(ctx: &Ctx) -> Value {
    let at = chrono::Utc::now().to_rfc3339();
    match ctx.log().memory_stats() {
        Ok(stats) => json!({"at": at, "stats": stats}),
        Err(error) => json!({"at": at, "error": error.to_string()}),
    }
}

fn failure(code: &str, message: &str, blame: &str, retryable: bool) -> Value {
    json!({"status":"error", "error": model_common::error_info(code,message,blame,retryable,retryable)})
}

impl Component for ResponsesModel {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if port != "request" {
            return;
        }
        let mut trace = PhaseTimer::start();
        let history_before = history_observation(ctx);
        let body = self.body(event, ctx, &mut trace);
        trace.checkpoint("body_attempt_finished");
        let input_items = body
            .as_ref()
            .ok()
            .and_then(|body| body["input"].as_array())
            .map(Vec::len);
        let mut request_bytes = None;
        let mut response_bytes = 0_u64;
        let result = match (body, self.key.as_deref(), self.config["baseUrl"].as_str()) {
            (Err(problem), _, _) => failure("material.invalid", &problem, "request", false),
            (_, None, _) => failure(
                "config.missing_api_key",
                "missing API key environment variable",
                "environment",
                false,
            ),
            (_, _, None) => failure(
                "config.missing_base_url",
                "missing Responses base URL",
                "environment",
                false,
            ),
            (Ok(mut body), Some(key), Some(base)) => {
                let native_compact = event.payload["purpose"] == "context.compact.responses";
                let trigger_compact =
                    native_compact && self.config["compactionProtocol"] == "trigger";
                if native_compact {
                    // Native compaction is a provider operation, not a request
                    // to generate a summary with thinking disabled. In
                    // particular, `none` is invalid for reasoning-only models.
                    body.as_object_mut()
                        .expect("request object")
                        .remove("reasoning");
                }
                if trigger_compact {
                    declare_compaction_web_history(&mut body);
                    body["input"]
                        .as_array_mut()
                        .expect("materialized input array")
                        .push(json!({"type":"compaction_trigger"}));
                } else if native_compact {
                    body = responses_wire::compact_request(
                        body["model"].as_str().unwrap_or_default(),
                        body["input"].as_array().cloned().unwrap_or_default(),
                        body["instructions"].as_str(),
                    );
                }
                let endpoint = if native_compact && !trigger_compact {
                    "responses/compact"
                } else {
                    "responses"
                };
                let token = ctx.cancellation();
                let purpose = event.payload.get("purpose").cloned();
                self.runtime.block_on(async {
                    let work = async {
                        let mut request = self.client
                            .post(format!("{}/{endpoint}", base.trim_end_matches('/')))
                            .bearer_auth(key)
                            .json(&body);
                        if trigger_compact {
                            request = request.header("x-codex-beta-features", "remote_compaction_v2");
                        }
                        // Build once, inspect the already-encoded byte buffer; never
                        // serialize again just to measure its length.
                        let request = match request.build() {
                            Ok(request) => request,
                            Err(problem) => return failure("transport.failed", &model_common::full_cause(&problem), "environment", false),
                        };
                        request_bytes = request.body().and_then(|body| body.as_bytes()).map(|bytes| bytes.len());
                        trace.checkpoint("request_encoded");
                        let response = match super::model_http::send(&self.client, request, &token).await {
                            Ok(response) => response,
                            Err(super::model_http::SendError::Cancelled) => return json!({"status":"cancelled"}),
                            Err(problem) => {
                                return failure(
                                    "transport.failed",
                                    &model_common::full_cause(&problem),
                                    "environment",
                                    problem.retryable(),
                                )
                            }
                        };
                        trace.checkpoint("response_headers");
                        if !response.status().is_success() {
                            let status = response.status().as_u16();
                            let text = response.text().await.unwrap_or_default();
                            return failure(
                                "provider.bad_response",
                                &format!("HTTP {status}: {text}"),
                                "provider",
                                status == 429 || status >= 500,
                            );
                        }
                        if native_compact && !trigger_compact {
                            let response: Value = match response.json().await {
                                Ok(response) => response,
                                Err(problem) => {
                                    return failure(
                                        "provider.invalid_compaction",
                                        &problem.to_string(),
                                        "provider",
                                        false,
                                    )
                                }
                            };
                            trace.checkpoint("compact_response_parsed");
                            return match responses_wire::compact_output(&response) {
                                Ok(output) => {
                                    let mut result = json!({"status":"ok", "nativeCompaction":{
                                        "dialect":"responses", "model":self.config["model"],
                                        "baseUrl":self.config["baseUrl"], "output":output}});
                                    if let Some(usage) =
                                        response.get("usage").filter(|v| v.is_object())
                                    {
                                        result["usage"] = usage.clone();
                                    }
                                    result
                                }
                                Err(problem) => failure(
                                    "provider.invalid_compaction",
                                    &problem,
                                    "provider",
                                    false,
                                ),
                            };
                        }
                        let mut parser = model_common::SseParser::default();
                        let mut compact_items = Vec::new();
                        let mut stream = response.bytes_stream();
                        while let Some(bytes) = stream.next().await {
                            let bytes = match bytes {
                                Ok(bytes) => bytes,
                                Err(problem) => {
                                    return failure(
                                        "transport.interrupted",
                                        &model_common::full_cause(&problem),
                                        "environment",
                                        true,
                                    )
                                }
                            };
                            if response_bytes == 0 && !bytes.is_empty() { trace.checkpoint("first_response_bytes"); }
                            response_bytes = response_bytes.saturating_add(bytes.len() as u64);
                            for value in parser.push(&bytes) {
                                match value["type"].as_str() {
                                    Some("response.output_text.delta") => {
                                        let mut note = json!({"chunk":value["delta"]});
                                        if let Some(purpose) = &purpose {
                                            note["purpose"] = purpose.clone();
                                        }
                                        ctx.notify(note);
                                    }
                                    Some("response.output_item.done") if trigger_compact => {
                                        compact_items.push(value["item"].clone());
                                    }
                                    Some("response.completed" | "response.incomplete") => {
                                        trace.checkpoint("terminal_response_parsed");
                                        if trigger_compact {
                                            let response = &value["response"];
                                            if response["status"] != "completed" {
                                                return failure("provider.invalid_compaction", "compaction did not complete", "provider", false);
                                            }
                                            let output = response["output"].as_array()
                                                .filter(|items| !items.is_empty()).unwrap_or(&compact_items);
                                            return match responses_wire::trigger_output(output) {
                                                Ok(output) => {
                                                    let mut result = json!({"status":"ok","nativeCompaction":{
                                                        "dialect":"responses","model":self.config["model"],
                                                        "baseUrl":self.config["baseUrl"],"output":output}});
                                                    result["responsesOutput"] = json!(response["output"].as_array()
                                                        .filter(|items| !items.is_empty()).unwrap_or(&compact_items));
                                                    if response["usage"].is_object() {
                                                        result["usage"] = response["usage"].clone();
                                                    }
                                                    result
                                                }
                                                Err(problem) => failure("provider.invalid_compaction", &problem, "provider", false),
                                            };
                                        }
                                        let mut response = value["response"].clone();
                                        let directory = ctx.ledger_path().map(crate::contracts::document::documents_dir);
                                        if let Err(problem) = super::responses_media::persist(&mut response, directory.as_deref()) {
                                            return failure("provider.invalid_image", &problem, "provider", false);
                                        }
                                        let completed = responses_wire::completed(&response)
                                            .unwrap_or_else(|problem| {
                                                failure(
                                                    "provider.invalid_output",
                                                    &problem,
                                                    "provider",
                                                    false,
                                                )
                                            });
                                        trace.checkpoint("response_normalized");
                                        return completed;
                                    }
                                    Some("response.failed" | "error") => {
                                        return failure(
                                            "provider.failed",
                                            &value.to_string(),
                                            "provider",
                                            false,
                                        )
                                    }
                                    _ => {}
                                }
                            }
                        }
                        failure(
                            "transport.incomplete",
                            "stream ended before a terminal response",
                            "provider",
                            true,
                        )
                    };
                    tokio::select! {
                        _ = token.cancelled() => json!({"status":"cancelled"}),
                        result = work => result,
                    }
                })
            }
        };
        trace.checkpoint("request_and_response_released");
        let history_after = history_observation(ctx);
        let timing = trace.finish("observation_ready");
        ctx.emit(
            "stats",
            EventDraft::new(
                CALL_COST,
                &[&event.id],
                json!({
                    "status": result["status"],
                    "purpose": event.payload.get("purpose"),
                    "timing": timing,
                    "counts": {
                        "materialParts": event.payload["input"]["parts"].as_array().map(Vec::len),
                        "inputItems": input_items, "requestBytes": request_bytes,
                        "sseBytesRead": response_bytes,
                    },
                    "historyBefore": history_before, "historyAfter": history_after,
                }),
            ),
        );
        let mut result = result;
        if let Some(purpose) = event.payload.get("purpose") {
            result["purpose"] = purpose.clone();
        }
        ctx.emit(
            "result",
            EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&event.id], result),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_declaration_preserves_options_and_other_tools_idempotently() {
        let local = json!({"type":"function","name":"web_search","parameters":{"type":"object"}});
        let hosted = json!({"type":"web_search","search_context_size":"low","filters":{"allowed_domains":["example.invalid"]}});
        for declarations in [
            vec![],
            vec![hosted.clone()],
            vec![
                hosted.clone(),
                json!({"type":"web_search","search_context_size":"high"}),
            ],
        ] {
            let mut tools = vec![local.clone()];
            tools.extend(declarations.clone());
            tools.push(json!({"type":"image_generation"}));
            let mut body = json!({
                "input":[{"type":"web_search_call","id":"ws_one"},{"type":"web_search_call","id":"ws_two"}],
                "tools":tools,
                "include":["reasoning.encrypted_content"]
            });
            let input = body["input"].clone();
            declare_compaction_web_history(&mut body);
            let expected = if declarations.is_empty() {
                json!([local, {"type":"image_generation"}, {"type":"web_search"}])
            } else {
                json!([local, hosted, {"type":"image_generation"}])
            };
            assert_eq!(body["tools"], expected);
            assert_eq!(body["input"], input);
            assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
            let once = body.clone();
            declare_compaction_web_history(&mut body);
            assert_eq!(body, once);
        }
    }

    #[test]
    fn compaction_declaration_ignores_text_and_nested_hosted_items() {
        let mut body = json!({
            "input":[
                {"type":"message","role":"user","content":"{\"type\":\"web_search_call\"}"},
                {"type":"function_call_output","call_id":"local_call","output":{"type":"web_search_call"}}
            ],
            "tools":[{"type":"web_search"},{"type":"web_search","search_context_size":"low"}],
            "include":[]
        });
        let before = body.clone();
        declare_compaction_web_history(&mut body);
        assert_eq!(body, before, "no hosted history means no request changes");
    }
}
