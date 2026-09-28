//! The first real brain: an Anthropic Messages API adapter.
//!
//! Same port profile as `scripted-model` — swapping a scripted assembly to a
//! real one changes the component name and config in the manifest, no wires.
//! Materialization (pointers → messages), fingerprint verification and SSE
//! accumulation are pure functions with unit tests; the network shell around
//! them is deliberately thin. The API key is read from an environment
//! variable at construction and never appears in any event.

use std::collections::BTreeMap;

use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::components::model_common::{
    error_info, place, thinking_for_call, thinking_from_config, Effort, Fragment, Thinking,
};
pub use crate::components::model_common::{verify_fingerprint, SseParser};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};
use crate::kernel::log::LogReader;

pub const NAME: &str = "anthropic-model";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![
            PortDecl::new("request", &[ce::MODEL_CALL_STARTED]),
            PortDecl::new("control", &[ce::INTERRUPTED]),
        ],
        outputs: vec![PortDecl::new("result", &[ce::MODEL_CALL_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: Some(crate::contracts::component::EffectSurface {
            network: vec!["*".to_string()],
            ..Default::default()
        }),
        // The watchman: a model call may not run longer than this
        implements: vec!["model-adapter".to_string()],
        tools: Vec::new(),
        prompt: Some(crate::components::model_common::INTERRUPTED_FRAGMENT.to_string()),
        handle_timeout_ms: Some(600_000),
        concurrency: None,
    }
}

pub struct AnthropicModel {
    model: String,
    max_tokens: u64,
    thinking: Option<Thinking>,
    /// This model's OWN effort rungs, from its profile. Empty = nothing
    /// declared, and this wire's defaults stand in.
    rungs: Vec<String>,
    /// The last downshift already reported, so a rung that cannot be served
    /// is announced ONCE rather than before every call. Cleared when the
    /// setting changes, so a later downshift speaks again.
    reported_downshift: Option<String>,
    system: Option<String>,
    base_url: String,
    api_key: Option<String>,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl AnthropicModel {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let key_env = get("apiKeyEnv")
            .and_then(Value::as_str)
            .unwrap_or("ANTHROPIC_API_KEY")
            .to_string();
        Self {
            model: get("model")
                .and_then(Value::as_str)
                .unwrap_or("claude-sonnet-5")
                .to_string(),
            max_tokens: get("maxTokens").and_then(Value::as_u64).unwrap_or(4096),
            thinking: thinking_from_config(get("thinking")),
            rungs: get("effort")
                .and_then(Value::as_array)
                .map(|words| {
                    words
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            reported_downshift: None,
            system: get("system").and_then(Value::as_str).map(str::to_string),
            base_url: get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or("https://api.anthropic.com")
                .to_string(),
            api_key: std::env::var(key_env).ok(),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build the adapter's async runtime"),
            // A connect timeout, and nothing else: a model may legitimately
            // take minutes to answer, so an overall timeout would cut off
            // real work. The gap this closes is a connection that never
            // establishes — without it the adapter waited forever, past its
            // own watchman deadline, and the kernel eventually declared the
            // whole adapter unresponsive over one unreachable endpoint.
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("the HTTP client builds with a connect timeout"),
        }
    }
}

impl AnthropicModel {
    /// Say out loud when a rung could not be served as asked. Paying for
    /// `max` and being served less is a difference the person who asked has
    /// to be able to see; the ledger cannot show it, because what goes on the
    /// ledger is the neutral rung and the translated budget lives only in the
    /// request body.
    fn report_downshift(&mut self, thinking: Option<&Thinking>, ctx: &Ctx) {
        let note = match thinking {
            Some(Thinking::Rung(rung)) => downshift(*rung, &self.rungs),
            _ => None,
        };
        if note != self.reported_downshift {
            if let Some(text) = &note {
                ctx.notify(json!({"note": text}));
            }
            self.reported_downshift = note;
        }
    }
}

impl Component for AnthropicModel {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if port != "request" {
            return; // "control" interrupts act through the cancellation token
        }
        let done =
            |payload: Value| EventDraft::new(ce::MODEL_CALL_COMPLETED, &[&event.id], payload);

        let input = &event.payload["input"];
        let parts = input["parts"].as_array().cloned().unwrap_or_default();
        if let Err(problem) = verify_fingerprint(&parts, input["fingerprint"].as_str()) {
            let error = error_info(
                "material.fingerprint_mismatch",
                &problem,
                "request",
                false,
                false,
            );
            ctx.emit("result", done(json!({"status": "error", "error": error})));
            return;
        }
        let messages = match materialize(
            &parts,
            ctx.log(),
            ctx.ledger_path()
                .map(crate::contracts::document::documents_dir)
                .as_deref(),
        ) {
            Ok(messages) => messages,
            Err(problem) => {
                let error = error_info("material.invalid", &problem, "request", false, false);
                ctx.emit("result", done(json!({"status": "error", "error": error})));
                return;
            }
        };
        let Some(api_key) = self.api_key.clone() else {
            let error = error_info(
                "config.missing_api_key",
                "missing API key environment variable",
                "environment",
                false,
                false,
            );
            ctx.emit("result", done(json!({"status": "error", "error": error})));
            return;
        };

        // Both of these follow the same rule: what the CALL carries wins, the
        // adapter's config is the fallback for a gateless assembly. Reported
        // before `system` borrows self, so this `&mut` does not collide with
        // the `&str` held across the call below.
        let thinking = thinking_for_call(&event.payload, self.thinking.as_ref());
        self.report_downshift(thinking.as_ref(), ctx);
        // The gate's assembled prompt and tool list are documents: past a
        // size they live in a file beside the ledger, so what arrives here is
        // a reference and has to be followed. Small ones arrive inline and
        // pass straight through.
        let carried = match (
            ctx.document(&event.payload["system"]),
            ctx.document(&event.payload["tools"]),
        ) {
            (Ok(system), Ok(tools)) => (system, tools),
            (Err(problem), _) | (_, Err(problem)) => {
                let error = error_info("material.unreadable", &problem, "request", false, false);
                ctx.emit("result", done(json!({"status": "error", "error": error})));
                return;
            }
        };
        let system = carried.0.as_str().or(self.system.as_deref());
        let body = build_request(
            &self.model,
            self.max_tokens,
            thinking.as_ref(),
            &self.rungs,
            system,
            messages,
            carried.1.as_array(),
        );
        let url = format!("{}/v1/messages", self.base_url);
        let token = ctx.cancellation();
        let client = self.client.clone();
        // A background call (e.g. the condenser) carries a `purpose`; tag its
        // live stream with it so a frontend can drop background streams
        // generically, without knowing this instance's name. The COMPLETION
        // carries it too, so that an observer holding one event — with no
        // ledger to trace its cause back through — can tell a background call
        // from the conversation.
        let purpose = event.payload.get("purpose").cloned();
        let completed_purpose = purpose.clone();

        // The async shell: select between the stream and the cancellation
        // token — Go-context style, zero polling. `block_on` runs on this
        // component's own thread; only this component waits.
        let payload = self.runtime.block_on(async {
            let response = match send_with_retries(&client, &url, &api_key, &body, &token).await {
                Ok(response) => response,
                Err(payload) => return payload,
            };
            let mut accumulator = Accumulator::default();
            let mut parser = SseParser::default();
            let mut stream = response.bytes_stream();
            loop {
                tokio::select! {
                    _ = token.cancelled() => return accumulator.finish_cancelled(),
                    item = stream.next() => match item {
                        None => break,
                        // Mid-stream transport failure: content already flowed,
                        // never retry — record what we have as an error
                        Some(Err(err)) => {
                            return accumulator.finish_error(&format!(
                                "the reply was cut off mid-stream: {}",
                                crate::components::model_common::full_cause(&err)
                            ))
                        }
                        Some(Ok(bytes)) => {
                            for data in parser.push(&bytes) {
                                if let Some(fragment) = accumulator.apply(&data) {
                                    let mut note = fragment.note();
                                    if let Some(p) = &purpose {
                                        note["purpose"] = p.clone();
                                    }
                                    ctx.notify(note);
                                }
                            }
                        }
                    }
                }
            }
            accumulator.finish()
        });
        let mut payload = payload;
        if let Some(purpose) = completed_purpose {
            payload["purpose"] = purpose;
        }
        ctx.emit("result", done(payload));
    }
}

/// Retry only while sending; response-body failures are handled by the caller.
async fn send_with_retries(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &Value,
    token: &tokio_util::sync::CancellationToken,
) -> Result<reqwest::Response, Value> {
    let request = client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(body);
    super::model_http::send_chat(client, request, url, token).await
}

/// This wire's own default rungs, for a model with no profile.
///
/// Anthropic documents five, and `xhigh` is the newest — some models that
/// have `max` do not have it. A model that differs says so in its profile.
const DEFAULT_RUNGS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// What this request will actually carry for `want`.
fn wire_word(want: Effort, rungs: &[String]) -> String {
    place(want, rungs).map(str::to_string).unwrap_or_else(|| {
        let fallback: Vec<String> = DEFAULT_RUNGS.iter().map(|s| s.to_string()).collect();
        place(want, &fallback)
            .unwrap_or(Effort::High.name())
            .to_string()
    })
}

/// What to tell the user when a rung could not be served as asked.
pub fn downshift(want: Effort, rungs: &[String]) -> Option<String> {
    let sent = wire_word(want, rungs);
    (sent != want.name()).then(|| {
        format!(
            "this model has no `{}` — sent `{sent}`, the nearest it offers",
            want.name()
        )
    })
}

fn build_request(
    model: &str,
    max_tokens: u64,
    thinking: Option<&Thinking>,
    rungs: &[String],
    system: Option<&str>,
    messages: Vec<Value>,
    tools: Option<&Vec<Value>>,
) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": true,
    });
    // The strength rides in `output_config.effort` on this wire. An earlier
    // version of this adapter sent `thinking.budget_tokens` instead, on the
    // belief that effort was the unsupported form — the reverse of the truth.
    // Anthropic's documentation calls effort "the recommended way to control
    // thinking depth"; budget_tokens is the older extended-thinking control,
    // paired with effort on only one model. Effort also has five rungs where
    // the budget mapping had four hand-written numbers.
    match thinking {
        Some(Thinking::Off) => body["thinking"] = json!({"type": "disabled"}),
        Some(Thinking::Rung(rung)) => {
            body["output_config"] = json!({"effort": wire_word(*rung, rungs)});
        }
        // A value this runtime does not interpret rides through untouched;
        // an assembly writing one has taken responsibility for it.
        Some(Thinking::Raw(word)) => {
            body["output_config"] = json!({"effort": word});
        }
        None => {}
    }
    if let Some(system) = system {
        // Stable prefix, marked cacheable — the simplest prompt-cache win
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": {"type": "ephemeral"},
        }]);
    }
    if let Some(tools) = tools.filter(|t| !t.is_empty()) {
        body["tools"] = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool["name"],
                    "description": tool["description"],
                    "input_schema": tool["parameters"],
                })
            })
            .collect();
    }
    body
}

