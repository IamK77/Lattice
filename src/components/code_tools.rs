//! Code navigation through explicitly configured local language servers.
//! File hashes guard subsequent reads; they are not workspace-index versions.
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};
use serde_json::{json, Value};

mod client;
mod source;
#[cfg(test)]
mod tests;
use client::Peer;
use source::Source;

pub const NAME: &str = "code-tools";

pub fn manifest() -> ComponentManifest {
    let effects = EffectSurface {
        reads: vec!["*".into()],
        writes: vec!["*".into()],
        network: vec!["*".into()],
        executes: true,
        reversible: false,
        ..Default::default()
    };
    ComponentManifest {
        name: NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: vec![],
        default_wiring: vec![],
        capabilities: Some(effects.clone()),
        implements: vec!["tool-provider".into()],
        tools: vec![json!({"name":"Code", "effects":effects,
            "description":"Navigate source using a local language server: symbols, read a named symbol, definition, or references. Workspace is the project root; path is relative to it or absolute. Input line and column are 1-based Unicode character positions; returned ranges are 0-based UTF-16 positions. No text-search substitutes. Missing servers fail explicitly; Lattice does not install them. Servers may build/index and use disk or network. Results identify observed file versions, but a server index is not an atomic workspace snapshot: inspect the original text before editing. Non-file virtual source URIs are not read. Use Read with expectedVersion to refuse changed files.",
            "parameters":{"type":"object","properties":{
                "workspace":{"type":"string"},"path":{"type":"string"},
                "action":{"type":"string","enum":["symbols","read","definition","references"]},
                "language":{"type":"string","description":"override extension detection with an LSP language id"},
                "symbol":{"type":"string","description":"exact symbol name or qualified name from symbols"},
                "line":{"type":"integer","minimum":1},"column":{"type":"integer","minimum":1},
                "expectedVersion":{"type":"string"},"offset":{"type":"integer","minimum":0},
                "limit":{"type":"integer","minimum":1,"maximum":100}
            },"required":["path"],"additionalProperties":false}
        })],
        prompt: None,
        handle_timeout_ms: Some(125_000),
        concurrency: Some(1),
    }
}

struct Session {
    key: (PathBuf, String),
    peer: Peer,
}

pub struct CodeTools {
    sessions: VecDeque<Session>,
    runtime: Option<tokio::runtime::Runtime>,
    config: Value,
}

impl CodeTools {
    pub fn from_config(config: Option<&Value>) -> Self {
        Self {
            sessions: VecDeque::new(),
            runtime: None,
            config: config.cloned().unwrap_or_else(|| json!({})),
        }
    }

    fn run(&mut self, args: &Value, ctx: &Ctx) -> Result<Value, String> {
        if ctx.cancelled() {
            return Err("code navigation cancelled before starting".into());
        }
        let action = validate_arguments(args)?;
        let root = PathBuf::from(args["workspace"].as_str().unwrap_or("."))
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !root.is_dir() {
            return Err("workspace must be a directory".into());
        }
        let path = args["path"].as_str().ok_or("path is required")?;
        let source = Source::load(&root.join(path))?;
        if !source.path.starts_with(&root) {
            return Err("query source must be inside the workspace".into());
        }
        if args["expectedVersion"]
            .as_str()
            .is_some_and(|hash| hash != source.version)
        {
            return Err("source differs from expectedVersion; locate it again".into());
        }
        let language = args["language"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| language(&source.path).map(str::to_owned))
            .ok_or("unknown source language; provide language explicitly")?;
        let server = self.config["servers"]
            .get(&language)
            .cloned()
            .unwrap_or_else(|| default_server(&language));
        let command: Vec<String> = server["command"]
            .as_array()
            .ok_or("configure a language server command array for this language")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("server command arguments must be strings")
            })
            .collect::<Result<_, _>>()?;
        let key = (root.clone(), server.to_string());
        if self.runtime.is_none() {
            self.runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?,
            );
        }
        let runtime = self.runtime.as_ref().unwrap();
        let mut session =
            if let Some(index) = self.sessions.iter().position(|entry| entry.key == key) {
                self.sessions.remove(index).unwrap()
            } else {
                while self.sessions.len() >= 2 {
                    if let Some(mut old) = self.sessions.pop_back() {
                        runtime.block_on(old.peer.stop());
                    }
                }
                let dir = super::tool_artifacts::directory(ctx.ledger_path())?;
                let _entered = runtime.enter();
                Session {
                    key,
                    peer: Peer::spawn(&command, &root, &server, &dir)?,
                }
            };
        let timeout = Duration::from_millis(
            self.config["timeoutMs"]
                .as_u64()
                .unwrap_or(30_000)
                .clamp(100, 120_000),
        );
        let cancellation = ctx.cancellation();
        let result = runtime.block_on(async {
            tokio::select! {
                result = query(&mut session.peer, &source, &language, action, args) => result,
                _ = tokio::time::sleep(timeout) => Err("language server deadline exceeded; peer stopped, no automatic retry".into()),
                _ = cancellation.cancelled() => Err("code navigation cancelled".into()),
            }
        });
        match result {
            Ok(mut result) => {
                result["serverLog"] = json!(session.peer.log);
                result["indexFreshness"] = json!("not-guaranteed");
                if result.to_string().len() > 65536 {
                    runtime.block_on(session.peer.stop());
                    return Err(
                        "navigation result with metadata exceeds 64 KiB; lower limit".into(),
                    );
                }
                self.sessions.push_front(session);
                Ok(result)
            }
            Err(error) => {
                runtime.block_on(session.peer.stop());
                Err(format!(
                    "{error}; language server log: {}",
                    session.peer.log.display()
                ))
            }
        }
    }
}

