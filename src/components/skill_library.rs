//! The skill library: on-demand procedural knowledge for the model, aligned
//! with the open Agent Skills standard (agentskills.io). A skill is a folder
//! whose SKILL.md carries YAML frontmatter (canon: schemas/skill_frontmatter.json)
//! followed by markdown instructions; the folder may bundle reference files
//! and scripts.
//!
//! Three-tier loading: the resident cost is one listing line per skill (the
//! assembly generator computes it via [`listing_prompt`] and carries it as
//! this instance's prompt override); the body enters context only when
//! load_skill is called; bundled files only when fetched by name. Scripts are
//! run by the model through the ordinary Run tool, so the effects policy
//! governs them like any other execution — the `allowed-tools` frontmatter
//! field is accepted for compatibility and deliberately ignored.
//!
//! Skills are resolved from the configured directories on every call (first
//! directory wins on a name clash), so a folder dropped in mid-session is
//! loadable immediately; only the resident listing waits for a restart.
//! install_skill fetches from a local path or a git URL, validates against
//! the canon before anything lands, and records a reasoned decision event.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::contracts::component::{
    ComponentManifest, EffectSurface, PortDecl, RuntimeKind, WireSuggestion,
};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "skill-library";

/// Decision event recorded for every installation (reason mandatory).
pub const SKILL_INSTALLED: &str = "skill.installed";

/// The invokable-skills menu, said to the ledger whenever it changes: name +
/// description per usable skill. Frontends fold it into their `/` palette —
/// late clients get it in the replay, a hot-installed library announces it
/// the moment it arrives. Emitted only when different from the last listing
/// on the ledger (a process-hosted library cannot see the ledger and
/// re-announces each boot — the honest degradation).
pub const SKILL_LISTING: &str = "skill.listing";

/// Default scan order: Lattice's own directory first (also the install
/// target), then the ecosystem's shared conventions, so skills installed by
/// other tools are visible here too. First directory wins on a name clash.
pub fn default_dirs() -> Vec<String> {
    vec![
        "./.lattice/skills".to_string(),
        "./.agents/skills".to_string(),
        "./.claude/skills".to_string(),
    ]
}

