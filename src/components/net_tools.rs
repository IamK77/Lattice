//! The `Fetch` tool: retrieve a URL over HTTP(S). This is the `network`
//! effect surface made real. The tool declares `network`; whether it may
//! reach the wire at all is the policy's call. A large body is exactly the
//! kind of result the context gate later digests, so the audit ledger keeps
//! the original and the model sees a note.
//!
//! Cancellation runs through the same token the model adapters use: an
//! interrupt or the watchman deadline aborts the in-flight request.

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "net-tools";

pub fn tool_decls(network: &[String]) -> Vec<Value> {
    vec![json!({
        "name": "Fetch",
        "description": "Fetch HTTP(S) once. HTML defaults to readable article text with code and links; format=raw keeps markup. Results include exact captured bytes and complete readable text as local artifacts for Read/Grep, source URL/time, and explicit truncation. Reading artifacts never repeats the HTTP request.",
        "parameters": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "method": {"type": "string"},
                "format": {"type": "string", "enum": ["auto", "raw", "text"], "description": "auto (default) extracts HTML, raw preserves markup, text explicitly requests readable HTML"},
            },
            "required": ["url"],
        },
        "effects": {"network": network, "writes": ["<tool artifacts>"]},
    })]
}

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: Some(EffectSurface {
            network: vec!["*".to_string()],
            writes: vec!["<tool artifacts>".to_string()],
            ..Default::default()
        }),
        implements: vec!["tool-provider".to_string()],
        tools: tool_decls(&["*".to_string()]),
        // No fragment: the `Fetch` schema already says what fetch does, and a
        // paragraph repeating it would cost every call the tokens twice.
        prompt: None,
        handle_timeout_ms: Some(60_000),
        // Several at once: a model that asks for three files in one turn
        // means them read together, not one after another. Safe here because
        // nothing is kept between deliveries.
        concurrency: Some(8),
    }
}

pub struct NetTools {
    max_bytes: usize,
    max_download_bytes: usize,
    exclusive: bool,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

impl NetTools {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        Self {
            // A context budget, not a safety limit — see the same field on the
            // `Run` tool. Roomier than a command's output because a page is
            // usually fetched to be read whole, and because `truncated: true`
            // already tells the model it did not get all of it.
            max_bytes: get("maxBytes")
                .and_then(Value::as_u64)
                .unwrap_or(65_536)
                .min(65_536) as usize,
            max_download_bytes: get("maxDownloadBytes")
                .and_then(Value::as_u64)
                .unwrap_or(8 * 1024 * 1024)
                .clamp(1, 16 * 1024 * 1024) as usize,
            exclusive: get("exclusive").and_then(Value::as_bool).unwrap_or(false),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build the Fetch tool's async runtime"),
            client: reqwest::Client::new(),
        }
    }