impl Drop for CodeTools {
    fn drop(&mut self) {
        if let Some(runtime) = &self.runtime {
            for mut session in self.sessions.drain(..) {
                runtime.block_on(session.peer.stop());
            }
        }
    }
}

impl Component for CodeTools {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if event.payload["tool"] != "Code" {
            return;
        }
        let args = &event.payload["arguments"];
        let result = validate_arguments(args)
            .map_err(|message| (message, "request"))
            .and_then(|_| {
                self.run(args, ctx)
                    .map_err(|message| (message, "environment"))
            });
        let mut payload = match result {
            Ok(result) => json!({"status":"ok","result":result}),
            Err((message, _)) if ctx.cancelled() => {
                json!({"status":"cancelled","result":{"note":message}})
            }
            Err((message, blame)) => {
                json!({"status":"error","error":{"code":"tool.code_navigation","message":message,"blame":blame,"retryable":false,"transient":false}})
            }
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}

fn language(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "py" | "pyi" => "python",
        "java" => "java",
        "cs" => "csharp",
        "go" => "go",
        "rs" => "rust",
        "c" => "c",
        "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "C" => "cpp",
        "php" => "php",
        "rb" | "rake" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        _ => return None,
    })
}

fn default_server(language: &str) -> Value {
    let command: &[&str] = match language {
        "rust" => &["rust-analyzer"],
        "javascript" | "javascriptreact" | "typescript" | "typescriptreact" => {
            &["typescript-language-server", "--stdio"]
        }
        "python" => &["pyright-langserver", "--stdio"],
        "go" => &["gopls"],
        "c" | "cpp" => &["clangd"],
        "csharp" => &["csharp-ls"],
        "php" => &["intelephense", "--stdio"],
        "swift" => &["sourcekit-lsp"],
        "kotlin" => &["kotlin-lsp", "--stdio"],
        // Running inside an existing bundle bypasses ruby-lsp's dependency
        // setup launcher. A missing bundle/server is an error, not an install.
        "ruby" => &["bundle", "exec", "ruby-lsp"],
        "java" => &[
            "jdtls",
            "-configuration",
            "{state}/configuration",
            "-data",
            "{state}/data",
        ],
        _ => return json!({}),
    };
    let mut server = json!({"command":command});
    if language == "java" {
        server["removeEnv"] = json!(["CLIENT_PORT", "CLIENT_HOST"]);
    }
    server
}

