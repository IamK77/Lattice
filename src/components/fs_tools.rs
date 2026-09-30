//! File tools — the first real-effect toolset: read / write / edit / ls. This
//! is where the effect surface stops being a demo: read tools declare `reads`,
//! the write tools declare `writes`, so an effects-policy gate on the tool wire
//! judges them by what they touch — a real door, not a test fixture.
//!
//! Two modes, chosen by whether the instance config carries a `root`:
//!
//! OPEN (no root, the default): paths are taken as given — absolute, or
//! relative to this process's working directory — and the surface says `*`,
//! which is then literally true. This is the default because the alternative
//! was never the boundary it looked like: `Run` sits on the same wire with
//! `executes` and `*` already, so confining only the narrow, auditable tools
//! bought no safety and cost the agent the ability to look at the project it
//! was started in.
//!
//! CONFINED (a root is configured): every path is relative to that root;
//! absolute paths and `..` escapes are refused outright, and a resolved path
//! whose real location (following symlinks) leaves the root is refused too.
//! For assemblies that mean it — and those want `Run` off the wire as well,
//! since no path check here constrains a shell.

use std::io::BufRead;
use std::path::{Component as PathComponent, Path, PathBuf};

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};

pub const READER: &str = "fs-reader";
pub const WRITER: &str = "fs-writer";

/// The tool declarations, effect surfaces included so a policy can judge
/// them. `None` means unconfined, and then the surface says `*` — the
/// declaration has to widen with the reach, or the gate is judging a fiction.
pub fn read_decls(root: Option<&str>) -> Vec<Value> {
    let scope = root.unwrap_or("*");
    let where_paths = match root {
        Some(_) => "relative to the tool root (absolute paths and .. are refused)",
        None => "absolute, or relative to the working directory",
    };
    vec![
        json!({
            "name": "Read",
            "description": format!("Read a UTF-8 text file, path {where_paths}. Long files \
                come back a page at a time: `from` is the first line (1-based, default 1) \
                and `limit` how many. The result says whether more follows and which line \
                to continue from, so a file too big to hold is still readable in full. \
                A single line too long to send is cut: the result then reports the line's \
                full size and a `nextByte` to continue that same line from. Opt into \
                versioned=true for a content hash and readKey (files up to 8 MiB). \
                expectedVersion refuses stale positions. Repeating a range with knownRead \
                revalidates it and sends a short receipt only if unchanged; omit knownRead \
                whenever the previous content is no longer in context."),
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "versioned": {"type": "boolean"},
                    "expectedVersion": {"type": "string", "description": "exact fileVersion from a prior location or read"},
                    "knownRead": {"type": "string", "description": "prior readKey for this range; only pass while its content remains available"},
                    "from": {"type": "integer", "description": "first line, 1-based"},
                    "limit": {"type": "integer", "description": "how many lines"},
                    "fromByte": {
                        "type": "integer",
                        "description": "start this many bytes into the first line; \
                            use the nextByte a cut page reported",
                    },
                },
                "required": ["path"],
            },
            "effects": {"reads": [scope], "reversible": true},
        }),
        json!({
            "name": "Ls",
            "description": format!("List the entries of a directory, path {where_paths}."),
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
            "effects": {"reads": [scope], "reversible": true},
        }),
    ]
}