/// The reasoning parts of a completed call, back in Anthropic block shape.
///
/// A readable part keeps its signature WHEN IT HAS ONE, and goes back without
/// one when it does not. Dropping the unsigned ones was the earlier reading —
/// "a ledger written by one brain, replayed through another" — and it is wrong,
/// because leaving them out does not produce a turn with less thinking in it,
/// it produces a turn this format REJECTS: an assistant turn that called a tool
/// must lead with a thinking block, and the endpoint says so regardless of
/// whether the request asked for thinking at all. Measured against
/// api.deepseek.com/anthropic (2026-07-31): no block → 400; an unsigned block →
/// accepted; an empty unsigned block → accepted.
///
/// Against Anthropic's own endpoint an unsigned block is presumably refused —
/// but so is no block, so this cannot cost a conversation that had one. What it
/// buys is every conversation that crosses dialects: a thought recorded through
/// the OpenAI wire has no signature to carry and never will.
fn thinking_blocks(payload: &Value) -> Vec<Value> {
    let Some(parts) = payload["reasoning"].as_array() else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| match part["kind"].as_str()? {
            ce::REASONING_TEXT => {
                let mut block = json!({
                    "type": "thinking",
                    "thinking": part["text"].as_str().unwrap_or_default(),
                });
                // Present only when this dialect produced the part. Absent is a
                // different thing from empty, so the key is left off entirely.
                if let Some(signature) = part["opaque"]["signature"].as_str() {
                    block["signature"] = json!(signature);
                }
                Some(block)
            }
            ce::REASONING_HIDDEN => Some(json!({
                "type": "redacted_thinking",
                "data": part["opaque"]["data"].as_str()?,
            })),
            _ => None,
        })
        .collect()
}