async fn query(
    peer: &mut Peer,
    source: &Source,
    language: &str,
    action: &str,
    args: &Value,
) -> Result<Value, String> {
    peer.initialize().await?;
    let capability = match action {
        "symbols" | "read" => "documentSymbolProvider",
        "definition" => "definitionProvider",
        _ => "referencesProvider",
    };
    if peer.capabilities[capability].is_null() || peer.capabilities[capability] == false {
        return Err(format!("language server does not provide {capability}"));
    }
    let uri = source.uri()?;
    // Do not leave our previous snapshots open as stale editor buffers.
    peer.close_other_documents(&uri).await?;
    peer.sync(source, language).await?;
    if matches!(action, "symbols" | "read") {
        let response = peer
            .request(
                "textDocument/documentSymbol",
                json!({"textDocument":{"uri":uri}}),
            )
            .await?;
        source.verify()?;
        return symbols(source, response, action, args);
    }
    let position = source.position(
        args["line"].as_u64().ok_or("line is required")?,
        args["column"].as_u64().ok_or("column is required")?,
    )?;
    let method = if action == "definition" {
        "textDocument/definition"
    } else {
        "textDocument/references"
    };
    let mut params = json!({"textDocument":{"uri":uri},"position":position});
    if action == "references" {
        params["context"] = json!({"includeDeclaration":true});
    }
    let first = location_rows(peer.request(method, params.clone()).await?);
    let (offset, limit) = page(args);
    let mut targets = BTreeMap::new();
    let mut bytes = 0;
    for row in first.iter().skip(offset).take(limit) {
        let Some(uri) = row["uri"].as_str() else {
            continue;
        };
        if targets.contains_key(uri) {
            continue;
        }
        let Some(path) = reqwest::Url::parse(uri)
            .ok()
            .and_then(|url| url.to_file_path().ok())
        else {
            continue;
        };
        let target = Source::load(&path)?;
        bytes += target.text.len();
        if targets.len() >= 16 || bytes > 16 * 1024 * 1024 {
            return Err("target snapshot budget exceeded; lower limit".into());
        }
        peer.sync(&target, self::language(&path).unwrap_or(language))
            .await?;
        targets.insert(uri.to_owned(), target);
    }
    let rows = location_rows(peer.request(method, params).await?);
    let mut selected = Vec::new();
    for mut row in rows.iter().skip(offset).take(limit).cloned() {
        let uri = row["uri"].as_str().ok_or("location has no URI")?;
        if let Some(target) = targets.get(uri) {
            let text = target.slice(&row["range"])?;
            row["path"] = json!(target.path);
            row["observedFileVersion"] = json!(target.version);
            row["read"] = target.read_from(target.offset(&row["range"]["start"])?);
            row["text"] = json!(text.chars().take(400).collect::<String>());
            row["textTruncated"] = json!(text.chars().take(401).count() > 400);
        } else if reqwest::Url::parse(uri).is_ok_and(|u| u.scheme() == "file") {
            return Err("navigation targets changed while synchronizing; retry rather than using unbound positions".into());
        } else {
            row["available"] = json!(false);
            row["reason"] = json!("virtual source URI is not a readable local file");
        }
        selected.push(row);
    }
    for target in targets.values() {
        target.verify()?;
    }
    source.verify()?;
    let mut result = json!({"path":source.path,"fileVersion":source.version,"positionEncoding":"utf-16","locations":selected,"indexFreshness":"not-guaranteed","more":offset.saturating_add(limit)<rows.len()});
    if offset.saturating_add(limit) < rows.len() {
        result["next"] =
            json!({"offset":offset.saturating_add(limit),"expectedVersion":source.version});
    }
    if result.to_string().len() > 65536 {
        return Err("navigation result exceeds 64 KiB; lower limit".into());
    }
    Ok(result)
}

fn validate_arguments(args: &Value) -> Result<&str, String> {
    if !args.is_object() {
        return Err("arguments must be an object".into());
    }
    for name in [
        "path",
        "workspace",
        "language",
        "action",
        "symbol",
        "expectedVersion",
    ] {
        if args
            .get(name)
            .is_some_and(|value| value.as_str().is_none_or(str::is_empty))
        {
            return Err(format!("{name} must be a nonempty string"));
        }
    }
    if args.get("path").is_none() {
        return Err("path is required".into());
    }
    for (name, minimum, maximum) in [
        ("line", 1, u64::MAX),
        ("column", 1, u64::MAX),
        ("offset", 0, usize::MAX as u64),
        ("limit", 1, 100),
    ] {
        if args
            .get(name)
            .is_some_and(|value| value.as_u64().is_none_or(|n| n < minimum || n > maximum))
        {
            return Err(format!(
                "{name} must be an integer between {minimum} and {maximum}"
            ));
        }
    }
    let action = args["action"].as_str().unwrap_or("symbols");
    match action {
        "symbols" => (),
        "read" if args.get("symbol").is_some() => (),
        "read" => return Err("symbol is required for read".into()),
        "definition" | "references"
            if args.get("line").is_some() && args.get("column").is_some() => {}
        "definition" | "references" => {
            return Err("line and column are required for navigation".into())
        }
        _ => return Err("unknown code action".into()),
    }
    Ok(action)
}

fn page(args: &Value) -> (usize, usize) {
    (
        args["offset"].as_u64().unwrap_or(0) as usize,
        args["limit"].as_u64().unwrap_or(50).clamp(1, 100) as usize,
    )
}

fn location_rows(value: Value) -> Vec<Value> {
    let mut rows: Vec<_> = match value {
        Value::Array(rows) => rows,
        Value::Null => vec![],
        row => vec![row],
    }
    .into_iter()
    .map(|row| {
        if !row["targetUri"].is_null() {
            json!({"uri":row["targetUri"],"range":row["targetSelectionRange"]})
        } else {
            json!({"uri":row["uri"],"range":row["range"]})
        }
    })
    .collect();
    rows.sort_by(|a, b| {
        let key = |row: &Value| {
            (
                row["range"]["start"]["line"].as_u64(),
                row["range"]["start"]["character"].as_u64(),
                row["range"]["end"]["line"].as_u64(),
                row["range"]["end"]["character"].as_u64(),
            )
        };
        a["uri"]
            .as_str()
            .cmp(&b["uri"].as_str())
            .then_with(|| key(a).cmp(&key(b)))
    });
    rows.dedup();
    rows
}