fn frontmatter_canon() -> &'static jsonschema::Validator {
    static CANON: OnceLock<jsonschema::Validator> = OnceLock::new();
    CANON.get_or_init(|| {
        let schema: Value =
            serde_json::from_str(include_str!("../../schemas/skill_frontmatter.json"))
                .expect("canon schema files are valid JSON");
        jsonschema::validator_for(&schema).expect("canon schema files compile")
    })
}

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![
            PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED]),
            // A "my folders changed" wake comes back here (see the `changed`
            // output): rescan and refresh the listing fragment — no model turn
            PortDecl::new("refresh", &[ce::WAKE]),
            // The expansion station on the user-input line: a message that
            // begins with /skill-name is expanded (the skill body replaces
            // it, arguments substituted) and re-emitted on `expanded`; any
            // other message is forwarded untouched. The user already decided
            // — no model round-trip, and both the original and the expansion
            // are on the ledger, causally linked.
            PortDecl::new("input", &[ce::USER_MESSAGE]),
        ],
        outputs: vec![
            PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED]),
            // Unwired by default: installations land on the ledger for audit,
            // they are not routed anywhere
            PortDecl::new("audit", &[SKILL_INSTALLED, SKILL_LISTING]),
            // The standing watch on the skill folders fires here; wire it
            // back to `refresh` (a legal ring) so a folder dropped in by hand
            // is sensed without a restart and without costing a model call
            PortDecl::new("changed", &[ce::WAKE]),
            // The far side of the expansion station (see `input`)
            PortDecl::new("expanded", &[ce::USER_MESSAGE]),
        ],
        events: vec![
            EventTypeDecl::decision(SKILL_INSTALLED, "A skill was installed into the library")
                .with_schema(json!({
                    "type": "object",
                    "required": ["name", "source"],
                    "properties": {
                        "name": {"type": "string"},
                        "source": {"type": "string"},
                        "sha256": {"type": "string"},
                    },
                })),
            EventTypeDecl::new(
                SKILL_LISTING,
                "The current invokable-skills menu (frontends fold it into their / palette)",
            )
            .with_schema(json!({
                "type": "object",
                "required": ["skills"],
                "properties": {
                    "skills": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["name", "description"],
                            "properties": {
                                "name": {"type": "string"},
                                "description": {"type": "string"},
                            },
                        },
                    },
                },
            })),
        ],
        // The self-referential ring no environment rule could guess: the
        // standing watch on the skill folders comes back to refresh
        default_wiring: vec![WireSuggestion {
            from: "self.changed".to_string(),
            to: "self.refresh".to_string(),
        }],
        capabilities: Some(EffectSurface {
            reads: vec!["<skills>".to_string()],
            writes: vec!["<skills>".to_string()],
            network: vec!["*".to_string()],
            executes: true,
            reversible: false,
            // Installing a skill puts new INSTRUCTIONS in front of the model,
            // which is an admission as much as new code is.
            admits: Some("skills".to_string()),
        }),
        implements: vec!["tool-provider".to_string()],
        tools: vec![
            json!({
                "name": "LoadSkill",
                "description": "Load a skill by name: returns the skill's full \
                    instructions (its SKILL.md body). Pass `file` to fetch a bundled \
                    file from the same skill's folder instead (a reference document, \
                    a template). The result carries `dir`, the skill's directory — \
                    use it as the path prefix when running the skill's bundled \
                    scripts with the Run tool.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "file": {"type": "string"},
                    },
                    "required": ["name"],
                },
                "effects": {"reads": ["<skills>"], "reversible": true},
            }),
            json!({
                "name": "InstallSkill",
                "description": "Install a skill into the library from a local \
                    directory path or a git repository URL. The source must be a \
                    single skill (a folder with SKILL.md at its root); it is \
                    validated against the frontmatter canon before anything lands. \
                    `reason` is mandatory — it is recorded on the audit ledger as a \
                    decision event. The skill is loadable immediately; it enters the \
                    resident listing on the next restart.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "source": {"type": "string"},
                        "reason": {"type": "string"},
                    },
                    "required": ["source", "reason"],
                },
                // `admits`: this call INTRODUCES new instructions the model
                // will follow — the marker the trust gate keys on
                "effects": {"writes": ["<skills>"], "network": ["*"], "executes": true,
                            "admits": "skills"},
            }),
        ],
        prompt: None,
        handle_timeout_ms: Some(120_000),
        concurrency: None,
    }
}

// ── Frontmatter ─────────────────────────────────────────

/// Split a SKILL.md into (frontmatter YAML, markdown body).
/// The file must open with a `---` line; the next `---` line closes the header.
/// The canon's own rule for a skill name (schemas/skill_frontmatter.json):
/// lowercase letters, digits and single hyphens. Written out rather than
/// regex-matched because it is three lines and the canon is the authority
/// either way — what matters is that it admits no separator, no dot, and no
/// empty string, so a name can never become a path to somewhere else.
pub fn is_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\r\n")
        .or(text.strip_prefix("---\n"))?;
    for terminator in ["\n---\n", "\n---\r\n"] {
        if let Some(at) = rest.find(terminator) {
            return Some((&rest[..at], &rest[at + terminator.len()..]));
        }
    }
    // Frontmatter closed at end-of-file, no body
    rest.strip_suffix("\n---\n")
        .or(rest.strip_suffix("\n---"))
        .map(|yaml| (yaml, ""))
}

/// Parse and validate one SKILL.md against the canon. `dir_name` fills an
/// absent `name` (the ecosystem's common shorthand) and must match a present
/// one, per the specification.
/// Returns (frontmatter, body) or a one-line reason the skill is unusable.
fn read_skill_text(text: &str, dir_name: &str) -> Result<(Value, String), String> {
    let (yaml, body) = split_frontmatter(text).ok_or("SKILL.md has no YAML frontmatter")?;
    let mut front: Value =
        serde_yaml::from_str(yaml).map_err(|e| format!("frontmatter is not valid YAML: {e}"))?;
    if !front.is_object() {
        return Err("frontmatter is not a YAML mapping".to_string());
    }
    if front.get("name").is_none() {
        front["name"] = json!(dir_name);
    }
    if let Err(error) = frontmatter_canon().validate(&front) {
        return Err(format!("frontmatter fails the canon: {error}"));
    }
    if front["name"].as_str() != Some(dir_name) {
        return Err(format!(
            "frontmatter name {:?} does not match the directory name {dir_name:?}",
            front["name"]
        ));
    }
    Ok((front, body.to_string()))
}