pub fn write_decls(root: Option<&str>) -> Vec<Value> {
    let scope = root.unwrap_or("*");
    let where_paths = match root {
        Some(_) => "relative to the tool root (absolute paths and .. are refused)",
        None => "absolute, or relative to the working directory",
    };
    vec![
        json!({
            "name": "Write",
            "description": format!("Write a whole UTF-8 text file, path {where_paths}, \
                creating parent directories as needed. The result says whether the file \
                was created or overwritten, and how much it replaced. To change PART of \
                an existing file, prefer `Edit` — it costs a fraction as much and cannot \
                lose the parts you did not mean to touch."),
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                "required": ["path", "content"],
            },
            // No `reversible` flag: overwriting cannot be undone
            "effects": {"writes": [scope]},
        }),
        json!({
            "name": "Edit",
            "description": format!("Replace an exact piece of text in a file, path \
                {where_paths}. `old` must appear EXACTLY ONCE — if it appears never or more than \
                once the edit is refused and nothing changes, so include enough \
                surrounding text to be unambiguous. Pass all=true to replace every \
                occurrence deliberately."),
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old": {"type": "string", "description": "exact text to replace"},
                    "new": {"type": "string", "description": "what to put there"},
                    "all": {"type": "boolean", "description": "replace every occurrence"},
                },
                "required": ["path", "old", "new"],
            },
            // Same surface as write: it changes a file, and cannot be undone
            "effects": {"writes": [scope]},
        }),
    ]
}

pub fn reader_manifest() -> ComponentManifest {
    file_manifest(
        READER,
        read_decls(None),
        EffectSurface {
            reads: vec!["*".into()],
            reversible: true,
            ..Default::default()
        },
        "Carry a returned file version into expectedVersion when following a location; \
         if it is stale, locate again. Pass knownRead only while the earlier content \
         is still in context; after context loss, read the body again. For a condensed \
         or truncated preview, follow the returned paths to saved logs or documents, \
         or its audit-event pointer, and check completeness rather than assuming the \
         preview is the whole result.",
    )
}

pub fn writer_manifest() -> ComponentManifest {
    file_manifest(
        WRITER,
        write_decls(None),
        EffectSurface {
            reads: vec!["*".into()],
            writes: vec!["*".into()],
            ..Default::default()
        },
        "Read a file before you edit it — `Edit` matches the text that is there \
         now, not the text you remember writing.",
    )
}

fn file_manifest(
    name: &str,
    tools: Vec<Value>,
    effects: EffectSurface,
    prompt: &str,
) -> ComponentManifest {
    ComponentManifest {
        name: name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{name}"),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        // Static declarations cover the default reach; configured roots narrow it.
        capabilities: Some(effects),
        implements: vec!["tool-provider".to_string()],
        tools,
        prompt: Some(prompt.to_string()),
        handle_timeout_ms: Some(10_000),
        // Several at once: a model that asks for three files in one turn
        // means them read together, not one after another. Safe here because
        // nothing is kept between deliveries.
        concurrency: Some(8),
    }
}

pub struct FsReader(FileAccess);
pub struct FsWriter(FileAccess);

impl FsReader {
    pub fn from_config(config: Option<&Value>) -> Self {
        Self(FileAccess::from_config(config))
    }
}
impl FsWriter {
    pub fn from_config(config: Option<&Value>) -> Self {
        Self(FileAccess::from_config(config))
    }
}

struct FileAccess {
    /// `None` = unconfined: paths are taken as given. `Some` = every path must
    /// resolve inside this directory.
    root: Option<PathBuf>,
    /// Largest file this tool will WRITE in one call. A guard against a
    /// runaway edit, so it is sized for real source files.
    max_bytes: usize,
    /// Largest page this tool will RETURN from a read. A context budget, and
    /// a much smaller number than the one above: these were one field, which
    /// meant the size a file may be written at also decided how much of a
    /// file lands in the conversation. Lowering the shared number would have
    /// refused legitimate writes; leaving it let one `Read` spend a large
    /// part of the window. They are separate concerns and now separate
    /// numbers. Over the budget the page stops early and says `more: true`,
    /// so the caller pages on with `from`.
    max_read_bytes: usize,
    /// Under fan-out wiring, answer only our own tools (false) or error on
    /// foreign ones (true)
    exclusive: bool,
}

/// The largest char boundary at or below `at` — a cut inside a multi-byte
/// character would produce a string that is not text at all.
fn clamped_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

