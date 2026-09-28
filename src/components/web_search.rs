//! Web search, with the search engine as configuration rather than as code.
//!
//! Every search service returns the same three things — a title, a link, a
//! scrap of text — and disagrees about everything else: where the key goes,
//! whether the query is a parameter or a body, what the array of results is
//! called. So the component is split the way the disagreement is: a pure
//! function that shapes the request, a pure function that reads the answer,
//! and one narrow place that speaks HTTP. The two pure halves are where the
//! per-backend knowledge lives and where the tests can reach; the network hop
//! has nothing in it worth testing.
//!
//! Four backends, chosen to cover the shapes rather than the market: a keyed
//! GET (Brave), a keyed POST with the key in the body (Tavily), a keyed POST
//! with the key in a header (Serper), and one that needs no key at all because
//! you run it yourself (SearXNG). A fifth is a `Backend` arm and two match
//! arms, which is the point of the split.

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "web-search";
pub const TOOL: &str = "WebSearch";

const DEFAULT_RESULTS: usize = 5;
const MAX_RESULTS: usize = 20;
/// Longest snippet kept per hit. A search result is a pointer, not the page:
/// what it has to do is let the model decide whether to `Fetch` it.
const MAX_SNIPPET: usize = 400;

/// Which service answers, and therefore how to ask it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Brave,
    Tavily,
    Serper,
    Searxng,
}

impl Backend {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "brave" => Some(Self::Brave),
            "tavily" => Some(Self::Tavily),
            "serper" => Some(Self::Serper),
            "searxng" | "searx" => Some(Self::Searxng),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Brave => "brave",
            Self::Tavily => "tavily",
            Self::Serper => "serper",
            Self::Searxng => "searxng",
        }
    }

    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Brave => "https://api.search.brave.com/res/v1/web/search",
            Self::Tavily => "https://api.tavily.com/search",
            Self::Serper => "https://google.serper.dev/search",
            // No default: a self-hosted instance has no address anyone else
            // can guess, so the assembly must name it.
            Self::Searxng => "",
        }
    }

    pub fn default_key_env(self) -> Option<&'static str> {
        match self {
            Self::Brave => Some("BRAVE_API_KEY"),
            Self::Tavily => Some("TAVILY_API_KEY"),
            Self::Serper => Some("SERPER_API_KEY"),
            // Yours to run, so yours to leave open or protect.
            Self::Searxng => None,
        }
    }
}

/// Percent-encode a query for a URL. Ten lines rather than a dependency: the
/// rule is small, it does not change, and a search query is exactly the case
/// where getting it wrong is silent — a `&` or a `+` in the words quietly
/// searches for something else.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A request, described rather than sent — so the shaping can be tested
/// without a network, a key, or a service that might be down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub post: bool,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// One result. The three things every service agrees on.
pub fn hit(title: &str, url: &str, snippet: &str) -> Value {
    let mut snippet = snippet.trim().to_string();
    if snippet.chars().count() > MAX_SNIPPET {
        snippet = snippet.chars().take(MAX_SNIPPET).collect::<String>() + "…";
    }
    json!({"title": title.trim(), "url": url.trim(), "snippet": snippet})
}