fn read_skill_dir(dir: &Path) -> Result<(Value, String), String> {
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("skill directory has no readable name")?;
    let text = std::fs::read_to_string(dir.join("SKILL.md"))
        .map_err(|e| format!("cannot read SKILL.md: {e}"))?;
    read_skill_text(&text, dir_name)
}

// ── Listing ─────────────────────────────────────────────

/// One entry discovered by a directory scan.
struct Discovered {
    name: String,
    /// Ok(description) or Err(why this skill is unusable) — surfaced in the
    /// listing rather than silently dropped, so a broken skill is a visible
    /// problem, not a ghost.
    description: Result<String, String>,
}

/// Scan the directories in priority order; first occurrence of a name wins.
fn scan(dirs: &[String]) -> Vec<Discovered> {
    let mut found: Vec<Discovered> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue; // a missing directory is not an error, just empty
        };
        let mut names: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && p.join("SKILL.md").is_file())
            .collect();
        names.sort();
        for path in names {
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
                continue;
            };
            if found.iter().any(|s| s.name == name) {
                continue; // shadowed by an earlier directory
            }
            let description = read_skill_dir(&path)
                .map(|(front, _)| front["description"].as_str().unwrap_or("").to_string());
            found.push(Discovered { name, description });
        }
    }
    found
}

/// The resident listing: one line per skill, computed by the assembly
/// generator at startup and carried as the skill-library instance's prompt
/// override. None when no skills exist (zero resident cost when unused).
pub fn listing_prompt(dirs: &[String]) -> Option<String> {
    let found = scan(dirs);
    if found.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Skills — packaged instructions for specific kinds of tasks. When a task \
         matches one below, call load_skill with its name BEFORE doing the task, \
         and follow the loaded instructions:\n",
    );
    for skill in &found {
        match &skill.description {
            Ok(description) => {
                text.push_str(&format!("- {}: {}\n", skill.name, description));
            }
            Err(why) => {
                text.push_str(&format!("- {}: (unavailable — {})\n", skill.name, why));
            }
        }
    }
    Some(text)
}

/// The invokable menu as the listing event's payload: usable skills only
/// (a broken skill cannot be meaningfully invoked; it stays visible in the
/// resident prompt listing instead).
fn menu_payload(dirs: &[String]) -> Value {
    let skills: Vec<Value> = scan(dirs)
        .into_iter()
        .filter_map(|s| {
            s.description
                .ok()
                .map(|d| json!({"name": s.name, "description": d}))
        })
        .collect();
    json!({"skills": skills})
}

// ── The component ───────────────────────────────────────

pub struct SkillLibrary {
    dirs: Vec<String>,
    /// Where install_skill lands new skills (defaults to the first directory)
    install_dir: String,
    /// Largest content returned by load_skill; extra is truncated with a marker
    max_bytes: usize,
    exclusive: bool,
}