impl FileAccess {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        Self {
            root: get("root").and_then(Value::as_str).map(PathBuf::from),
            max_bytes: get("maxBytes").and_then(Value::as_u64).unwrap_or(1_048_576) as usize,
            max_read_bytes: get("maxReadBytes")
                .and_then(Value::as_u64)
                .unwrap_or(16_384) as usize,
            exclusive: get("exclusive").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// Resolve a request path: unconfined, take it as given; confined,
    /// resolve it against the root and refuse every escape.
    fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        let Some(root) = self.root.clone() else {
            return Ok(PathBuf::from(rel));
        };
        let rel = Path::new(rel);
        let mut resolved = root.clone();
        for component in rel.components() {
            match component {
                PathComponent::Normal(part) => resolved.push(part),
                PathComponent::CurDir => {}
                PathComponent::ParentDir => {
                    return Err("path may not climb out of the root with ..".to_string())
                }
                PathComponent::RootDir | PathComponent::Prefix(_) => {
                    return Err("path must be relative to the tool root".to_string())
                }
            }
        }
        // Symlink guard: the real location of the nearest existing ancestor
        // must still sit inside the real root
        let real_root = root
            .canonicalize()
            .map_err(|e| format!("tool root is unavailable: {e}"))?;
        let mut probe = resolved.as_path();
        loop {
            if let Ok(real) = probe.canonicalize() {
                if !real.starts_with(&real_root) {
                    return Err("path resolves outside the tool root".to_string());
                }
                break;
            }
            match probe.parent() {
                Some(parent) => probe = parent,
                None => break,
            }
        }
        Ok(resolved)
    }

    fn read(&self, tool: &str, args: &Value) -> Value {
        let path = match args["path"].as_str() {
            Some(path) => path,
            None => return err("tool.bad_arguments", "missing 'path' argument"),
        };
        let resolved = match self.resolve(path) {
            Ok(resolved) => resolved,
            Err(why) => return err("tool.path_refused", &why),
        };
        match tool {
            "Read" => self.read_page(path, &resolved, args),
            "Ls" => match std::fs::read_dir(&resolved) {
                Ok(entries) => {
                    let mut names: Vec<Value> = Vec::new();
                    for entry in entries.flatten() {
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        names.push(json!({
                            "name": entry.file_name().to_string_lossy(),
                            "dir": is_dir,
                        }));
                    }
                    json!({"status": "ok", "result": {"path": path, "entries": names}})
                }
                Err(e) => io_err(&e),
            },
            _ => err("tool.unknown", &format!("unknown read tool: {tool}")),
        }
    }