/// Pointers → Anthropic messages. Consecutive tool results merge into one
/// user message, as the API requires for parallel tool calls.
/// One material part that ANSWERS a call, appended as this wire's tool result
/// — merging into a preceding tool-result user message, which is what this
/// format wants when several calls ran in parallel. Shared so a result reads
/// the same whether it was reached in order or pulled up behind its call.
fn push_tool_result(
    messages: &mut Vec<Value>,
    part: &Value,
    log: &LogReader,
    docs: Option<&std::path::Path>,
) -> Result<(), String> {
    let (id, note) = match (part["event"].as_str(), part["digest"]["of"].as_str()) {
        (Some(id), _) => (id, None),
        (None, Some(of)) => (of, part["digest"]["text"].as_str()),
        _ => return Err(format!("malformed material part: {part}")),
    };
    let Some(event) = log.get(id).map_err(|e| e.to_string())? else {
        return Err(format!("material points at an unknown event: {id}"));
    };
    // A settled chain: the call it closed is named by the event it caused
    if event.event_type == ce::INTERRUPTED {
        let call = crate::components::model_common::answered_call(part, log)?.unwrap_or_default();
        let block = json!({
            "type": "tool_result",
            "tool_use_id": call,
            "content": crate::components::model_common::interrupted_text(&event.payload),
            "is_error": true,
        });
        if let Some(last) = messages.last_mut() {
            if last["role"] == "user" && last["content"][0]["type"] == "tool_result" {
                last["content"].as_array_mut().unwrap().push(block);
                return Ok(());
            }
        }
        messages.push(json!({"role": "user", "content": [block]}));
        return Ok(());
    }
    let Some(call) = event.payload["call"].as_str() else {
        return Err(format!("tool result {id} has no call id"));
    };
    let is_error = note.is_none() && event.payload["status"] == "error";
    let content = match note {
        Some(text) => text.to_string(),
        None => crate::components::model_common::completion_text_at(&event.payload, &event.id),
    };
    let images = if note.is_none() && !is_error {
        super::media_document::tool_pngs(&event.payload["result"], docs)?
    } else {
        Vec::new()
    };
    let content = if images.is_empty() {
        json!(content)
    } else {
        let mut blocks = vec![json!({"type":"text","text":content})];
        blocks.extend(images.into_iter().map(|data| json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":data}})));
        json!(blocks)
    };
    let block = json!({
        "type": "tool_result",
        "tool_use_id": call,
        "content": content,
        "is_error": is_error,
    });
    if let Some(last) = messages.last_mut() {
        if last["role"] == "user" && last["content"][0]["type"] == "tool_result" {
            last["content"].as_array_mut().unwrap().push(block);
            return Ok(());
        }
    }
    messages.push(json!({"role": "user", "content": [block]}));
    Ok(())
}

/// `docs` is the directory beside the ledger where attachments live — `None`
/// when this stream has no ledger, in which case no attachment is reachable and
/// the text goes out alone.
pub fn materialize(
    parts: &[Value],
    log: &LogReader,
    docs: Option<&std::path::Path>,
) -> Result<Vec<Value>, String> {
    // Where each call's answer sits, so an assistant turn is followed
    // IMMEDIATELY by its results — this wire requires it, and the ledger's
    // order is causal, not conversational. A call with no answer at all is
    // dropped rather than replayed.
    let answers = crate::components::model_common::answer_index(parts, log)?;
    let mut placed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut messages: Vec<Value> = Vec::new();
    for (at, part) in parts.iter().enumerate() {
        if placed.contains(&at) {
            continue; // already emitted, right behind the call it answers
        }
        if let Some(inline) = part.get("inline") {
            messages.push(inline.clone());
            continue;
        }
        // A digest part: present the note instead of the original (contract
        // 02, third part kind). The note carries the recall id in its text.
        if let Some(digest) = part.get("digest") {
            if digest["of"]
                .as_str()
                .map(|id| log.get(id))
                .transpose()
                .map_err(|e| e.to_string())?
                .flatten()
                .is_some_and(|event| {
                    event
                        .payload
                        .get("nativeCompaction")
                        .is_some_and(|v| !v.is_null())
                })
            {
                return Err("native Responses compaction requires its originating dialect".into());
            }
            let text = digest["text"].as_str().unwrap_or_default();
            messages.push(json!({
                "role": "user",
                "content": [{"type": "text", "text": format!("[digested] {text}")}],
            }));
            continue;
        }
        let Some(id) = part["event"].as_str() else {
            return Err(format!("malformed material part: {part}"));
        };
        let Some(event) = log.get(id).map_err(|e| e.to_string())? else {
            return Err(format!("material points at an unknown event: {id}"));
        };
        match event.event_type.as_str() {
            ce::USER_MESSAGE => {
                // Pictures FIRST, then the sentence. Anthropic's own guidance:
                // a question that arrives after what it is about reads better
                // than one asked before it.
                let mut content: Vec<Value> =
                    crate::components::model_common::attached_images(&event.payload, docs)
                        .into_iter()
                        .map(|(media, data)| {
                            json!({"type": "image", "source": {
                                "type": "base64", "media_type": media, "data": data}})
                        })
                        .collect();
                content.push(json!({"type": "text", "text": event.payload["text"]}));
                messages.push(json!({"role": "user", "content": content}));
            }
            ce::WAKE => messages.push(json!({
                "role": "user",
                "content": [{"type": "text",
                    "text": crate::components::model_common::wake_text(&event.payload)}],
            })),
            ce::MODEL_CALL_COMPLETED => {
                let mut content = Vec::new();
                // Dialect law: a turn that called a tool must carry its
                // thinking back, and the thinking blocks must LEAD the turn.
                // The demand is unconditional — the endpoint makes it whether
                // or not the request asked for thinking — so a turn with
                // nothing to say still leads with an empty block rather than
                // with its tool call.
                let calls: Vec<&Value> = event.payload["toolCalls"]
                    .as_array()
                    .map(|calls| {
                        calls
                            .iter()
                            .filter(|call| {
                                call["id"]
                                    .as_str()
                                    .is_some_and(|id| answers.contains_key(id))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !calls.is_empty() {
                    let mut thinking = thinking_blocks(&event.payload);
                    if thinking.is_empty() {
                        // Nothing readable survived, or the turn never thought.
                        // Either way the block itself is what is required.
                        thinking.push(json!({"type": "thinking", "thinking": ""}));
                    }
                    content.extend(thinking);
                }
                if let Some(text) = event.payload["text"].as_str() {
                    if !text.is_empty() {
                        content.push(json!({"type": "text", "text": text}));
                    }
                }
                let ids: Vec<String> = calls
                    .iter()
                    .filter_map(|c| c["id"].as_str().map(str::to_string))
                    .collect();
                for call in calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": call["id"],
                        "name": call["tool"],
                        "input": call["arguments"],
                    }));
                }
                if !content.is_empty() {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
                // The results, right here. They merge into one user message
                // below, which is what this wire wants for parallel calls.
                for id in ids {
                    if let Some(&answer_at) = answers.get(&id) {
                        push_tool_result(&mut messages, &parts[answer_at], log, docs)?;
                        placed.insert(answer_at);
                    }
                }
            }
            // One call, one answer. A call the kernel settled AND the tool
            // reported on has two; the other is a duplicate, and this format
            // refuses a second tool_result for one id just as firmly as the
            // other one does.
            ce::INTERRUPTED | ce::TOOL_EXEC_COMPLETED
                if crate::components::model_common::is_chosen_answer(&answers, parts, at, log)? =>
            {
                push_tool_result(&mut messages, part, log, docs)?
            }
            // Started events and control events carry no message content
            _ => {}
        }
    }
    if messages.is_empty() {
        return Err("materialized to an empty conversation".to_string());
    }
    Ok(messages)
}

enum Block {
    Text,
    Tool {
        id: String,
        name: String,
        args: String,
    },
    /// Thinking, and the signature the provider will demand back with it.
    /// They live in one block because they belong to one another.
    Thinking {
        text: String,
        signature: String,
    },
    /// Thinking the provider sealed: unreadable here, still carried back.
    Redacted {
        data: String,
    },
}

/// Folds streaming events into one completed-state payload.
/// `apply` returns the text chunk to surface live, if any.
#[derive(Default)]
pub struct Accumulator {
    text: String,
    blocks: BTreeMap<u64, Block>,
    usage: Map<String, Value>,
    stop_reason: Option<String>,
    error: Option<String>,
}

impl Accumulator {
    pub fn apply(&mut self, data: &Value) -> Option<Fragment> {
        match data["type"].as_str()? {
            "message_start" => {
                if let Some(usage) = data["message"]["usage"].as_object() {
                    self.usage.extend(usage.clone());
                }
                None
            }
            "content_block_start" => {
                let index = data["index"].as_u64()?;
                let block = &data["content_block"];
                match block["type"].as_str().unwrap_or_default() {
                    "tool_use" => {
                        self.blocks.insert(
                            index,
                            Block::Tool {
                                id: block["id"].as_str().unwrap_or_default().to_string(),
                                name: block["name"].as_str().unwrap_or_default().to_string(),
                                args: String::new(),
                            },
                        );
                    }
                    "thinking" => {
                        self.blocks.insert(
                            index,
                            Block::Thinking {
                                text: block["thinking"].as_str().unwrap_or_default().to_string(),
                                signature: block["signature"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                            },
                        );
                    }
                    // Sealed thinking arrives whole, with no deltas to follow
                    "redacted_thinking" => {
                        self.blocks.insert(
                            index,
                            Block::Redacted {
                                data: block["data"].as_str().unwrap_or_default().to_string(),
                            },
                        );
                    }
                    _ => {
                        self.blocks.insert(index, Block::Text);
                    }
                }
                None
            }
            "content_block_delta" => {
                let index = data["index"].as_u64()?;
                let delta = &data["delta"];
                match delta["type"].as_str()? {
                    "text_delta" => {
                        let chunk = delta["text"].as_str()?.to_string();
                        self.text.push_str(&chunk);
                        Some(Fragment::Text(chunk))
                    }
                    "thinking_delta" => {
                        let chunk = delta["thinking"].as_str()?.to_string();
                        if let Some(Block::Thinking { text, .. }) = self.blocks.get_mut(&index) {
                            text.push_str(&chunk);
                        }
                        Some(Fragment::Reasoning(chunk))
                    }
                    // The signature arrives after the thought it signs
                    "signature_delta" => {
                        if let Some(Block::Thinking { signature, .. }) = self.blocks.get_mut(&index)
                        {
                            signature.push_str(delta["signature"].as_str().unwrap_or_default());
                        }
                        None
                    }
                    "input_json_delta" => {
                        if let Some(Block::Tool { args, .. }) = self.blocks.get_mut(&index) {
                            args.push_str(delta["partial_json"].as_str().unwrap_or_default());
                        }
                        None
                    }
                    _ => None,
                }
            }
            "message_delta" => {
                if let Some(usage) = data["usage"].as_object() {
                    self.usage.extend(usage.clone());
                }
                if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
                None
            }
            "error" => {
                self.error = Some(data["error"].to_string());
                None
            }
            _ => None,
        }
    }

    fn tool_calls(&self) -> Vec<Value> {
        self.blocks
            .values()
            .filter_map(|block| match block {
                Block::Tool { id, name, args } => {
                    let arguments: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
                    Some(json!({"id": id, "tool": name, "arguments": arguments}))
                }
                _ => None,
            })
            .collect()
    }

    /// The reasoning parts of this call, in contract shape and in block order.
    /// The signature stays inside the part that carries the thought it signs,
    /// so no later step can pair it with a different one. An endpoint speaking
    /// this format that sends no `signature_delta` leaves the part unsigned
    /// rather than signed with nothing: replaying an empty signature is a
    /// claim, and the two are not the same thing to whoever checks it.
    fn reasoning_parts(&self) -> Vec<Value> {
        self.blocks
            .values()
            .filter_map(|block| match block {
                Block::Thinking { text, signature } => Some(ce::reasoning_text_part(
                    text,
                    (!signature.is_empty()).then(|| json!({"signature": signature})),
                )),
                Block::Redacted { data } => Some(ce::reasoning_hidden_part(json!({"data": data}))),
                _ => None,
            })
            .collect()
    }

    /// Whatever the model had thought by this point rides along on EVERY
    /// completed state, including the ones that ended badly.
    fn with_reasoning(&self, mut payload: Value) -> Value {
        let parts = self.reasoning_parts();
        if !parts.is_empty() {
            payload["reasoning"] = json!(parts);
        }
        payload
    }

    pub fn finish(&self) -> Value {
        if let Some(error) = &self.error {
            return self.with_reasoning(json!({
                "status": "error",
                "error": error_info("provider.api_error", error, "provider", false, true),
                "usage": self.usage,
            }));
        }
        // Defense borrowed from pi: a response truncated by max_tokens can
        // carry tool-call JSON that parses but is silently incomplete —
        // executing it would act on half an intention. Fail the whole call.
        if self.stop_reason.as_deref() == Some("max_tokens") && !self.tool_calls().is_empty() {
            return self.with_reasoning(json!({
                "status": "error",
                "error": error_info(
                    "provider.truncated_tool_call",
                    "response hit max_tokens mid tool call; arguments may be silently incomplete",
                    "request",
                    false,
                    false,
                ),
                "text": self.text,
                "stopReason": "max_tokens",
                "usage": self.usage,
            }));
        }
        let mut payload = json!({"status": "ok", "usage": self.usage});
        if let Some(reason) = &self.stop_reason {
            // Truncation must be visible: max_tokens shows up here
            payload["stopReason"] = json!(reason);
        }
        if !self.text.is_empty() {
            payload["text"] = json!(self.text);
        }
        let calls = self.tool_calls();
        if !calls.is_empty() {
            payload["toolCalls"] = json!(calls);
        }
        self.with_reasoning(payload)
    }

    pub fn finish_cancelled(&self) -> Value {
        self.with_reasoning(json!({
            "status": "cancelled", "text": self.text, "usage": self.usage,
        }))
    }

    pub fn finish_error(&self, error: &str) -> Value {
        self.with_reasoning(json!({
            "status": "error",
            "error": error_info("provider.stream_interrupted", error, "provider", false, true),
            "text": self.text,
            "usage": self.usage,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::core_events::core_event_decls;
    use crate::kernel::log::EventLog;
    use sha2::{Digest, Sha256};

    fn fingerprint_of(parts: &[Value]) -> String {
        let mut hasher = Sha256::new();
        for part in parts {
            if let Some(event) = part["event"].as_str() {
                hasher.update(event.as_bytes());
            } else {
                hasher.update(part["inline"].to_string().as_bytes());
            }
            hasher.update(b"\n");
        }
        format!("sha256:{:x}", hasher.finalize())
    }

    /// The sibling of the OpenAI adapter's test, and the same real 400: a
    /// call the kernel settled at its deadline, which the tool ALSO reported
    /// on. Two tool_result blocks for one tool_use_id is an orphan here too.
    #[test]
    fn a_call_answered_twice_still_renders_exactly_one_tool_result() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let asked = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "toolCalls": [
                        {"id": "slow", "tool": "Run", "arguments": {}}
                    ]}),
                ),
                "model",
            )
            .unwrap();
        let started = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[&asked.id],
                    json!({"call": "slow", "tool": "Run", "arguments": {}}),
                ),
                "loop",
            )
            .unwrap();
        let settled = log
            .append(
                EventDraft::new(ce::INTERRUPTED, &[&started.id], json!({"by": "deadline"})),
                "core",
            )
            .unwrap();
        let reported = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&started.id],
                    json!({"call": "slow", "status": "cancelled", "result": "half of it"}),
                ),
                "shell",
            )
            .unwrap();

        let messages = materialize(
            &[
                json!({"event": asked.id}),
                json!({"event": settled.id}),
                json!({"event": reported.id}),
            ],
            &log.reader(),
            None,
        )
        .unwrap();
        let blocks: Vec<&Value> = messages
            .iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter(|b| b["tool_use_id"] == "slow")
            .collect();
        assert_eq!(blocks.len(), 1, "one call, one answer: {messages:#?}");
        let said = blocks[0]["content"].as_str().unwrap();
        assert!(
            said.contains("cancelled") && said.contains("half of it"),
            "the tool's own word, cut-short marked and partial output kept: {said}"
        );
    }

    #[test]
    fn fingerprint_gate_refuses_swapped_material() {
        let parts = vec![json!({"event": "ev_1"})];
        assert!(verify_fingerprint(&parts, Some(&fingerprint_of(&parts))).is_ok());
        assert!(verify_fingerprint(&parts, Some("sha256:doctored")).is_err());
        assert!(verify_fingerprint(&parts, None).is_err());
    }

    /// A tool-call turn replays its thinking LEADING the turn, signature
    /// attached where there is one. A part recorded through another dialect has
    /// none and goes back without it: this format demands a thinking block on a
    /// turn that called a tool, so leaving the part out would not make the turn
    /// quieter, it would make it unsendable.
    #[test]
    fn thinking_leads_the_replayed_turn_signed_or_not() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let signed = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "text": "let me add", "reasoning": [
                        {"kind": "text", "text": "a sum", "opaque": {"signature": "sig_abc"}},
                        {"kind": "hidden", "opaque": {"data": "sealed_xyz"}},
                    ], "toolCalls": [{"id": "toolu_1", "tool": "calc", "arguments": {}}]}),
                ),
                "model",
            )
            .unwrap();
        let foreign = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "reasoning": [{"kind": "text", "text": "no signature"}],
                           "toolCalls": [{"id": "toolu_2", "tool": "calc", "arguments": {}}]}),
                ),
                "model",
            )
            .unwrap();

        // Both calls must be ANSWERED, or neither is replayable at all
        let answer = |log: &mut EventLog, cause: &str, call: &str| {
            log.append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[cause],
                    json!({"call": call, "status": "ok", "result": 11.0}),
                ),
                "tools",
            )
            .unwrap()
        };
        let a1 = answer(&mut log, &signed.id, "toolu_1");
        let a2 = answer(&mut log, &foreign.id, "toolu_2");
        let messages = materialize(
            &[
                json!({"event": signed.id}),
                json!({"event": a1.id}),
                json!({"event": foreign.id}),
                json!({"event": a2.id}),
            ],
            &log.reader(),
            None,
        )
        .unwrap();
        assert_eq!(
            messages[0]["content"][0],
            json!({"type": "thinking", "thinking": "a sum", "signature": "sig_abc"})
        );
        assert_eq!(
            messages[0]["content"][1],
            json!({"type": "redacted_thinking", "data": "sealed_xyz"})
        );
        assert_eq!(messages[0]["content"][2]["type"], "text");
        assert_eq!(messages[0]["content"][3]["type"], "tool_use");
        // Recorded through another dialect: the thought still leads the turn,
        // carrying no signature because there never was one to carry
        assert_eq!(
            messages[2]["content"][0],
            json!({"type": "thinking", "thinking": "no signature"})
        );
        assert_eq!(messages[2]["content"][1]["type"], "tool_use");
    }

    /// A tool-call turn with no thinking recorded at all — the model in that
    /// seat did not think, or thinking was off at the time — still leads with
    /// the block. The endpoint asks for the block, not for its contents, and it
    /// asks whether or not this request enabled thinking.
    #[test]
    fn a_tool_call_turn_that_never_thought_still_leads_with_the_block() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        for reasoning in [json!(null), json!([])] {
            let mut payload = json!({"status": "ok",
                "toolCalls": [{"id": "toolu_1", "tool": "calc", "arguments": {}}]});
            if !reasoning.is_null() {
                payload["reasoning"] = reasoning;
            }
            let called = log
                .append(
                    EventDraft::new(ce::MODEL_CALL_COMPLETED, &[], payload),
                    "model",
                )
                .unwrap();
            let answered = log
                .append(
                    EventDraft::new(
                        ce::TOOL_EXEC_COMPLETED,
                        &[&called.id],
                        json!({"call": "toolu_1", "status": "ok", "result": 11.0}),
                    ),
                    "tools",
                )
                .unwrap();
            let messages = materialize(
                &[json!({"event": called.id}), json!({"event": answered.id})],
                &log.reader(),
                None,
            )
            .unwrap();
            assert_eq!(
                messages[0]["content"][0],
                json!({"type": "thinking", "thinking": ""}),
                "a turn that called a tool may not start at the tool call"
            );
            assert_eq!(messages[0]["content"][1]["type"], "tool_use");
        }
    }

    #[test]
    fn materializes_a_tool_round_trip_from_the_log() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let user = log
            .append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "add 4 and 7"})),
                "ui",
            )
            .unwrap();
        let reply = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&user.id],
                    json!({"status": "ok", "toolCalls": [
                        {"id": "toolu_1", "tool": "calc", "arguments": {"numbers": [4, 7]}}
                    ]}),
                ),
                "model",
            )
            .unwrap();
        let outcome = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&reply.id],
                    json!({"call": "toolu_1", "status": "ok", "result": 11.0}),
                ),
                "tools",
            )
            .unwrap();

        let parts = vec![
            json!({"event": user.id}),
            json!({"event": reply.id}),
            json!({"event": outcome.id}),
        ];
        let messages = materialize(&parts, &log.reader(), None).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        // A tool-call turn always opens with the thinking block this format
        // demands — empty here, since this turn recorded no thinking
        assert_eq!(messages[1]["content"][0]["type"], "thinking");
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        assert_eq!(messages[1]["content"][1]["id"], "toolu_1");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn parallel_tool_results_merge_into_one_user_message() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let a = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[],
                    json!({"call": "toolu_a", "status": "ok", "result": 3.0}),
                ),
                "tools",
            )
            .unwrap();
        let b = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[],
                    json!({"call": "toolu_b", "status": "error", "error": {"code": "tool.failed", "message": "nope", "blame": "request"}}),
                ),
                "tools",
            )
            .unwrap();

        let parts = vec![json!({"event": a.id}), json!({"event": b.id})];
        let messages = materialize(&parts, &log.reader(), None).unwrap();
        assert_eq!(messages.len(), 1, "tool results merge into one user turn");
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[0]["content"][1]["is_error"], true);
    }

    #[test]
    fn accumulator_folds_a_streamed_reply() {
        let mut parser = SseParser::default();
        let mut accumulator = Accumulator::default();
        let mut chunks = Vec::new();
        let fixture = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":4}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"they want\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" a sum\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_abc\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"sealed_xyz\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"4 + 7\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\" = 11\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"calc\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"numbers\\\":\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"[1,2]}\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":25}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        // Feed in awkward slices to prove incremental parsing
        for piece in fixture.as_bytes().chunks(17) {
            for data in parser.push(piece) {
                if let Some(chunk) = accumulator.apply(&data) {
                    chunks.push(chunk);
                }
            }
        }

        assert_eq!(
            chunks,
            vec![
                Fragment::Reasoning("they want".to_string()),
                Fragment::Reasoning(" a sum".to_string()),
                Fragment::Text("4 + 7".to_string()),
                Fragment::Text(" = 11".to_string()),
            ]
        );
        let payload = accumulator.finish();
        assert_eq!(payload["status"], "ok");
        // The reply is the reply — thinking never leaks into it
        assert_eq!(payload["text"], "4 + 7 = 11");
        // Signature stays inside the part carrying the thought it signs; the
        // sealed block is carried without being readable
        assert_eq!(
            payload["reasoning"],
            json!([
                {"kind": "text", "text": "they want a sum", "opaque": {"signature": "sig_abc"}},
                {"kind": "hidden", "opaque": {"data": "sealed_xyz"}},
            ])
        );
        assert_eq!(payload["toolCalls"][0]["id"], "toolu_9");
        assert_eq!(payload["toolCalls"][0]["tool"], "calc");
        assert_eq!(
            payload["toolCalls"][0]["arguments"]["numbers"],
            json!([1, 2])
        );
        assert_eq!(payload["usage"]["input_tokens"], 10);
        assert_eq!(payload["usage"]["cache_read_input_tokens"], 4);
        assert_eq!(payload["usage"]["output_tokens"], 25);
        assert_eq!(payload["stopReason"], "tool_use");
    }

    #[test]
    fn truncated_tool_calls_are_failed_not_executed() {
        let mut accumulator = Accumulator::default();
        accumulator.apply(&serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "calc", "input": {}},
        }));
        accumulator.apply(&serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": "{\"numbers\": [1, 2]}"},
        }));
        accumulator.apply(&serde_json::json!({
            "type": "message_delta", "delta": {"stop_reason": "max_tokens"}, "usage": {},
        }));

        let payload = accumulator.finish();
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "provider.truncated_tool_call");
        assert!(
            payload.get("toolCalls").is_none(),
            "half an intention must not execute"
        );
    }

    #[test]
    fn multibyte_characters_survive_chunk_splits() {
        // Chinese text whose UTF-8 bytes will be split at every possible
        // boundary — the parser must never emit replacement characters
        let fixture = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"你好，世界\"}}\n";
        for chunk_size in 1..8 {
            let mut parser = SseParser::default();
            let mut accumulator = Accumulator::default();
            let mut chunks = Vec::new();
            for piece in fixture.as_bytes().chunks(chunk_size) {
                for data in parser.push(piece) {
                    if let Some(chunk) = accumulator.apply(&data) {
                        chunks.push(chunk);
                    }
                }
            }
            let joined: String = chunks.iter().map(Fragment::text).collect();
            assert_eq!(joined, "你好，世界", "chunk size {chunk_size}");
            assert!(!joined.contains('\u{FFFD}'));
        }
    }

    /// The strength rides in `output_config.effort` on this wire. An earlier
    /// version of this adapter sent `thinking.budget_tokens` instead, on the
    /// belief that effort was unsupported here — the reverse of the truth.
    /// This pins the direction so the mistake cannot be made twice.
    #[test]
    fn the_strength_rides_in_output_config_not_a_token_budget() {
        let rungs: Vec<String> = ["low", "medium", "high", "xhigh", "max"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let body = build_request(
            "m",
            8_000,
            Some(&Thinking::Rung(Effort::Xhigh)),
            &rungs,
            None,
            vec![],
            None,
        );
        assert_eq!(body["output_config"]["effort"], "xhigh");
        assert!(
            body["thinking"].get("budget_tokens").is_none(),
            "budget_tokens is the older control and is not what this sends"
        );
    }

    /// Five rungs, and `xhigh` is newer than `max` — some models that have
    /// `max` do not have `xhigh`. A model saying so gets its request placed
    /// on what it does have.
    #[test]
    fn a_model_without_xhigh_places_the_request_on_what_it_has() {
        let no_xhigh: Vec<String> = ["low", "medium", "high", "max"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let body = build_request(
            "m",
            8_000,
            Some(&Thinking::Rung(Effort::Xhigh)),
            &no_xhigh,
            None,
            vec![],
            None,
        );
        assert_eq!(
            body["output_config"]["effort"], "max",
            "one step from high and from max — the tie rounds up"
        );
        assert!(downshift(Effort::Xhigh, &no_xhigh).is_some());
    }

    /// Off must stay expressible on this wire, and it must not carry a budget.
    #[test]
    fn off_disables_without_touching_the_effort_field() {
        let off = build_request("m", 8_000, Some(&Thinking::Off), &[], None, vec![], None);
        assert_eq!(off["thinking"], json!({"type": "disabled"}));
        assert!(off.get("output_config").is_none());
    }
}