impl SkillLibrary {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let dirs: Vec<String> = get("dirs")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_else(default_dirs);
        let install_dir = get("installDir")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| dirs.first().cloned())
            .unwrap_or_else(|| "./.lattice/skills".to_string());
        Self {
            dirs,
            install_dir,
            max_bytes: get("maxBytes").and_then(Value::as_u64).unwrap_or(262_144) as usize,
            exclusive: get("exclusive").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// Where the skill called `name` lives, if it is one of ours.
    ///
    /// The name is checked against the canon's own rule before it is joined to
    /// anything. Rust's `join` REPLACES the whole path when handed an absolute
    /// one, so `load_skill("/etc/whatever")` read straight out of the
    /// filesystem, and `"../.."` walked out the same way — "skills are only
    /// read from the configured folders" was not true of the one argument a
    /// model chooses freely. The bundled-file reader next door already guards
    /// its argument; this one was the way in that nobody had closed.
    fn resolve(&self, name: &str) -> Option<PathBuf> {
        if !is_skill_name(name) {
            return None; // not a name at all — certainly not one of ours
        }
        for dir in &self.dirs {
            let path = Path::new(dir).join(name);
            if path.join("SKILL.md").is_file() {
                return Some(path);
            }
        }
        None
    }

    /// Say the menu to the ledger — but only when it differs from the last
    /// listing already there. No listing yet counts as an empty menu, so a
    /// skill-less boot stays silent and a restart repeats nothing. (A
    /// process-hosted library sees an empty ledger and honestly
    /// re-announces a non-empty menu each boot.)
    fn announce_listing(&self, ctx: &mut Ctx, causes: &[&str]) -> Result<(), String> {
        let payload = menu_payload(&self.dirs);
        let last_skills = ctx
            .log()
            .scan_back_types(&[SKILL_LISTING], |event, _| {
                Ok(Some(event.payload["skills"].clone()))
            })
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| json!([]));
        if last_skills != payload["skills"] {
            ctx.emit("audit", EventDraft::new(SKILL_LISTING, causes, payload));
        }
        Ok(())
    }

    /// The expansion of a `/skill-name args` user message, or None when the
    /// message is not an invocation of an installed, usable skill (it is
    /// then forwarded untouched — a mistyped name reaches the model as
    /// typed, and the resident listing lets it help). `$ARGUMENTS` in the
    /// body is substituted; without the placeholder, non-empty arguments
    /// are appended, mirroring the ecosystem convention.
    fn expand(&self, text: &str) -> Option<String> {
        let rest = text.strip_prefix('/')?;
        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((name, args)) => (name, args.trim()),
            None => (rest.trim_end(), ""),
        };
        if name.is_empty() {
            return None;
        }
        let dir = self.resolve(name)?;
        let (_, body) = read_skill_dir(&dir).ok()?;
        let body = self.truncate(body);
        Some(if body.contains("$ARGUMENTS") {
            body.replace("$ARGUMENTS", args)
        } else if args.is_empty() {
            body
        } else {
            format!("{body}\n\nARGUMENTS: {args}")
        })
    }

    fn truncate(&self, mut content: String) -> String {
        if content.len() > self.max_bytes {
            let mut cut = self.max_bytes;
            while !content.is_char_boundary(cut) {
                cut -= 1;
            }
            content.truncate(cut);
            content.push_str("\n[truncated]");
        }
        content
    }

    // ── load_skill ──────────────────────────────────────

    fn load(&self, arguments: &Value, ctx: &Ctx) -> Value {
        let Some(name) = arguments["name"].as_str() else {
            return error(
                "skill.bad_request",
                "load_skill requires a `name`",
                "request",
            );
        };
        let Some(dir) = self.resolve(name) else {
            return error(
                "skill.unknown",
                &format!("no skill named {name:?} in the library"),
                "request",
            );
        };
        let dir_display = dir.display().to_string();

        if let Some(file) = arguments["file"].as_str() {
            return match self.read_bundled(&dir, file) {
                Ok(content) => json!({"status": "ok", "result": {
                    "skill": name,
                    "file": file,
                    "dir": dir_display,
                    "content": self.truncate(content),
                }}),
                Err(payload) => payload,
            };
        }

        let (_, body) = match read_skill_dir(&dir) {
            Ok(parsed) => parsed,
            Err(why) => {
                return error(
                    "skill.invalid",
                    &format!("skill {name:?} is unusable: {why}"),
                    "request",
                )
            }
        };
        let digest = sha256_hex(body.as_bytes());

        // Already on the ledger with identical content? Answer with a note
        // instead of re-injecting the full body — the original is in context
        // (or recoverable via recall_event).
        let already = match ctx
            .log()
            .scan_back_types(&[ce::TOOL_EXEC_COMPLETED], |event, _| {
                Ok((event.payload["status"] == "ok"
                    && event.payload["result"]["skill"] == name
                    && event.payload["result"]["sha256"] == digest.as_str()
                    && event.payload["result"]["content"].is_string())
                .then(|| event.id.clone()))
            }) {
            Ok(already) => already,
            Err(problem) => {
                return error(
                    "skill.history_read_failed",
                    &problem.to_string(),
                    "environment",
                )
            }
        };
        if let Some(at) = already {
            return json!({"status": "ok", "result": {
                "skill": name,
                "dir": dir_display,
                "sha256": digest,
                "note": format!(
                    "already loaded at event {at}; content unchanged — follow that copy"
                ),
            }});
        }

        json!({"status": "ok", "result": {
            "skill": name,
            "dir": dir_display,
            "sha256": digest,
            "content": self.truncate(body),
        }})
    }

    /// Fetch a bundled file, confined to the skill's directory: no absolute
    /// paths, no `..`, and the resolved location (symlinks followed) must
    /// stay inside the skill folder.
    fn read_bundled(&self, dir: &Path, file: &str) -> Result<String, Value> {
        let relative = Path::new(file);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(error(
                "skill.invalid_path",
                "`file` must be a relative path inside the skill folder",
                "request",
            ));
        }
        let target = dir.join(relative);
        let confined = target
            .canonicalize()
            .ok()
            .zip(dir.canonicalize().ok())
            .is_some_and(|(target, root)| target.starts_with(root));
        if !confined {
            return Err(error(
                "skill.invalid_path",
                "the file does not exist or resolves outside the skill folder",
                "request",
            ));
        }
        std::fs::read_to_string(&target).map_err(|e| {
            error(
                "skill.unreadable",
                &format!("cannot read {file:?}: {e}"),
                "environment",
            )
        })
    }

    // ── install_skill ───────────────────────────────────

    /// Returns the outcome payload, plus (name, sha256) on success for the
    /// caller to record the decision event.
    fn install(&self, arguments: &Value, ctx: &Ctx) -> (Value, Option<(String, String)>) {
        let Some(source) = arguments["source"].as_str() else {
            return (
                error(
                    "skill.bad_request",
                    "install_skill requires a `source`",
                    "request",
                ),
                None,
            );
        };
        if arguments["reason"].as_str().is_none_or(str::is_empty) {
            return (
                error(
                    "skill.bad_request",
                    "install_skill requires a `reason` — it is recorded on the audit ledger",
                    "request",
                ),
                None,
            );
        }

        if let Err(e) = std::fs::create_dir_all(&self.install_dir) {
            return (
                error(
                    "skill.install_failed",
                    &format!("cannot create the install directory: {e}"),
                    "environment",
                ),
                None,
            );
        }
        let staging = Path::new(&self.install_dir).join(".staging");
        let _ = std::fs::remove_dir_all(&staging);

        let cancelled = || ctx.cancelled();
        if let Err(fetch_error) = crate::fetch::fetch(source, &staging, &cancelled) {
            let _ = std::fs::remove_dir_all(&staging);
            let payload = match fetch_error {
                crate::fetch::FetchError::Cancelled => {
                    error("skill.cancelled", "installation cancelled", "request")
                }
                crate::fetch::FetchError::Failed(why) => {
                    error("skill.fetch_failed", &why, "environment")
                }
            };
            return (payload, None);
        }

        // Validate before anything lands under a real name; a rejected skill
        // leaves no trace
        let text = match std::fs::read_to_string(staging.join("SKILL.md")) {
            Ok(text) => text,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return (
                    error(
                        "skill.invalid",
                        &format!("the source has no readable SKILL.md at its root: {e}"),
                        "request",
                    ),
                    None,
                );
            }
        };
        let fallback = crate::fetch::source_basename(source);
        let (front, body) = match read_skill_text(&text, &fallback) {
            Ok(parsed) => parsed,
            Err(why) => {
                let _ = std::fs::remove_dir_all(&staging);
                return (error("skill.invalid", &why, "request"), None);
            }
        };
        let name = front["name"].as_str().unwrap_or(&fallback).to_string();

        let target = Path::new(&self.install_dir).join(&name);
        if target.exists() {
            let _ = std::fs::remove_dir_all(&staging);
            return (
                error(
                    "skill.exists",
                    &format!("a skill named {name:?} is already installed"),
                    "request",
                ),
                None,
            );
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::remove_dir_all(&staging);
            return (
                error(
                    "skill.install_failed",
                    &format!("cannot move the skill into place: {e}"),
                    "environment",
                ),
                None,
            );
        }

        let digest = sha256_hex(body.as_bytes());
        let payload = json!({"status": "ok", "result": {
            "installed": name,
            "dir": target.display().to_string(),
            "sha256": digest,
            "note": "loadable immediately via load_skill; enters the resident listing on restart",
        }});
        (payload, Some((name, digest)))
    }
}