    fn write(&self, tool: &str, args: &Value) -> Value {
        let path = match args["path"].as_str() {
            Some(path) => path,
            None => return err("tool.bad_arguments", "missing 'path' argument"),
        };
        let resolved = match self.resolve(path) {
            Ok(resolved) => resolved,
            Err(why) => return err("tool.path_refused", &why),
        };
        match tool {
            "Write" => {
                let content = args["content"].as_str().unwrap_or("");
                if content.len() > self.max_bytes {
                    return err(
                        "tool.too_large",
                        &format!(
                            "content is {} bytes; limit is {}",
                            content.len(),
                            self.max_bytes
                        ),
                    );
                }
                // What this call DESTROYED belongs on the record as much as what
                // it wrote — "overwrote 900 bytes" and "created" are different
                // events, and afterwards nothing can tell them apart.
                let replaced = std::fs::metadata(&resolved).map(|m| m.len()).ok();
                if let Some(parent) = resolved.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return io_err(&e);
                    }
                }
                match std::fs::write(&resolved, content) {
                    Ok(()) => json!({"status": "ok", "modelText": json!({
                        "path": path, "bytes": content.len(), "created": replaced.is_none(),
                        "replacedBytes": replaced,
                    }).to_string(), "result": {
                        "path": path,
                        // What this call CHANGED, said by the only party that
                        // knows for sure. A reader skimming a finished turn
                        // asks one question first — did anything change, and
                        // what — and nothing downstream should have to guess
                        // the answer from a tool's name or its arguments.
                        "changed": path,
                        "bytes": content.len(),
                        "created": replaced.is_none(),
                        "replacedBytes": replaced,
                        // What went in, in brief. A write reported only as a
                        // byte count is unreadable afterwards — neither the
                        // human watching nor the agent re-reading its own
                        // trace can tell what the file now says.
                        "preview": head(content, PREVIEW_LINES),
                    }}),
                    Err(e) => io_err(&e),
                }
            }
            "Edit" => self.edit(path, &resolved, args),
            _ => err("tool.unknown", &format!("unknown tool: {tool}")),
        }
    }

    /// One page of a file: `from` lines in, `limit` lines long. Reads line by
    /// line, so a file far larger than memory is still readable — and refusing
    /// a big file outright (what this used to do) left the agent with nothing
    /// at all, which is worse than a page and a way to ask for the next.
    fn read_page(&self, path: &str, resolved: &Path, args: &Value) -> Value {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let file = match std::fs::File::open(resolved) {
            Ok(file) => file,
            Err(e) => return io_err(&e),
        };
        if args["versioned"] != true
            && args["expectedVersion"].is_null()
            && args["knownRead"].is_null()
        {
            return self.read_page_from(path, args, file);
        }
        // Opt-in snapshots are bounded; ordinary ledger paging never hashes
        // or buffers the entire file. Positions and content use these same bytes.
        const MAX_SNAPSHOT: u64 = 8 * 1024 * 1024;
        let mut bytes = Vec::new();
        if let Err(e) = file.take(MAX_SNAPSHOT + 1).read_to_end(&mut bytes) {
            return io_err(&e);
        }
        if bytes.len() as u64 > MAX_SNAPSHOT {
            return err(
                "tool.too_large",
                "versioned reads are limited to 8 MiB; use ordinary Read paging for larger files",
            );
        }
        let version = format!("sha256:{:x}", Sha256::digest(&bytes));
        if args["expectedVersion"]
            .as_str()
            .is_some_and(|expected| expected != version)
        {
            return err(
                "tool.stale_version",
                "file content differs from expectedVersion; locate and read it again",
            );
        }
        let mut payload = self.read_page_from(path, args, std::io::Cursor::new(bytes));
        if payload["status"] != "ok" {
            return payload;
        }
        payload["result"]["fileVersion"] = json!(version);
        let read_key = format!(
            "sha256:{:x}",
            Sha256::digest(payload["result"].to_string().as_bytes())
        );
        payload["result"]["readKey"] = json!(read_key);
        if args["knownRead"].as_str() == Some(read_key.as_str()) {
            let mut receipt = payload["result"].clone();
            receipt.as_object_mut().unwrap().remove("content");
            receipt["unchanged"] = json!(true);
            payload["modelText"] = json!(receipt.to_string());
        }
        payload
    }

    fn read_page_from(&self, path: &str, args: &Value, file: impl std::io::Read) -> Value {
        const DEFAULT_LIMIT: usize = 2000;
        let from = args["from"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = args["limit"]
            .as_u64()
            .unwrap_or(DEFAULT_LIMIT as u64)
            .max(1) as usize;
        let from_byte = args["fromByte"].as_u64().unwrap_or(0) as usize;
        let mut reader = std::io::BufReader::new(file);
        let mut content = String::new();
        let mut line = String::new();
        let mut taken = 0usize;
        let mut line_no = 0usize;
        let mut more = false;
        loop {
            line.clear();
            // read_line, not lines(): it KEEPS the terminator, so a file whose
            // last line has no newline reads back byte for byte. Adding one
            // would mean write-then-read did not return what was written.
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                // Not text: say so plainly rather than handing back mojibake
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    return err("tool.not_text", "file is not valid UTF-8 text")
                }
                Err(e) => return io_err(&e),
            }
            line_no += 1;
            if line_no < from {
                continue;
            }
            // The first line may be resumed part-way through, which is how a
            // line too long to send in one piece is read to the end.
            let piece = if taken == 0 && from_byte > 0 {
                let rest: &str = line
                    .get(clamped_boundary(&line, from_byte)..)
                    .unwrap_or_default();
                rest
            } else {
                &line
            };
            // A page also stops at the byte ceiling: one enormous line must
            // not become an enormous event
            if taken == limit || (taken > 0 && content.len() + piece.len() > self.max_read_bytes) {
                more = true;
                break;
            }
            // One line, on its own, longer than a whole page. Sending it whole
            // was the hole this closes: a ledger event carrying a command's
            // output, or a minified bundle, is a single line of any size at
            // all, and the ceiling above only ever looked at the SECOND line
            // onward. Cut it, say so, and say where to pick it up.
            if piece.len() > self.max_read_bytes {
                let cut = clamped_boundary(piece, self.max_read_bytes);
                content.push_str(&piece[..cut]);
                let start = if taken == 0 { from_byte } else { 0 };
                return json!({"status": "ok", "result": {
                    "path": path,
                    "content": content,
                    "from": from,
                    "to": line_no,
                    "more": true,
                    "cut": {
                        "line": line_no,
                        "bytes": line.len(),
                        "shown": [start, start + cut],
                    },
                    "nextByte": start + cut,
                }});
            }
            content.push_str(piece);
            taken += 1;
        }
        let mut result = json!({
            "path": path,
            "content": content,
            "from": from,
            "to": from + taken.saturating_sub(1),
            "more": more,
        });
        if more {
            result["next"] = json!(from + taken);
        }
        json!({"status": "ok", "result": result})
    }

    /// Replace an exact piece of text. Ambiguity is refused rather than
    /// guessed: a pattern matching twice means the caller does not yet know
    /// which one it meant, and editing the wrong one is silent damage.
    fn edit(&self, path: &str, resolved: &Path, args: &Value) -> Value {
        let (Some(old), Some(new)) = (args["old"].as_str(), args["new"].as_str()) else {
            return err("tool.bad_arguments", "edit needs 'old' and 'new'");
        };
        if old.is_empty() {
            return err("tool.bad_arguments", "'old' may not be empty");
        }
        let text = match std::fs::read_to_string(resolved) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return err("tool.not_text", "file is not valid UTF-8 text")
            }
            Err(e) => return io_err(&e),
        };
        let hits = text.matches(old).count();
        let all = args["all"].as_bool().unwrap_or(false);
        match hits {
            0 => return err("tool.no_match", "that text does not appear in the file"),
            n if n > 1 && !all => {
                return err(
                    "tool.ambiguous",
                    &format!(
                        "that text appears {n} times; include more surrounding text, \
                         or pass all=true to replace every one"
                    ),
                )
            }
            _ => {}
        }
        let edited = if all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        };
        if edited.len() > self.max_bytes {
            return err(
                "tool.too_large",
                &format!(
                    "the edited file would be {} bytes; limit is {}",
                    edited.len(),
                    self.max_bytes
                ),
            );
        }
        match std::fs::write(resolved, &edited) {
            Ok(()) => json!({"status": "ok", "modelText": json!({
                "path": path, "replaced": if all { hits } else { 1 }, "bytes": edited.len(),
            }).to_string(), "result": {
                "path": path,
                "changed": path,
                "replaced": if all { hits } else { 1 },
                "bytes": edited.len(),
                // The edit IS its own diff: what left and what arrived. No
                // diff algorithm needed — the caller named both sides.
                "preview": diff(old, new),
                "editDiff": crate::edit_diff::EditDiff::between(&text, &edited),
            }}),
            Err(e) => io_err(&e),
        }
    }
}