/// How to ask this backend for `query`.
pub fn build(backend: Backend, endpoint: &str, key: Option<&str>, query: &str, want: usize) -> Ask {
    let key = key.unwrap_or_default().to_string();
    match backend {
        Backend::Brave => Ask {
            post: false,
            url: format!("{endpoint}?q={}&count={want}", escape(query)),
            headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                ("X-Subscription-Token".to_string(), key),
            ],
            body: None,
        },
        Backend::Tavily => Ask {
            post: true,
            url: endpoint.to_string(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            // Tavily is the odd one: the key rides in the body, not a header
            body: Some(json!({"api_key": key, "query": query, "max_results": want}).to_string()),
        },
        Backend::Serper => Ask {
            post: true,
            url: endpoint.to_string(),
            headers: vec![
                ("X-API-KEY".to_string(), key),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: Some(json!({"q": query, "num": want}).to_string()),
        },
        Backend::Searxng => Ask {
            post: false,
            url: format!(
                "{}/search?q={}&format=json",
                endpoint.trim_end_matches('/'),
                escape(query)
            ),
            headers: vec![("Accept".to_string(), "application/json".to_string())],
            body: None,
        },
    }
}

/// Read this backend's answer. Every one of them buries the same three fields
/// under a different name; nothing else here is worth carrying forward.
pub fn read(backend: Backend, body: &Value, want: usize) -> Vec<Value> {
    let rows = match backend {
        Backend::Brave => body["web"]["results"].as_array(),
        Backend::Tavily | Backend::Searxng => body["results"].as_array(),
        Backend::Serper => body["organic"].as_array(),
    };
    let Some(rows) = rows else { return Vec::new() };
    rows.iter()
        .take(want)
        .map(|row| {
            let title = row["title"].as_str().unwrap_or_default();
            // Serper says `link`; everyone else says `url`
            let url = row["url"]
                .as_str()
                .or(row["link"].as_str())
                .unwrap_or_default();
            // Brave says `description`; Tavily and SearXNG say `content`;
            // Serper says `snippet`
            let snippet = row["description"]
                .as_str()
                .or(row["content"].as_str())
                .or(row["snippet"].as_str())
                .unwrap_or_default();
            hit(title, url, snippet)
        })
        .collect()
}

/// Some services answer the question as well as pointing at pages. When one
/// does, it is worth carrying: it is often all the model needed.
pub fn direct_answer(backend: Backend, body: &Value) -> Option<String> {
    match backend {
        Backend::Tavily => body["answer"].as_str().map(str::to_string),
        Backend::Serper => body["answerBox"]["answer"]
            .as_str()
            .or(body["answerBox"]["snippet"].as_str())
            .map(str::to_string),
        _ => None,
    }
}

pub fn tool_decls(network: &[String]) -> Vec<Value> {
    vec![json!({
        "name": TOOL,
        "description": "Search the web and return a list of results: title, url and a \
            short snippet of each. A result is a pointer, not the page — read one with \
            `Fetch` when the snippet is not enough.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "what to search for, in the \
                    words someone would use"},
                "limit": {"type": "integer", "description": "how many results (default 5)"},
            },
            "required": ["query"],
        },
        "effects": {"network": network},
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
        // The manifest is shared by every instance and the search service is
        // configuration, so the honest surface here is the widest one an
        // instance could have. A configured instance only ever narrows it.
        capabilities: Some(EffectSurface {
            network: vec!["*".to_string()],
            ..Default::default()
        }),
        implements: vec!["tool-provider".to_string()],
        tools: tool_decls(&["*".to_string()]),
        prompt: None,
        handle_timeout_ms: Some(30_000),
        // Several searches at once, like the other read-only tools.
        concurrency: Some(8),
    }
}

pub struct WebSearch {
    backend: Backend,
    endpoint: String,
    key: Option<String>,
    default_results: usize,
    exclusive: bool,
    client: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

impl WebSearch {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let backend = get("backend")
            .and_then(Value::as_str)
            .and_then(Backend::parse)
            .unwrap_or(Backend::Brave);
        let endpoint = get("endpoint")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| backend.default_endpoint().to_string());
        // The key is read from the ENVIRONMENT, never from the assembly: a
        // manifest is a document people commit, and a secret in it is a secret
        // published. Same rule the model adapters follow.
        let key_env = get("apiKeyEnv")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| backend.default_key_env().map(str::to_string));
        Self {
            backend,
            endpoint,
            key: key_env.and_then(|name| std::env::var(name).ok()),
            default_results: get("maxResults")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_RESULTS),
            exclusive: get("exclusive").and_then(Value::as_bool).unwrap_or(false),
            client: reqwest::Client::new(),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime always builds"),
        }
    }

    fn search(&self, query: &str, want: usize, ctx: &Ctx) -> Value {
        if self.endpoint.is_empty() {
            return err(
                "search.not_configured",
                &format!(
                    "the {} backend has no default address; give this instance an \
                     `endpoint`",
                    self.backend.name()
                ),
                "environment",
            );
        }
        if self.key.is_none() && self.backend.default_key_env().is_some() {
            // Named, so the answer is actionable rather than "it did not work"
            return err(
                "search.no_key",
                &format!(
                    "no API key for the {} backend; set {}",
                    self.backend.name(),
                    self.backend.default_key_env().unwrap_or("the key variable")
                ),
                "environment",
            );
        }

        let ask = build(
            self.backend,
            &self.endpoint,
            self.key.as_deref(),
            query,
            want,
        );
        let token = ctx.cancellation();
        let client = self.client.clone();
        let backend = self.backend;
        self.runtime.block_on(async move {
            let mut request = if ask.post {
                client.post(&ask.url)
            } else {
                client.get(&ask.url)
            };
            for (name, value) in &ask.headers {
                request = request.header(name, value);
            }
            if let Some(body) = ask.body {
                request = request.body(body);
            }
            let response = tokio::select! {
                _ = token.cancelled() => return json!({"status": "cancelled"}),
                outcome = request.send() => outcome,
            };
            let response = match response {
                Ok(response) => response,
                Err(e) => return err("search.network", &e.to_string(), "environment"),
            };
            let http_status = response.status().as_u16();
            let text = tokio::select! {
                _ = token.cancelled() => return json!({"status": "cancelled"}),
                text = response.text() => text,
            };
            let text = match text {
                Ok(text) => text,
                Err(e) => return err("search.network", &e.to_string(), "environment"),
            };
            if !(200..300).contains(&http_status) {
                // The service's own words, clipped: a 401 says "bad key" far
                // more usefully than anything invented here would.
                let mut detail: String = text.chars().take(300).collect();
                if detail.trim().is_empty() {
                    detail = format!("HTTP {http_status}");
                }
                return err(
                    "search.rejected",
                    &format!(
                        "the {} backend answered {http_status}: {detail}",
                        backend.name()
                    ),
                    "environment",
                );
            }
            let Ok(body) = serde_json::from_str::<Value>(&text) else {
                return err(
                    "search.bad_response",
                    &format!("the {} backend did not answer with JSON", backend.name()),
                    "environment",
                );
            };
            let results = read(backend, &body, want);
            let mut out = json!({"backend": backend.name(), "results": results});
            if let Some(answer) = direct_answer(backend, &body) {
                out["answer"] = json!(answer);
            }
            json!({"status": "ok", "result": out})
        })
    }
}

impl Component for WebSearch {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or_default();
        if tool != TOOL {
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
        let mut payload = match event.payload["arguments"]["query"].as_str() {
            Some(query) if !query.trim().is_empty() => {
                let want = event.payload["arguments"]["limit"]
                    .as_u64()
                    .map(|n| n as usize)
                    .unwrap_or(self.default_results)
                    .clamp(1, MAX_RESULTS);
                self.search(query, want, ctx)
            }
            _ => err("tool.bad_arguments", "missing 'query' argument", "request"),
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}

fn err(code: &str, message: &str, blame: &str) -> Value {
    // A transport failure is worth another try; a bad argument is not. The
    // judgment fields exist so the model can tell those apart without
    // reading prose, and answering "no" to both for everything made the
    // distinction useless. See `net_tools::err`.
    let transport = code == "search.network";
    json!({"status": "error", "error": {
        "code": code,
        "message": message,
        "blame": blame,
        "retryable": transport,
        "transient": transport,
    }})
}