impl Component for SkillLibrary {
    /// Put the OS notification on the skill folders (the ones that exist):
    /// any change injects a wake through `changed`, which the assembly wires
    /// back to `refresh`. Runs on every start — fresh or reopened — so the
    /// standing watch needs no persistence of its own.
    fn restore(&mut self, ctx: &mut Ctx) {
        // Read recovery state before starting the standing watch.
        if let Err(error) = self.announce_listing(ctx, &[]) {
            ctx.fail("restore skill listing", error, &[]);
            return;
        }
        crate::components::fs_watch::arm_standing(ctx.injector(), "changed", &self.dirs, 500);
    }

    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if port == "input" {
            // The expansion station (see the `input` port declaration)
            let text = event.payload["text"].as_str().unwrap_or("");
            let payload = match self.expand(text) {
                Some(expanded) => json!({"text": expanded}),
                None => event.payload.clone(),
            };
            ctx.emit(
                "expanded",
                EventDraft::new(ce::USER_MESSAGE, &[&event.id], payload),
            );
            return;
        }
        if port == "refresh" {
            // The folders changed under us: refresh the listing fragment.
            // The context gate adopts the change when the cache is cold, as
            // ever; a brand-new skill is loadable by name right away.
            if let Err(error) = self.announce_listing(ctx, &[&event.id]) {
                ctx.fail("refresh skill listing", error, &[]);
                return;
            }
            ctx.set_prompt(listing_prompt(&self.dirs));
            return;
        }
        let tool = event.payload["tool"].as_str().unwrap_or("");
        let ours = tool == "LoadSkill" || tool == "InstallSkill";
        if !ours && !self.exclusive {
            return; // someone else's tool; the fan-out convention is silence
        }
        let arguments = &event.payload["arguments"];
        let mut payload = match tool {
            "LoadSkill" => self.load(arguments, ctx),
            "InstallSkill" => {
                let (payload, installed) = self.install(arguments, ctx);
                if let Some((name, sha256)) = installed {
                    // The reasoned decision record; the audit port is unwired
                    // by default, so this lands on the ledger and goes nowhere
                    ctx.emit(
                        "audit",
                        EventDraft::new(
                            SKILL_INSTALLED,
                            &[&event.id],
                            json!({
                                "name": name,
                                "source": arguments["source"],
                                "sha256": sha256,
                            }),
                        )
                        .with_reason(arguments["reason"].as_str().unwrap_or("")),
                    );
                }
                payload
            }
            other => error("tool.unknown", &format!("unknown tool: {other}"), "request"),
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
        // Refresh the resident listing after any skill activity, so installs
        // and folder drop-ins become visible without a restart. The deferred
        // discipline holds: the context gate adopts a changed system prompt
        // only when the provider cache is cold anyway, so this never costs a
        // warm prefix — until then the new skill stays reachable by name
        // (the model knows it from the conversation that installed it).
        // And the frontends' menu keeps pace (unchanged menus stay silent).
        if let Err(error) = self.announce_listing(ctx, &[&event.id]) {
            ctx.fail("refresh skill listing", error, &[]);
            return;
        }
        ctx.set_prompt(listing_prompt(&self.dirs));
    }
}

// ── Helpers ─────────────────────────────────────────────

fn error(code: &str, message: &str, blame: &str) -> Value {
    json!({"status": "error", "error": {
        "code": code,
        "message": message,
        "blame": blame,
        "retryable": false,
        "transient": false,
    }})
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