/// How many lines of a written file, or of either side of an edit, ride back
/// on the result. Enough to recognise what happened, not enough to become a
/// second copy of the file on the ledger.
const PREVIEW_LINES: usize = 6;

/// The first `n` lines of a text, saying how many were left out.
fn head(text: &str, n: usize) -> String {
    let total = text.lines().count();
    let mut out: String = text.lines().take(n).collect::<Vec<_>>().join("\n");
    if total > n {
        out.push_str(&format!("\n… (+{} lines)", total - n));
    }
    out
}

/// An edit rendered as what it removed and what it added.
fn diff(old: &str, new: &str) -> String {
    let side = |marker: char, text: &str| -> String {
        let total = text.lines().count();
        let mut lines: Vec<String> = text
            .lines()
            .take(PREVIEW_LINES)
            .map(|l| format!("{marker} {l}"))
            .collect();
        if total > PREVIEW_LINES {
            lines.push(format!("{marker} … (+{} lines)", total - PREVIEW_LINES));
        }
        lines.join("\n")
    };
    format!("{}\n{}", side('-', old), side('+', new))
}

fn err(code: &str, message: &str) -> Value {
    json!({"status": "error", "error": {
        "code": code,
        "message": message,
        "blame": "request",
        "retryable": false,
        "transient": false,
    }})
}

