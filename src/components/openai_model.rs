//! The second real brain: an OpenAI-compatible Chat Completions adapter.
//!
//! Its reason to exist is double. Practically, it speaks DeepSeek's NATIVE
//! format (the production endpoint, by recorded cost decision), whose usage
//! carries the fuller cache statistics (`prompt_cache_hit_tokens` /
//! `prompt_cache_miss_tokens`). Architecturally, it is the proof that the
//! model-adapter profile is genuinely interchangeable: same ports, same
//! completed-event shape, same exam — swapping brains changes a component
//! name and config, never a wire.
//!
//! Same discipline as the Anthropic adapter: materialization (pointers →
//! messages), fingerprint verification and SSE accumulation are pure
//! functions with unit tests; the network shell is thin; the API key comes
//! from an environment variable and never appears in any event.

use std::collections::BTreeMap;

use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::components::model_common::{
    error_info, place, thinking_for_call, thinking_from_config, verify_fingerprint, Effort,
    Fragment, SseParser, Thinking,
};
use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};
use crate::kernel::log::LogReader;

pub const NAME: &str = "openai-model";

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

pub struct OpenAiModel {
    model: String,
    max_tokens: u64,
    thinking: Option<Thinking>,
    /// This model's OWN effort rungs, from its profile. Empty = nothing
    /// declared, and this wire's defaults stand in.
    rungs: Vec<String>,
    /// The last downshift already reported, so a rung that cannot be served
    /// is announced ONCE rather than before every call.
    reported_downshift: Option<String>,
    system: Option<String>,
    base_url: String,
    api_key: Option<String>,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl OpenAiModel {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let key_env = get("apiKeyEnv")
            .and_then(Value::as_str)
            .unwrap_or("DEEPSEEK_API_KEY")
            .to_string();
        Self {
            model: get("model")
                .and_then(Value::as_str)
                .unwrap_or("deepseek-v4-flash")
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
                .unwrap_or("https://api.deepseek.com")
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

impl OpenAiModel {
    /// Say out loud when a rung could not be served as asked. What lands on
    /// the ledger is the neutral rung; the word actually sent lives only in
    /// the request body, so without this a request for `max` served as `high`
    /// would leave no trace anywhere.
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

impl Component for OpenAiModel {
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
        // adapter's config is the fallback for a gateless assembly. The
        // system prompt travels that way because the context gate assembles
        // it; the thinking setting travels that way because a person can turn
        // it mid-conversation and nothing here can be reconfigured while it
        // runs. Reported before `system` borrows self, so this `&mut` does
        // not collide with the `&str` held across the call below.
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
        let url = format!("{}/chat/completions", self.base_url);
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
                                for fragment in accumulator.apply(&data) {
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
    let request = client.post(url).bearer_auth(api_key).json(body);
    super::model_http::send_chat(client, request, url, token).await
}

/// This wire's own default rungs, for a model with no profile to declare its
/// own. Conservative on purpose: `low`/`medium`/`high` is the set the OpenAI
/// documentation has carried longest and the widest set every compatible
/// endpoint is likely to accept. A model that has more (or fewer) says so in
/// its profile.
const DEFAULT_RUNGS: [&str; 3] = ["low", "medium", "high"];

/// What this request will actually carry for `want`, given the model's own
/// declared rungs.
fn wire_word(want: Effort, rungs: &[String]) -> String {
    place(want, rungs).map(str::to_string).unwrap_or_else(|| {
        let fallback: Vec<String> = DEFAULT_RUNGS.iter().map(|s| s.to_string()).collect();
        place(want, &fallback)
            .unwrap_or(Effort::High.name())
            .to_string()
    })
}

/// What to tell the user when a rung could not be served as asked — `None`
/// when it was served exactly.
///
/// This is the only place the substitution can be seen. The ledger records the
/// NEUTRAL rung, and the word actually sent lives in the request body, which
/// is not recorded anywhere; without this, asking for `max` on a two-rung
/// model and being served its floor would leave no trace at all.
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
    let mut all = Vec::new();
    if let Some(system) = system {
        // The stable prefix goes first; OpenAI-compatible caching is
        // automatic on repeated prefixes, no marker needed
        all.push(json!({"role": "system", "content": system}));
    }
    all.extend(messages);
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": all,
        "stream": true,
        // Without this the final usage chunk is never sent
        "stream_options": {"include_usage": true},
    });
    // DeepSeek's native format: the switch is `thinking`, the strength is
    // `reasoning_effort`. Absent = say nothing, so a plain OpenAI endpoint
    // never sees a parameter it would reject.
    match thinking {
        // "Do not think" is a VALUE of the same parameter on models that
        // carry `none` (OpenAI's bottom), and the separate switch elsewhere
        // (DeepSeek). Reaching for the switch on a model that has `none`
        // would send a parameter it may not know at all.
        Some(Thinking::Off) if rungs.iter().any(|r| r == "none") => {
            body["reasoning_effort"] = json!("none");
        }
        Some(Thinking::Off) => body["thinking"] = json!({"type": "disabled"}),
        Some(Thinking::Rung(rung)) => {
            body["thinking"] = json!({"type": "enabled"});
            body["reasoning_effort"] = json!(wire_word(*rung, rungs));
        }
        Some(Thinking::Raw(word)) => {
            body["thinking"] = json!({"type": "enabled"});
            body["reasoning_effort"] = json!(word);
        }
        None => {}
    }
    if let Some(tools) = tools.filter(|t| !t.is_empty()) {
        body["tools"] = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool["name"],
                        "description": tool["description"],
                        "parameters": tool["parameters"],
                    },
                })
            })
            .collect();
    }
    body
}

/// One material part that ANSWERS a call, as this format's `tool` message —
/// whether the part points at the result event or carries a digest standing
/// in for it. Shared so a result reads the same wherever it is placed.
fn answer_message(part: &Value, log: &LogReader) -> Result<Value, String> {
    let (id, text) = match (part["event"].as_str(), part["digest"]["of"].as_str()) {
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
        return Ok(json!({
            "role": "tool",
            "tool_call_id": call,
            "content": crate::components::model_common::interrupted_text(&event.payload),
        }));
    }
    let Some(call) = event.payload["call"].as_str() else {
        return Err(format!("tool result {id} has no call id"));
    };
    let content = match text {
        Some(note) => note.to_string(),
        None => crate::components::model_common::completion_text_at(&event.payload, &event.id),
    };
    Ok(json!({"role": "tool", "tool_call_id": call, "content": content}))
}