fn symbols(source: &Source, response: Value, action: &str, args: &Value) -> Result<Value, String> {
    let values = match response {
        Value::Array(rows) => rows,
        Value::Null => Vec::new(),
        _ => return Err("documentSymbol response must be an array or null".into()),
    };
    let mut pending: Vec<_> = values.into_iter().map(|row| (String::new(), row)).collect();
    let mut rows = Vec::new();
    let mut outline_bytes = 0usize;
    while let Some((prefix, mut row)) = pending.pop() {
        if rows.len() >= 10_000 {
            return Err("symbol index exceeds the bounded outline limit".into());
        }
        let name = row["name"].as_str().ok_or("symbol has no name")?;
        if prefix.len() + name.len() + 2 > 4096 {
            return Err("qualified symbol name exceeds 4 KiB".into());
        }
        let name = name.to_owned();
        let qualified = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}::{name}")
        };
        let children = match row.get_mut("children").map(Value::take) {
            Some(Value::Array(children)) => children,
            None | Some(Value::Null) => Vec::new(),
            _ => return Err("symbol children must be an array".into()),
        };
        outline_bytes =
            outline_bytes.saturating_add(qualified.len().saturating_mul(children.len() + 1));
        if outline_bytes > 2 * 1024 * 1024 || rows.len() + pending.len() + children.len() >= 10_000
        {
            return Err("symbol index exceeds the bounded outline limit".into());
        }
        pending.extend(children.into_iter().map(|child| (qualified.clone(), child)));
        let full_range = row.get("range").cloned();
        if full_range.is_none() {
            let same_file = row["location"]["uri"]
                .as_str()
                .and_then(|uri| reqwest::Url::parse(uri).ok())
                .and_then(|uri| uri.to_file_path().ok())
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| path == source.path);
            if !same_file {
                return Err("symbol location belongs to another document".into());
            }
        }
        let range = full_range
            .clone()
            .unwrap_or_else(|| row["location"]["range"].clone());
        source.slice(&range)?;
        rows.push(json!({"name":name,"qualified":qualified,"kind":row["kind"],"range":range,"fullRange":full_range.is_some()}));
    }
    rows.sort_by_key(|row| {
        (
            row["range"]["start"]["line"].as_u64(),
            row["range"]["start"]["character"].as_u64(),
        )
    });
    if action == "read" {
        let name = args["symbol"]
            .as_str()
            .ok_or("symbol is required for read")?;
        let matches: Vec<_> = rows
            .iter()
            .filter(|row| {
                (row["name"] == name || row["qualified"] == name)
                    && args["line"].as_u64().is_none_or(|line| {
                        row["range"]["start"]["line"]
                            .as_u64()
                            .is_some_and(|n| n + 1 == line)
                    })
            })
            .collect();
        if matches.len() != 1 {
            return Err(
                "symbol missing or ambiguous; inspect symbols and specify its starting line".into(),
            );
        }
        let row = matches[0];
        if row["fullRange"] != true {
            return Err(
                "server did not provide a full symbol range; refusing to guess a function body"
                    .into(),
            );
        }
        let text = source.slice(&row["range"])?;
        let mut end = text.len().min(48 * 1024);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let start = source.offset(&row["range"]["start"])?;
        let mut result = json!({"path":source.path,"fileVersion":source.version,"symbol":row,"positionEncoding":"utf-16","content":&text[..end],"truncated":end<text.len(),"read":source.read_from(start)});
        loop {
            let bytes = result.to_string().len();
            if bytes <= 65536 {
                break;
            }
            if end == 0 {
                return Err("symbol metadata exceeds 64 KiB".into());
            }
            // Escaped JSON bytes are not source bytes: subtracting their
            // difference can discard the entire preview for control characters.
            end /= 2;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            result["content"] = json!(&text[..end]);
            result["truncated"] = json!(true);
        }
        Ok(result)
    } else {
        symbol_page(source, rows, args)
    }
}

fn symbol_page(source: &Source, rows: Vec<Value>, args: &Value) -> Result<Value, String> {
    let (offset, limit) = page(args);
    let mut result = json!({"path":source.path,"fileVersion":source.version,"positionEncoding":"utf-16","symbols":rows.iter().skip(offset).take(limit).collect::<Vec<_>>(),"more":offset.saturating_add(limit)<rows.len()});
    if offset.saturating_add(limit) < rows.len() {
        result["next"] =
            json!({"offset":offset.saturating_add(limit),"expectedVersion":source.version});
    }
    if result.to_string().len() > 65536 {
        return Err("symbol result exceeds 64 KiB; lower limit".into());
    }
    Ok(result)
}