fn io_err(e: &std::io::Error) -> Value {
    // Some IO failures are a fact about the world right now — the file was
    // locked, the descriptor table was full, the read was interrupted — and
    // those are worth another try. A missing file or a refused permission is
    // not: it will fail the same way twice, and telling the model otherwise
    // buys nothing but a second identical error.
    let again = matches!(
        e.kind(),
        std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::ResourceBusy
    );
    json!({"status": "error", "error": {
        "code": "tool.io",
        "message": e.to_string(),
        "blame": "environment",
        "retryable": again,
        "transient": again,
    }})
}

impl Component for FsReader {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        let payload = match tool {
            "Read" | "Ls" => self.0.read(tool, &event.payload["arguments"]),
            _ if self.0.exclusive => err("tool.unknown", &format!("unknown read tool: {tool}")),
            _ => return,
        };
        complete(event, ctx, payload);
    }
}

impl Component for FsWriter {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        let payload = match tool {
            "Write" | "Edit" => self.0.write(tool, &event.payload["arguments"]),
            _ if self.0.exclusive => err("tool.unknown", &format!("unknown write tool: {tool}")),
            _ => return,
        };
        complete(event, ctx, payload);
    }
}

fn complete(event: &EventEnvelope, ctx: &mut Ctx, mut payload: Value) {
    // A receipt is optional; never reject a completed write for an oversized receipt.
    if payload["modelText"]
        .as_str()
        .is_some_and(|text| text.len() > 8192)
    {
        payload.as_object_mut().unwrap().remove("modelText");
    }
    payload["call"] = event.payload["call"].clone();
    ctx.emit(
        "outcome",
        EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
    );
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn reuse_checks_the_whole_snapshot_and_the_exact_returned_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("code.rs");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let tools = FileAccess::from_config(None);
        let first = tools.read_page("code.rs", &path, &json!({"versioned": true, "limit": 1}));
        assert_eq!(first["result"]["content"], "one\n");
        let args = json!({"limit": 1, "knownRead": first["result"]["readKey"]});
        let same = tools.read_page("code.rs", &path, &args);
        assert!(same["modelText"].as_str().unwrap().contains("unchanged"));
        assert_eq!(
            same["result"]["content"], "one\n",
            "the audit retains the original bytes"
        );
        let other_range = tools.read_page(
            "code.rs",
            &path,
            &json!({"from": 2, "limit": 1, "knownRead": first["result"]["readKey"]}),
        );
        assert!(other_range["modelText"].is_null());
        std::fs::write(&path, "one\nnew\n").unwrap();
        let changed = tools.read_page("code.rs", &path, &args);
        assert!(
            changed["modelText"].is_null(),
            "an unseen change also invalidates the file version"
        );
        let stale = tools.read_page(
            "code.rs",
            &path,
            &json!({"expectedVersion": first["result"]["fileVersion"]}),
        );
        assert_eq!(stale["error"]["code"], "tool.stale_version");
        let plain = tools.read_page("code.rs", &path, &json!({}));
        assert!(plain["result"]["fileVersion"].is_null());
    }
}