/// Pointers → Chat Completions messages. Tool results become one `tool`-role
/// message EACH (this API wants them separate — the exact opposite of the
/// Anthropic format, which merges them into one user turn).
/// `docs` is the directory beside the ledger where attachments live — `None`
/// when this stream has no ledger, in which case no attachment is reachable and
/// the text goes out alone.
pub fn materialize(
    parts: &[Value],
    log: &LogReader,
    docs: Option<&std::path::Path>,
) -> Result<Vec<Value>, String> {
    let messages = materialize_with_reasoning(parts, log, docs, false)?;
    // Chat tool messages are textual. Images follow the complete group of
    // tool replies as a user image message, never between parallel answers.
    let mut output = Vec::new();
    let mut images = Vec::new();
    for message in messages {
        if message["role"] != "tool" && !images.is_empty() {
            output.push(json!({"role":"user","content":std::mem::take(&mut images)}));
        }
        if message["role"] == "tool" {
            if let Some(result) = message["content"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
            {
                let pixels = super::media_document::tool_pngs(&result, docs)?;
                if !pixels.is_empty() {
                    images.push(json!({"type":"text","text":format!("Images returned by tool call {}.", message["tool_call_id"].as_str().unwrap_or_default())}));
                    images.extend(pixels.into_iter().map(|data| json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{data}")}})));
                }
            }
        }
        output.push(message);
    }
    if !images.is_empty() {
        output.push(json!({"role":"user","content":images}));
    }
    Ok(output)
}

/// The Responses encoder needs the original reasoning blocks, not the Chat
/// Completions rendering. This private flag never changes Chat wire output.
pub(crate) fn materialize_with_reasoning(
    parts: &[Value],
    log: &LogReader,
    docs: Option<&std::path::Path>,
    preserve_reasoning: bool,
) -> Result<Vec<Value>, String> {
    // Where each call's answer sits, so an assistant turn can be followed
    // IMMEDIATELY by its results — this format rejects the whole request
    // otherwise, and the ledger's order is causal, not conversational. A call
    // with no answer at all is dropped rather than replayed: presenting it as
    // if it had come back is the very thing being rejected.
    let answers = crate::components::model_common::answer_index(parts, log)?;
    let mut placed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut messages: Vec<Value> = Vec::new();
    for (at, part) in parts.iter().enumerate() {
        // Already emitted, right behind the call it answers
        if placed.contains(&at) {
            continue;
        }
        if let Some(inline) = part.get("inline") {
            messages.push(inline.clone());
            continue;
        }
        // A digest part: present the note instead of the original (contract
        // 02, third part kind). The note carries the recall id in its text.
        if let Some(digest) = part.get("digest") {
            if let Some(summary) = digest["of"]
                .as_str()
                .map(|id| log.get(id))
                .transpose()
                .map_err(|e| e.to_string())?
                .flatten()
            {
                if let Some(native) = summary
                    .payload
                    .get("nativeCompaction")
                    .filter(|v| !v.is_null())
                {
                    if !preserve_reasoning {
                        return Err(
                            "native Responses compaction requires its originating dialect".into(),
                        );
                    }
                    messages.push(json!({"_native_compaction":native}));
                    continue;
                }
            }
            let text = digest["text"].as_str().unwrap_or_default();
            // A digest standing in for a tool result MUST still materialize as a
            // `tool` message: this format rejects an assistant `tool_calls`
            // message not immediately followed by a tool message for each id.
            // Context management (contract 02) may digest a tool result on its
            // own — the adapter, which knows this format's rule, repairs the
            // adjacency by re-tagging the digest as the tool reply it replaces.
            if let Some(of) = digest["of"].as_str() {
                if let Some(orig) = log.get(of).map_err(|e| e.to_string())? {
                    if orig.event_type == ce::TOOL_EXEC_COMPLETED {
                        if let Some(call) = orig.payload["call"].as_str() {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": call,
                                "content": text,
                            }));
                            continue;
                        }
                    }
                }
            }
            messages.push(json!({
                "role": "user",
                "content": format!("[digested] {text}"),
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
                let images = crate::components::model_common::attached_images(&event.payload, docs);
                if images.is_empty() {
                    // A BARE STRING while there is nothing else to say. This
                    // wire accepts either, and some OpenAI-compatible endpoints
                    // only accept the string — so the shape does not change
                    // for the overwhelmingly common message that has no
                    // picture attached to it.
                    messages.push(json!({"role": "user", "content": event.payload["text"]}));
                } else {
                    let mut content: Vec<Value> = images
                        .into_iter()
                        .map(|(media, data)| {
                            json!({"type": "image_url", "image_url": {
                                "url": format!("data:{media};base64,{data}")}})
                        })
                        .collect();
                    content.push(json!({"type": "text", "text": event.payload["text"]}));
                    messages.push(json!({"role": "user", "content": content}));
                }
            }
            ce::WAKE => messages.push(json!({
                "role": "user",
                "content": crate::components::model_common::wake_text(&event.payload),
            })),
            ce::MODEL_CALL_COMPLETED => {
                let text = event.payload["text"].as_str().unwrap_or_default();
                let calls: Vec<Value> = event.payload["toolCalls"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .filter(|call| {
                        call["id"]
                            .as_str()
                            .is_some_and(|id| answers.contains_key(id))
                    })
                    .map(|call| {
                        json!({
                            "id": call["id"],
                            "type": "function",
                            "function": {
                                "name": call["tool"],
                                // Arguments travel as a JSON STRING in this format
                                "arguments": call["arguments"].to_string(),
                            },
                        })
                    })
                    .collect();
                if text.is_empty()
                    && calls.is_empty()
                    && !(preserve_reasoning
                        && event.payload["reasoning"]
                            .as_array()
                            .is_some_and(|r| !r.is_empty()))
                {
                    continue;
                }
                let mut message = json!({"role": "assistant", "content": text});
                if !calls.is_empty() {
                    message["tool_calls"] = json!(calls);
                    // Dialect law, measured against the live endpoint: on a
                    // turn that called a tool this family DEMANDS the thinking
                    // back — "The `reasoning_content` in the thinking mode must
                    // be passed back to the API", 400 — and an EMPTY string
                    // satisfies it. So the field goes out on EVERY tool-call
                    // turn, present or not.
                    //
                    // Sending it unconditionally is what makes a model swap
                    // survivable. A turn can reach this point with nothing
                    // readable to hand back for reasons that have nothing to do
                    // with this endpoint: another dialect sealed its thinking
                    // (Anthropic's redacted blocks are carried but unreadable),
                    // or the model in that seat did not think at all, or
                    // thinking was off then and is on now. Keying off "did we
                    // record any thinking" would leave every one of those as a
                    // conversation that cannot make another call.
                    //
                    // On a turn that called nothing the field is not asked for,
                    // so it is not sent. Endpoints without the notion ignore
                    // one they are handed.
                    message["reasoning_content"] = json!(ce::reasoning_text(&event.payload));
                }
                if preserve_reasoning {
                    message["_reasoning"] = event.payload["reasoning"].clone();
                    if let Some(output) = event.payload["responsesOutput"].as_array() {
                        // Keep provider order and item identities; omit only calls
                        // without an answer, as the shared materializer does.
                        message["_responses_output"] = json!(output
                            .iter()
                            .filter(|item| {
                                item["type"] != "function_call"
                                    || item["call_id"]
                                        .as_str()
                                        .is_some_and(|id| answers.contains_key(id))
                            })
                            .collect::<Vec<_>>());
                    }
                }
                let ids: Vec<String> = calls
                    .iter()
                    .filter_map(|c| c["id"].as_str().map(str::to_string))
                    .collect();
                messages.push(message);
                // The results, right here, in the order their calls were made
                for id in ids {
                    if let Some(&answer_at) = answers.get(&id) {
                        messages.push(answer_message(&parts[answer_at], log)?);
                        placed.insert(answer_at);
                    }
                }
            }
            // One call, one answer — whichever way the second one got onto the
            // ledger. A call the kernel settled AND the tool reported on has
            // two endings; so does one a component answered twice. Both are
            // states the ledger can be in and neither can be edited out of it,
            // so the adapter is where the duplicate has to stop: a second tool
            // message for one id is refused by the endpoint, and it would be in
            // the material for every turn after this one.
            ce::INTERRUPTED | ce::TOOL_EXEC_COMPLETED
                if crate::components::model_common::is_chosen_answer(&answers, parts, at, log)? =>
            {
                messages.push(answer_message(part, log)?);
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

struct ToolCall {
    id: String,
    name: String,
    args: String,
}

/// Folds Chat Completions stream chunks into one completed-state payload.
/// `apply` returns the text chunk to surface live, if any.
#[derive(Default)]
pub struct Accumulator {
    /// Whether any event at all arrived. A 200 whose body was not this format
    /// produces none, and reporting that as a successful empty reply is worse
    /// than any error: seen for real when a catalog entry's `baseUrl` was
    /// missing its `/v1`, so the request reached a gateway's HOME PAGE. The
    /// HTML parsed as zero events, the call was recorded `status: ok` with no
    /// text, and nothing anywhere said the address was wrong.
    saw_event: bool,
    text: String,
    reasoning: String,
    calls: BTreeMap<u64, ToolCall>,
    usage: Map<String, Value>,
    finish_reason: Option<String>,
    error: Option<String>,
}

impl Accumulator {
    /// Returns every fragment this chunk carried. A Vec rather than an Option
    /// because THIS wire format puts thinking and reply in one delta object:
    /// at the moment the model stops thinking and starts answering, a single
    /// chunk can hold both, and taking only the first would silently drop the
    /// other. (The Anthropic accumulator returns an Option — there each block
    /// is separate by construction. Both iterate the same at the call site.)
    pub fn apply(&mut self, data: &Value) -> Vec<Fragment> {
        self.saw_event = true;
        // A mid-stream error object ends the message
        if let Some(error) = data.get("error") {
            self.error = Some(error.to_string());
            return Vec::new();
        }
        // The final chunk carries usage at the top level (with
        // stream_options.include_usage) — DeepSeek's native format includes
        // the cache hit/miss token counts here
        if let Some(usage) = data["usage"].as_object() {
            self.usage.extend(usage.clone());
        }
        let choice = &data["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_string());
        }
        let delta = &choice["delta"];
        if let Some(fragments) = delta["tool_calls"].as_array() {
            for fragment in fragments {
                let index = fragment["index"].as_u64().unwrap_or(0);
                let call = self.calls.entry(index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    args: String::new(),
                });
                if let Some(id) = fragment["id"].as_str() {
                    call.id.push_str(id);
                }
                if let Some(name) = fragment["function"]["name"].as_str() {
                    call.name.push_str(name);
                }
                if let Some(args) = fragment["function"]["arguments"].as_str() {
                    call.args.push_str(args);
                }
            }
        }
        // Thinking has its own field and never merges into the reply text.
        // Both are read from the SAME delta: a chunk may carry either or both.
        let mut fragments = Vec::new();
        if let Some(chunk) = delta["reasoning_content"].as_str() {
            if !chunk.is_empty() {
                self.reasoning.push_str(chunk);
                fragments.push(Fragment::Reasoning(chunk.to_string()));
            }
        }
        if let Some(chunk) = delta["content"].as_str() {
            if !chunk.is_empty() {
                self.text.push_str(chunk);
                fragments.push(Fragment::Text(chunk.to_string()));
            }
        }
        fragments
    }

    /// The reasoning parts of this call, in contract shape. DeepSeek's native
    /// format hands back one plain string, so one readable part with no
    /// dialect baggage covers it.
    fn reasoning_parts(&self) -> Vec<Value> {
        let (_, inlined) = Self::split_inline_thinking(&self.text);
        let mut thought = self.reasoning.clone();
        if !inlined.is_empty() {
            if !thought.is_empty() {
                thought.push('\n');
            }
            thought.push_str(&inlined);
        }
        if thought.is_empty() {
            return Vec::new();
        }
        vec![ce::reasoning_text_part(&thought, None)]
    }

    /// The reply, with any thinking a gateway inlined into it taken out.
    fn reply_text(&self) -> String {
        Self::split_inline_thinking(&self.text).0
    }

    fn tool_calls(&self) -> Vec<Value> {
        self.calls
            .values()
            .map(|call| {
                let arguments: Value =
                    serde_json::from_str(&call.args).unwrap_or_else(|_| json!({}));
                json!({"id": call.id, "tool": call.name, "arguments": arguments})
            })
            .collect()
    }

    /// Whatever the model had thought by this point rides along on EVERY
    /// completed state, including the ones that ended badly — a call that
    /// died mid-thought is exactly the one whose reasoning a reader needs.
    /// Thinking a gateway flattened into the reply, moved where it belongs.
    ///
    /// This wire format has a field for thinking (`reasoning_content`), and a
    /// provider that uses it needs nothing here. Some gateways in front of a
    /// reasoning model do not: they inline the thought into the CONTENT as
    /// `<thinking>…</thinking>`, so it arrives as part of the answer and is
    /// shown to the person as if the model had said it out loud.
    ///
    /// Recognised only in this exact shape, and only where it is the shape the
    /// whole segment has: a tag pair the model happened to write about (in
    /// prose, in a code block) reads as one it wrote, and there is no way to
    /// tell them apart from here. Splitting on a literal tag IS a guess; it is
    /// made because a gateway already guessed wrong upstream, and this is the
    /// dialect's job — knowing how one endpoint really answers.
    fn split_inline_thinking(text: &str) -> (String, String) {
        const OPEN: &str = "<thinking>";
        const CLOSE: &str = "</thinking>";
        let (mut reply, mut thought) = (String::new(), String::new());
        let mut rest = text;
        while let Some(at) = rest.find(OPEN) {
            let Some(end) = rest[at..].find(CLOSE) else {
                break; // an unclosed tag is prose, not a wrapper
            };
            reply.push_str(&rest[..at]);
            thought.push_str(&rest[at + OPEN.len()..at + end]);
            thought.push('\n');
            rest = &rest[at + end + CLOSE.len()..];
        }
        reply.push_str(rest);
        (reply.trim().to_string(), thought.trim().to_string())
    }

    fn with_reasoning(&self, mut payload: Value) -> Value {
        let parts = self.reasoning_parts();
        if !parts.is_empty() {
            payload["reasoning"] = json!(parts);
        }
        payload
    }

    pub fn finish(&self) -> Value {
        if !self.saw_event && self.error.is_none() {
            return self.with_reasoning(json!({
                "status": "error",
                "error": error_info(
                    "provider.not_this_format",
                    "the endpoint answered 200 but sent nothing this format recognises — \
                     check the baseUrl (an OpenAI-compatible one usually ends in /v1)",
                    "request",
                    false,
                    false,
                ),
                "usage": self.usage,
            }));
        }
        if let Some(error) = &self.error {
            return self.with_reasoning(json!({
                "status": "error",
                "error": error_info("provider.api_error", error, "provider", false, true),
                "usage": self.usage,
            }));
        }
        // Same defense as the Anthropic adapter: a response truncated by the
        // token limit ("length" here) can carry tool-call JSON that parses
        // but is silently incomplete — executing it would act on half an
        // intention. Fail the whole call.
        if self.finish_reason.as_deref() == Some("length") && !self.calls.is_empty() {
            return self.with_reasoning(json!({
                "status": "error",
                "error": error_info(
                    "provider.truncated_tool_call",
                    "response hit the token limit mid tool call; arguments may be silently incomplete",
                    "request",
                    false,
                    false,
                ),
                "text": self.reply_text(),
                "stopReason": "length",
                "usage": self.usage,
            }));
        }
        let mut payload = json!({"status": "ok", "usage": self.usage});
        if let Some(reason) = &self.finish_reason {
            // Truncation must be visible: "length" shows up here
            payload["stopReason"] = json!(reason);
        }
        let reply = self.reply_text();
        if !reply.is_empty() {
            payload["text"] = json!(reply);
        }
        let calls = self.tool_calls();
        if !calls.is_empty() {
            payload["toolCalls"] = json!(calls);
        }
        self.with_reasoning(payload)
    }

    pub fn finish_cancelled(&self) -> Value {
        self.with_reasoning(json!({
            "status": "cancelled", "text": self.reply_text(), "usage": self.usage,
        }))
    }

    pub fn finish_error(&self, error: &str) -> Value {
        self.with_reasoning(json!({
            "status": "error",
            "error": error_info("provider.stream_interrupted", error, "provider", false, true),
            "text": self.reply_text(),
            "usage": self.usage,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::core_events::core_event_decls;
    use crate::kernel::log::EventLog;

    /// A gateway that inlines thinking into the reply has it moved back.
    ///
    /// Seen for real: `gpt-5.6-sol` behind a gateway answered with
    /// `<thinking>**Writing proposal before implementation**</thinking>`
    /// inside its CONTENT, seven times in one conversation, so the thought was
    /// displayed as if it had been said out loud. DeepSeek's endpoint uses the
    /// field this format has for it and never does this.
    #[test]
    fn thinking_a_gateway_inlined_into_the_reply_is_moved_to_where_it_belongs() {
        let mut acc = Accumulator::default();
        acc.apply(&json!({"choices": [{"delta": {
            "content": "<thinking>**Writing the proposal**</thinking>Here is the plan."
        }}]}));
        let done = acc.finish();
        assert_eq!(
            done["text"], "Here is the plan.",
            "the thought is out of the reply"
        );
        assert_eq!(
            done["reasoning"][0]["text"], "**Writing the proposal**",
            "and in the reasoning, where a frontend keeps it out of the answer"
        );
    }

    /// An unclosed tag is prose, not a wrapper. Cutting at an opening tag with
    /// no closing one would swallow the rest of the answer.
    #[test]
    fn an_unclosed_tag_is_left_where_it_is() {
        let mut acc = Accumulator::default();
        acc.apply(&json!({"choices": [{"delta": {
            "content": "write <thinking> in the docs to mean a thought"
        }}]}));
        let done = acc.finish();
        assert_eq!(
            done["text"], "write <thinking> in the docs to mean a thought",
            "untouched"
        );
        assert!(done.get("reasoning").is_none());
    }

    /// A provider that uses the field this format HAS for thinking keeps
    /// working exactly as before, and both sources end up together.
    #[test]
    fn the_proper_field_still_works_and_the_two_sources_join() {
        let mut acc = Accumulator::default();
        acc.apply(&json!({"choices": [{"delta": {"reasoning_content": "proper thought"}}]}));
        acc.apply(&json!({"choices": [{"delta": {
            "content": "<thinking>inlined thought</thinking>answer"
        }}]}));
        let done = acc.finish();
        assert_eq!(done["text"], "answer");
        let thought = done["reasoning"][0]["text"].as_str().unwrap();
        assert!(thought.contains("proper thought"), "{thought}");
        assert!(thought.contains("inlined thought"), "{thought}");
    }

    #[test]
    fn responses_reasoning_only_event_survives_materialization() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let output = json!([{"type":"reasoning", "id":"r", "summary":[],
            "encrypted_content":"sealed"}]);
        let payload = crate::components::responses_wire::completed(
            &json!({"status":"completed","output":output}),
        )
        .unwrap();
        let event = log
            .append(
                EventDraft::new(ce::MODEL_CALL_COMPLETED, &[], payload),
                "model",
            )
            .unwrap();
        let parts = [json!({"event":event.id})];
        let input = crate::components::responses_model::materialize(
            &parts,
            &log.reader(),
            None,
            "model",
            "http://localhost",
        )
        .unwrap();
        assert_eq!(json!(input), output);
        assert_eq!(
            materialize(&parts, &log.reader(), None).unwrap_err(),
            "materialized to an empty conversation"
        );
    }

    #[test]
    fn responses_materialization_retains_interleaved_provider_output() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let output = json!([
            {"type":"reasoning","id":"r1","summary":[],"encrypted_content":"one"},
            {"type":"function_call","id":"f1","call_id":"c1","name":"Read","arguments":"{}"},
            {"type":"message","id":"m","role":"assistant","phase":"commentary",
             "content":[{"type":"output_text","text":"checking"}]},
            {"type":"reasoning","id":"r2","summary":[],"encrypted_content":"two"},
            {"type":"function_call","id":"f2","call_id":"c2","name":"Read","arguments":"{}"}
        ]);
        let payload = crate::components::responses_wire::completed(
            &json!({"status":"completed","output":output}),
        )
        .unwrap();
        let event = log
            .append(
                EventDraft::new(ce::MODEL_CALL_COMPLETED, &[], payload),
                "model",
            )
            .unwrap();
        let mut parts = vec![json!({"event":event.id})];
        for call in ["c2", "c1"] {
            let answer = log
                .append(
                    EventDraft::new(
                        ce::TOOL_EXEC_COMPLETED,
                        &[&event.id],
                        json!({"call":call,"status":"ok","result":"answer"}),
                    ),
                    "tool",
                )
                .unwrap();
            parts.push(json!({"event":answer.id}));
        }
        let input = crate::components::responses_model::materialize(
            &parts,
            &log.reader(),
            None,
            "model",
            "http://localhost",
        )
        .unwrap();
        assert_eq!(json!(&input[..5]), output);
        assert_eq!(input[5]["type"], "function_call_output");
        assert_eq!(input[5]["call_id"], "c1");
        assert_eq!(input[6]["call_id"], "c2");
        assert_eq!(input.len(), 7);
    }

    #[test]
    fn materializes_a_tool_round_trip_in_chat_completions_shape() {
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
                        {"id": "call_1", "tool": "calc", "arguments": {"numbers": [4, 7]}}
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
                    json!({"call": "call_1", "status": "ok", "result": 11.0}),
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
        assert_eq!(
            messages[0],
            json!({"role": "user", "content": "add 4 and 7"})
        );
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "calc");
        // Arguments must be a JSON string, not an object
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            json!("{\"numbers\":[4,7]}")
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
    }

    /// DeepSeek answers 400 if an assistant turn that called a tool comes back
    /// without its thinking, and ignores the thinking on a turn that called
    /// nothing. Both halves are pinned: sending it where it is demanded, and
    /// not paying for it where it is discarded.
    #[test]
    fn thinking_rides_back_with_tool_calls_and_only_with_tool_calls() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let thought = json!([{"kind": "text", "text": "they want a sum"}]);
        let called = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "reasoning": thought, "toolCalls": [
                        {"id": "call_1", "tool": "calc", "arguments": {}}
                    ]}),
                ),
                "model",
            )
            .unwrap();
        let spoke = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "reasoning": thought, "text": "it is 11"}),
                ),
                "model",
            )
            .unwrap();

        // The call must be ANSWERED, or it is not replayable at all
        let outcome = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&called.id],
                    json!({"call": "call_1", "status": "ok", "result": 11.0}),
                ),
                "tools",
            )
            .unwrap();
        let messages = materialize(
            &[
                json!({"event": called.id}),
                json!({"event": outcome.id}),
                json!({"event": spoke.id}),
            ],
            &log.reader(),
            None,
        )
        .unwrap();
        assert_eq!(messages[0]["reasoning_content"], "they want a sum");
        assert_eq!(messages[1]["role"], "tool");
        assert!(messages[2].get("reasoning_content").is_none());
    }

    /// A tool-call turn with nothing readable to hand back still hands back the
    /// field, because the endpoint that demands it accepts an empty string and
    /// rejects an absent one. Three ways a turn arrives here empty, all of them
    /// reachable by swapping the model mid-conversation: another dialect sealed
    /// the thinking, another model did not think, thinking was off at the time.
    #[test]
    fn a_tool_call_turn_with_no_readable_thinking_still_answers_the_demand() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let calls = json!([{"id": "call_1", "tool": "calc", "arguments": {}}]);
        // Sealed by another dialect: carried, unreadable, contributes no text
        let sealed = json!([{"kind": "hidden", "opaque": {"data": "AAAA"}}]);
        let cases = [
            json!({"status": "ok", "reasoning": sealed, "toolCalls": calls}),
            json!({"status": "ok", "reasoning": [], "toolCalls": calls}),
            json!({"status": "ok", "toolCalls": calls}),
        ];
        for payload in cases {
            let called = log
                .append(
                    EventDraft::new(ce::MODEL_CALL_COMPLETED, &[], payload),
                    "model",
                )
                .unwrap();
            let outcome = log
                .append(
                    EventDraft::new(
                        ce::TOOL_EXEC_COMPLETED,
                        &[&called.id],
                        json!({"call": "call_1", "status": "ok", "result": 11.0}),
                    ),
                    "tools",
                )
                .unwrap();
            let messages = materialize(
                &[json!({"event": called.id}), json!({"event": outcome.id})],
                &log.reader(),
                None,
            )
            .unwrap();
            assert_eq!(
                messages[0]["reasoning_content"], "",
                "an empty thought is what satisfies the demand; an absent field is a 400"
            );
        }
    }

    /// The other direction of a swap: thinking a DIFFERENT dialect produced is
    /// readable prose here, so it rides back as prose. Nothing about the block
    /// it came from travels with it — this wire has no place to put it and the
    /// endpoint asked for none.
    #[test]
    fn thinking_from_another_dialect_rides_back_as_prose() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        // What the Anthropic adapter records: text plus that dialect's own
        // baggage, which only that dialect can use
        let foreign = json!([
            {"kind": "text", "text": "they want a sum", "opaque": {"signature": "sig-abc"}},
            {"kind": "hidden", "opaque": {"data": "AAAA"}},
        ]);
        let called = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "reasoning": foreign, "toolCalls": [
                        {"id": "call_1", "tool": "calc", "arguments": {}}
                    ]}),
                ),
                "model",
            )
            .unwrap();
        let outcome = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&called.id],
                    json!({"call": "call_1", "status": "ok", "result": 11.0}),
                ),
                "tools",
            )
            .unwrap();
        let messages = materialize(
            &[json!({"event": called.id}), json!({"event": outcome.id})],
            &log.reader(),
            None,
        )
        .unwrap();
        assert_eq!(messages[0]["reasoning_content"], "they want a sum");
        let wire = messages[0].to_string();
        assert!(
            !wire.contains("sig-abc") && !wire.contains("AAAA"),
            "the other dialect's baggage must not reach this wire: {wire}"
        );
    }

    /// A call nobody ever answered — a gate still waiting on a human, an
    /// interrupt, a crashed process — must not be replayed as if it had
    /// happened. Both wire formats reject an assistant turn whose tool calls
    /// go unanswered, and reject the WHOLE request: one hanging call would
    /// otherwise make every later message in that conversation fail forever,
    /// with no way back. (Seen in the wild: three authorization questions,
    /// one answered, and the session could never speak again.)
    #[test]
    fn a_call_nobody_answered_is_left_out_rather_than_bricking_the_conversation() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let asked = log
            .append(
                EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
                "ui",
            )
            .unwrap();
        let wanted = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "text": "installing", "toolCalls": [
                        {"id": "answered", "tool": "calc", "arguments": {}},
                        {"id": "hanging", "tool": "InstallComponent", "arguments": {}},
                    ]}),
                ),
                "model",
            )
            .unwrap();
        let outcome = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&wanted.id],
                    json!({"call": "answered", "status": "ok", "result": 1.0}),
                ),
                "tools",
            )
            .unwrap();

        let messages = materialize(
            &[
                json!({"event": asked.id}),
                json!({"event": wanted.id}),
                json!({"event": outcome.id}),
            ],
            &log.reader(),
            None,
        )
        .unwrap();
        let assistant = &messages[1];
        let ids: Vec<&str> = assistant["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec!["answered"],
            "only the call that came back may be replayed"
        );
        // What it said still stands; only the unanswerable call is gone
        assert_eq!(assistant["content"], "installing");
    }

    /// The ledger's order is CAUSAL; the wire's order is conversational. A
    /// background wake, or a gate holding one call while others run, puts
    /// several assistant turns on the record before any result arrives — and
    /// this format rejects the whole request unless each assistant turn is
    /// followed immediately by its own results. Placing them is the adapter's
    /// job, because the shape is the dialect's demand, not the ledger's fault.
    /// (Seen in the wild: two wakes mid-round, and every later message 400ed.)
    #[test]
    fn each_assistant_turn_is_followed_by_its_own_results_whatever_the_ledger_order() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let call = |log: &mut EventLog, id: &str| {
            log.append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "toolCalls": [
                        {"id": id, "tool": "calc", "arguments": {}}
                    ]}),
                ),
                "model",
            )
            .unwrap()
        };
        let result = |log: &mut EventLog, id: &str| {
            log.append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[],
                    json!({"call": id, "status": "ok", "result": 1.0}),
                ),
                "tools",
            )
            .unwrap()
        };
        // Two calls in a row, THEN both results — the shape a wake produced
        let first = call(&mut log, "a");
        let second = call(&mut log, "b");
        let ra = result(&mut log, "a");
        let rb = result(&mut log, "b");

        let messages = materialize(
            &[
                json!({"event": first.id}),
                json!({"event": second.id}),
                json!({"event": ra.id}),
                json!({"event": rb.id}),
            ],
            &log.reader(),
            None,
        )
        .unwrap();

        let shape: Vec<(&str, &str)> = messages
            .iter()
            .map(|m| {
                (
                    m["role"].as_str().unwrap(),
                    m["tool_call_id"]
                        .as_str()
                        .or_else(|| m["tool_calls"][0]["id"].as_str())
                        .unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("assistant", "a"),
                ("tool", "a"),
                ("assistant", "b"),
                ("tool", "b"),
            ],
            "each call is answered before the next one is made"
        );
    }

    /// A call the kernel SETTLED still answers the model — and answers it
    /// honestly. Dropping it would leave the model believing it never asked,
    /// so it would ask again, forever. Calling it "failed" would be worse: a
    /// component can die with its work half done, and a model told "failed"
    /// retries something that may already have deleted a file or spent money.
    /// The only true thing is that it ended and nobody knows what it did.
    #[test]
    fn a_settled_call_tells_the_model_it_ended_without_claiming_it_failed() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let asked = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "toolCalls": [
                        {"id": "gone", "tool": "vanish", "arguments": {}}
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
                    json!({"call": "gone", "tool": "vanish", "arguments": {}}),
                ),
                "loop",
            )
            .unwrap();
        let settled = log
            .append(
                EventDraft::new(ce::INTERRUPTED, &[&started.id], json!({"by": "crash"})),
                "core",
            )
            .unwrap();

        let messages = materialize(
            &[json!({"event": asked.id}), json!({"event": settled.id})],
            &log.reader(),
            None,
        )
        .unwrap();
        assert_eq!(
            messages[0]["tool_calls"][0]["id"], "gone",
            "the call is replayed, because it IS answered"
        );
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "gone");
        let told = messages[1]["content"].as_str().unwrap();
        assert!(
            told.contains("interrupted"),
            "it says what happened: {told}"
        );
        assert!(
            told.contains("unknown"),
            "and refuses to claim the work did not happen: {told}"
        );
    }

    /// Two honest answers, one call — from a real 400 on 2026-07-28.
    ///
    /// A shell command ran past its deadline. The kernel settled the call
    /// ("interrupted, outcome unknown") and the tool, seeing the same
    /// cancellation, ALSO reported back ("cancelled"). Both went into the
    /// material, both were rendered, and DeepSeek answered: "Messages with
    /// role 'tool' must be a response to a preceding message with tool_calls".
    /// Worse, the pair stays in the material, so every later turn died too.
    #[test]
    fn a_call_answered_twice_still_renders_exactly_one_tool_message() {
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
        // The tool answers too, a moment later, on its way out
        let reported = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&started.id],
                    json!({"call": "slow", "status": "cancelled", "result": "no output"}),
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
        let answers: Vec<&serde_json::Value> = messages
            .iter()
            .filter(|m| m["role"] == "tool" && m["tool_call_id"] == "slow")
            .collect();
        assert_eq!(
            answers.len(),
            1,
            "one call, one answer — two is an orphan and a 400: {messages:#?}"
        );
        assert!(
            answers[0]["content"]
                .as_str()
                .unwrap()
                .contains("cancelled"),
            "and it is the TOOL's word, which says more than the kernel's"
        );
    }

    /// The sibling of the test above, and the one that was missing: two
    /// RESULTS rather than a settlement and a result.
    ///
    /// The kernel writes no second ending, so this shape does not come from
    /// it — it comes from a ledger written before that rule, or from a
    /// component in another language answering twice. That is the whole reason
    /// the adapter is a second line of defense rather than a formality: the
    /// kernel can only guard what it is writing now, and this material is
    /// forever.
    #[test]
    fn a_call_that_reported_twice_still_renders_exactly_one_tool_message() {
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let asked = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "toolCalls": [
                        {"id": "twice", "tool": "Run", "arguments": {}}
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
                    json!({"call": "twice", "tool": "Run", "arguments": {}}),
                ),
                "loop",
            )
            .unwrap();
        let first = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&started.id],
                    json!({"call": "twice", "status": "ok", "result": "one"}),
                ),
                "shell",
            )
            .unwrap();
        let second = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&started.id],
                    json!({"call": "twice", "status": "ok", "result": "two"}),
                ),
                "shell",
            )
            .unwrap();

        let messages = materialize(
            &[
                json!({"event": asked.id}),
                json!({"event": first.id}),
                json!({"event": second.id}),
            ],
            &log.reader(),
            None,
        )
        .unwrap();
        let answers: Vec<&serde_json::Value> = messages
            .iter()
            .filter(|m| m["role"] == "tool" && m["tool_call_id"] == "twice")
            .collect();
        assert_eq!(
            answers.len(),
            1,
            "one call, one answer — a second one is an orphan, and it stays in \
             the material for every turn after this: {messages:#?}"
        );
    }

    #[test]
    fn parallel_tool_results_stay_separate_tool_messages() {
        // The exact opposite of the Anthropic adapter's merge — this is what
        // format-specific materialization is FOR
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let a = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[],
                    json!({"call": "call_a", "status": "ok", "result": 3.0}),
                ),
                "tools",
            )
            .unwrap();
        let b = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[],
                    json!({"call": "call_b", "status": "error", "error": {"code": "tool.failed", "message": "nope", "blame": "request"}}),
                ),
                "tools",
            )
            .unwrap();

        let parts = vec![json!({"event": a.id}), json!({"event": b.id})];
        let messages = materialize(&parts, &log.reader(), None).unwrap();
        assert_eq!(messages.len(), 2, "one tool message per result");
        assert_eq!(messages[0]["tool_call_id"], "call_a");
        assert_eq!(messages[1]["tool_call_id"], "call_b");
        assert_eq!(messages[1]["content"], "nope");
    }

    #[test]
    fn a_digested_tool_result_stays_a_tool_message() {
        // Context management may digest a tool result on its own. The digest must
        // still materialize as a `tool` message so the preceding assistant
        // `tool_calls` stays satisfied — otherwise DeepSeek/OpenAI rejects the
        // request with "assistant message with tool_calls must be followed by
        // tool messages". (This is the exact 400 seen in the wild.)
        let mut log = EventLog::in_memory(core_event_decls(), "main");
        let reply = log
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status": "ok", "toolCalls": [
                        {"id": "call_9", "tool": "Fetch", "arguments": {"url": "x"}}
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
                    json!({"call": "call_9", "status": "ok", "result": "a big page"}),
                ),
                "tools",
            )
            .unwrap();
        // The material keeps the assistant call verbatim but digests the result.
        let parts = vec![
            json!({"event": reply.id}),
            json!({"digest": {"of": outcome.id, "text": "fetch returned a big page"}}),
        ];
        let messages = materialize(&parts, &log.reader(), None).unwrap();
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_9");
        assert_eq!(
            messages[1]["role"], "tool",
            "a digested tool result stays a tool message, preserving adjacency"
        );
        assert_eq!(messages[1]["tool_call_id"], "call_9");
        assert_eq!(messages[1]["content"], "fetch returned a big page");
    }

    #[test]
    fn accumulator_folds_a_streamed_reply_with_deepseek_cache_usage() {
        let mut parser = SseParser::default();
        let mut accumulator = Accumulator::default();
        let mut chunks = Vec::new();
        let fixture = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"they want\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\" a sum\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"4 + 7\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" = 11\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"type\":\"function\",\"function\":{\"name\":\"calc\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"numbers\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"[1,2]}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":25,\"prompt_cache_hit_tokens\":4,\"prompt_cache_miss_tokens\":6}}\n\n",
            "data: [DONE]\n\n",
        );
        // Feed in awkward slices to prove incremental parsing
        for piece in fixture.as_bytes().chunks(17) {
            for data in parser.push(piece) {
                chunks.extend(accumulator.apply(&data));
            }
        }

        // Thinking and reply arrive interleaved on one connection and stay
        // apart: each chunk is labelled with which one it was
        assert_eq!(
            chunks,
            vec![
                Fragment::Reasoning("they want".to_string()),
                Fragment::Reasoning(" a sum".to_string()),
                Fragment::Text("4 + 7".to_string()),
                Fragment::Text(" = 11".to_string()),
            ]
        );
        assert_eq!(
            chunks[0].note(),
            json!({"chunk": "they want", "phase": "reasoning"})
        );
        assert_eq!(chunks[2].note(), json!({"chunk": "4 + 7"}));

        let payload = accumulator.finish();
        assert_eq!(payload["status"], "ok");
        // The reply is the reply — thinking never leaks into it
        assert_eq!(payload["text"], "4 + 7 = 11");
        assert_eq!(
            payload["reasoning"],
            json!([{"kind": "text", "text": "they want a sum"}])
        );
        assert_eq!(payload["toolCalls"][0]["id"], "call_9");
        assert_eq!(payload["toolCalls"][0]["tool"], "calc");
        assert_eq!(
            payload["toolCalls"][0]["arguments"]["numbers"],
            json!([1, 2])
        );
        assert_eq!(payload["stopReason"], "tool_calls");
        // The cache statistics this adapter exists for
        assert_eq!(payload["usage"]["prompt_tokens"], 10);
        assert_eq!(payload["usage"]["completion_tokens"], 25);
        assert_eq!(payload["usage"]["prompt_cache_hit_tokens"], 4);
        assert_eq!(payload["usage"]["prompt_cache_miss_tokens"], 6);
    }

    /// The handover chunk: this wire format can put the last of the thinking
    /// and the first of the reply in ONE delta. Both must survive — reading
    /// only the first field would silently eat the answer.
    #[test]
    fn one_delta_carrying_both_thinking_and_reply_loses_neither() {
        let mut accumulator = Accumulator::default();
        let fragments = accumulator.apply(&json!({"choices": [{"index": 0, "delta": {
            "reasoning_content": "a greeting",
            "content": "Hello!",
        }, "finish_reason": null}]}));
        assert_eq!(
            fragments,
            vec![
                Fragment::Reasoning("a greeting".to_string()),
                Fragment::Text("Hello!".to_string()),
            ]
        );
        let payload = accumulator.finish();
        assert_eq!(payload["text"], "Hello!");
        assert_eq!(payload["reasoning"][0]["text"], "a greeting");
    }

    #[test]
    fn truncated_tool_calls_are_failed_not_executed() {
        let mut accumulator = Accumulator::default();
        accumulator.apply(&json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "calc", "arguments": "{\"numbers\": [1, 2]}"}}
            ]}, "finish_reason": null}],
        }));
        accumulator.apply(&json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "length"}],
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
    fn a_mid_stream_error_object_fails_the_call() {
        let mut accumulator = Accumulator::default();
        accumulator.apply(&json!({"choices": [{"index": 0, "delta": {"content": "partial"}, "finish_reason": null}]}));
        accumulator.apply(&json!({"error": {"message": "overloaded", "type": "server_error"}}));
        let payload = accumulator.finish();
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "provider.api_error");
        assert_eq!(payload["error"]["blame"], "provider");
    }

    #[test]
    fn build_request_carries_system_tools_and_usage_option() {
        let body = build_request(
            "deepseek-v4-flash",
            2048,
            None,
            &rungs(),
            Some("be brief"),
            vec![json!({"role": "user", "content": "hi"})],
            Some(&vec![json!({
                "name": "calc",
                "description": "sum",
                "parameters": {"type": "object"},
                "effects": {"reversible": true},
            })]),
        );
        assert_eq!(
            body["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "calc");
        // The effect surface is Lattice's business, not the provider's
        assert!(body["tools"][0]["function"].get("effects").is_none());
        // Unset means unsaid: a plain OpenAI endpoint must not receive a
        // parameter it has never heard of
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    /// The rungs the product actually runs on.
    fn rungs() -> Vec<String> {
        ["low", "medium", "high"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn build_request_translates_the_thinking_knob_into_deepseek_parameters() {
        let off = build_request("m", 8, Some(&Thinking::Off), &rungs(), None, vec![], None);
        assert_eq!(off["thinking"], json!({"type": "disabled"}));
        assert!(off.get("reasoning_effort").is_none());

        let on = build_request(
            "m",
            8,
            Some(&Thinking::Rung(Effort::High)),
            &rungs(),
            None,
            vec![],
            None,
        );
        assert_eq!(on["thinking"], json!({"type": "enabled"}));
        assert_eq!(on["reasoning_effort"], "high");
    }

    /// DeepSeek's real ladder, which is the one the product runs on: two
    /// rungs, and `high` is the FLOOR. The bug this pins is not a collapse
    /// but an INVERSION — an earlier version mapped `max` to the word "high"
    /// by hand, so asking for the most thinking sent the least.
    #[test]
    fn max_reaches_deepseeks_ceiling_rather_than_its_floor() {
        let deepseek: Vec<String> = ["high", "max"].iter().map(|s| s.to_string()).collect();
        let word = |rung| {
            build_request(
                "m",
                8,
                Some(&Thinking::Rung(rung)),
                &deepseek,
                None,
                vec![],
                None,
            )["reasoning_effort"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(word(Effort::Max), "max", "max must not send the floor");
        assert_eq!(word(Effort::High), "high");
        assert_eq!(word(Effort::Low), "high", "below the floor lands on it");

        assert!(downshift(Effort::Low, &deepseek).is_some(), "and says so");
        assert!(downshift(Effort::Max, &deepseek).is_none(), "max was exact");
    }

    /// "Do not think" is a VALUE of the same parameter on a model carrying
    /// `none`, and the separate switch elsewhere. Sending the switch to a
    /// model that expresses off through the parameter would mean sending a
    /// field it may not know.
    #[test]
    fn off_uses_none_where_the_model_has_it_and_the_switch_where_it_does_not() {
        let with_none: Vec<String> = ["none", "low", "medium", "high"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let body = build_request("m", 8, Some(&Thinking::Off), &with_none, None, vec![], None);
        assert_eq!(body["reasoning_effort"], "none");
        assert!(body.get("thinking").is_none(), "no switch needed");

        let without: Vec<String> = ["high", "max"].iter().map(|s| s.to_string()).collect();
        let body = build_request("m", 8, Some(&Thinking::Off), &without, None, vec![], None);
        assert_eq!(body["thinking"], json!({"type": "disabled"}));
        assert!(body.get("reasoning_effort").is_none());
    }

    /// `none` is declared but is NOT a rung: nearest-matching must never turn
    /// a request to think a little into a request not to think at all.
    #[test]
    fn none_is_never_chosen_by_nearest_matching() {
        let with_none: Vec<String> = ["none", "high"].iter().map(|s| s.to_string()).collect();
        for rung in Effort::ALL {
            let body = build_request(
                "m",
                8,
                Some(&Thinking::Rung(rung)),
                &with_none,
                None,
                vec![],
                None,
            );
            assert_eq!(
                body["reasoning_effort"], "high",
                "{rung:?} must not land on `none`"
            );
        }
    }

    /// A word Lattice does not know rides through untouched. Removing this
    /// would make a provider-specific value impossible to express at all.
    #[test]
    fn an_unknown_word_is_forwarded_rather_than_rejected_or_mapped() {
        let raw = build_request(
            "m",
            8,
            Some(&Thinking::Raw("ultra".to_string())),
            &rungs(),
            None,
            vec![],
            None,
        );
        assert_eq!(raw["thinking"], json!({"type": "enabled"}));
        assert_eq!(raw["reasoning_effort"], "ultra");
    }
}