    fn fetch(&self, url: &str, method: &str, format: &str, ctx: &Ctx) -> Value {
        if !matches!(format, "auto" | "raw" | "text") {
            return err(
                "tool.bad_arguments",
                "format must be auto, raw, or text",
                "request",
            );
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return err(
                "tool.bad_arguments",
                "url must be http:// or https://",
                "request",
            );
        }
        let method = match method.to_ascii_uppercase().parse::<reqwest::Method>() {
            Ok(method) => method,
            Err(_) => return err("tool.bad_arguments", "unrecognized HTTP method", "request"),
        };
        let token = ctx.cancellation();
        let client = self.client.clone();
        let url = url.to_string();
        let max = self.max_download_bytes;
        self.runtime.block_on(async {
            let request = client
                .request(method, &url)
                .header(reqwest::header::USER_AGENT, "Lattice")
                .send();
            let response = tokio::select! {
                _ = token.cancelled() => return json!({"status": "cancelled"}),
                outcome = request => outcome,
            };
            let mut response = match response {
                Ok(response) => response,
                Err(e) => return err("tool.network", &e.to_string(), "environment"),
            };
            let http_status = response.status().as_u16();
            let source = response.url().to_string();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let mut bytes = Vec::new();
            loop {
                let chunk = tokio::select! {
                    _ = token.cancelled() => return json!({"status": "cancelled"}),
                    chunk = response.chunk() => chunk,
                };
                match chunk {
                    Ok(Some(chunk)) => {
                        let take = chunk.len().min(max + 1 - bytes.len());
                        bytes.extend_from_slice(&chunk[..take]);
                        if bytes.len() > max {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return err("tool.network", &e.to_string(), "environment"),
                }
            }
            let source_truncated = bytes.len() > max;
            bytes.truncate(max);
            let dir = match super::tool_artifacts::directory(ctx.ledger_path()) {
                Ok(dir) => dir,
                Err(e) => return err("tool.artifact_io", &e, "environment"),
            };
            let body = FetchBody {
                bytes: &bytes,
                source: &source,
                content_type: &content_type,
                http_status,
                source_truncated,
            };
            match present(body, format, self.max_bytes, &dir) {
                Ok(result) => json!({"status": "ok", "result": result}),
                Err(e) => err("tool.artifact_io", &e, "environment"),
            }
        })
    }
}

struct FetchBody<'a> {
    bytes: &'a [u8],
    source: &'a str,
    content_type: &'a str,
    http_status: u16,
    source_truncated: bool,
}

fn present(
    body: FetchBody<'_>,
    format: &str,
    max_bytes: usize,
    dir: &std::path::Path,
) -> Result<Value, String> {
    let mut raw = super::tool_artifacts::store(dir, body.bytes, "raw")?;
    raw["complete"] = json!(!body.source_truncated);
    let html = body
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("text/html")
        || body
            .content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/xhtml+xml");
    let reading = format != "raw" && html;
    let mut reading_error = None;
    let (mut text, scope) = if reading {
        match super::web_text::extract(body.bytes, body.source) {
            Ok(result) => result,
            Err(e) => {
                reading_error = Some(e);
                (String::new(), "unavailable")
            }
        }
    } else {
        (String::from_utf8_lossy(body.bytes).into_owned(), "raw")
    };
    let document = super::tool_artifacts::store(dir, text.as_bytes(), "txt")?;
    let truncated = body.source_truncated || text.len() > max_bytes;
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    let mut result = json!({
        "http_status": body.http_status, "url": body.source,
        "fetchedAt": chrono::Utc::now().to_rfc3339(), "contentType": body.content_type,
        "format": if reading { "text" } else { "raw" }, "scope": scope,
        "truncated": truncated, "sourceTruncated": body.source_truncated,
        "lossyUtf8": std::str::from_utf8(body.bytes).is_err(),
        "raw": raw, "document": document, "body": text,
    });
    if let Some(error) = reading_error {
        result["readingError"] = json!(error);
    }
    Ok(result)
}

fn err(code: &str, message: &str, blame: &str) -> Value {
    // The judgment fields are for the reader, and the reader is the model.
    // A transport failure — a reset connection, a name that did not resolve
    // this second, a timeout — is exactly the case where trying again is the
    // right move, and telling the model "not retryable, not transient" talks
    // it out of the one thing that would have worked. Everything else here
    // is a fact about the request itself, which will fail the same way twice.
    let transport = code == "tool.network";
    json!({"status": "error", "error": {
        "code": code,
        "message": message,
        "blame": blame,
        "retryable": transport,
        "transient": transport,
    }})
}

impl Component for NetTools {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        if tool != "Fetch" {
            if self.exclusive {
                let mut payload = err("tool.unknown", &format!("unknown tool: {tool}"), "request");
                payload["call"] = event.payload["call"].clone();
                ctx.emit(
                    "outcome",
                    EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
                );
            }
            return; // fan-out convention: silence on foreign tools
        }
        let args = &event.payload["arguments"];
        let method = args["method"].as_str().unwrap_or("GET");
        let mut payload = self.fetch(
            args["url"].as_str().unwrap_or(""),
            method,
            args["format"].as_str().unwrap_or("auto"),
            ctx,
        );
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}

#[cfg(test)]
mod tests;
