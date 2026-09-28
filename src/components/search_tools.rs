//! Search tools — finding files by name and by content.
//!
//! These exist so that LOOKING does not cost the price of DOING. An agent that
//! wants to find where something is written can reach for `run("grep …")`, but
//! `Run` declares `executes`, and a policy that grants execution has granted
//! everything. These tools declare `reads` and nothing else, so an assembly can
//! let an agent read a codebase thoroughly while forbidding it to change or run
//! anything at all.
//!
//! That narrowness is STRUCTURAL, which is what makes the declaration worth
//! believing: the search runs inside this process, against libraries, with no
//! shell and no child process anywhere. There is no argument that turns a
//! search into a write, because the code that would write does not exist here.
//!
//! The engine is ripgrep's own (`ignore` for the walk, `grep-searcher` and
//! `grep-regex` for matching, `globset` for names) — linked, not invoked. The
//! alternative, running `rg` when the machine happens to have it, would have
//! meant executing a program (the very cost these tools exist to avoid) and
//! would have made the same call answer differently on different machines,
//! which a runtime whose ledger is meant to replay cannot afford.

use std::path::{Component as PathComponent, Path, PathBuf};

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "search-tools";

/// How many hits come back when the caller does not say, and the ceiling on
/// one returned line. A search across a repository can match thousands of
/// times; an event carrying all of them helps nobody and costs everybody.
const DEFAULT_LIMIT: usize = 100;
const MAX_LINE: usize = 400;

/// The most directory entries one search may VISIT, whatever it was asked.
///
/// Not a time limit, deliberately: this component refuses to answer the same
/// call differently on different machines (see the note at the top), and a
/// clock does exactly that. A count is the same everywhere.
///
/// Sized against real work rather than against the pathological case: a large
/// repository is tens of thousands of entries, so this leaves room to spare
/// while keeping the walk to a couple of seconds. What it stops is the search
/// that was never going to finish — a home directory with ignore files
/// disabled is 2.2M entries, measured, and burns a core the whole way.
const MAX_VISITED: usize = 200_000;

pub fn tool_decls(root: Option<&str>) -> Vec<Value> {
    let scope = root.unwrap_or("*");
    // Where a search may start. The rest of the wording is shared, because the
    // one thing that differs between the modes is exactly this.
    let where_start = match root {
        Some(_) => "under the tool root (absolute paths and .. are refused)",
        None => "anywhere: `path` may be absolute or relative to the working directory",
    };
    vec![
        json!({
            "name": "Find",
            "description": format!("Find files by name, searching {where_start}. `glob` is a \
                shell-style pattern (e.g. \"**/*.rs\", \"src/**/mod.rs\") matched against \
                each path RELATIVE to where the search starts, while the paths that come \
                back are ready to hand straight to `Read`. Dotfiles and files \
                .gitignore excludes are skipped unless you ask for them. Read-only: this cannot \
                change anything."),
            "parameters": {
                "type": "object",
                "properties": {
                    "glob": {"type": "string", "description": "e.g. **/*.rs"},
                    "path": {"type": "string", "description": "subdirectory to search under"},
                    "hidden": {"type": "boolean", "description": "also look at dotfiles"},
                    "ignored": {"type": "boolean", "description": "also look at files \
                        .gitignore excludes. Expensive: it stops honouring every ignore \
                        file on the way down, so a large tree grows several times over. \
                        Narrow `path` first."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                    "maxBytes": {"type": "integer", "minimum": 4096, "maximum": 65536},
                    "cursor": {"type": "object", "description": "copy next from the previous page, keeping the same query; changes to directory metadata invalidate it"},
                },
                "required": ["glob"],
            },
            "effects": {"reads": [scope], "reversible": true},
        }),
        json!({
            "name": "Grep",
            "description": format!("Search file CONTENTS by regular expression, searching \
                {where_start}, returning path, line number and the matching line. Narrow it \
                with `glob` (which files) and `path` (where to start). Dotfiles, files .gitignore excludes, and \
                binaries are skipped unless you ask otherwise. Read-only: this cannot change anything."),
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "regular expression"},
                    "glob": {"type": "string", "description": "restrict to matching paths"},
                    "path": {"type": "string", "description": "subdirectory to search under"},
                    "ignoreCase": {"type": "boolean"},
                    "fixed": {"type": "boolean", "description": "match literal text instead of a regular expression"},
                    "context": {"type": "integer", "minimum": 0, "maximum": 10, "description": "nearby lines on each side; overlapping ranges appear once, marked match=true/false"},
                    "output": {"type": "string", "enum": ["content", "files", "count"], "description": "matching lines (default), file names only, or per-file matching-line counts"},
                    "matchWindow": {"type": "boolean", "description": "show text near the first match with zero-based matchByte/windowByte, instead of the line opening"},
                    "hidden": {"type": "boolean", "description": "also look at dotfiles"},
                    "ignored": {"type": "boolean", "description": "also look at files \
                        .gitignore excludes. Expensive: it stops honouring every ignore \
                        file on the way down, so a large tree grows several times over. \
                        Narrow `path` first."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                    "maxBytes": {"type": "integer", "minimum": 4096, "maximum": 65536},
                    "cursor": {"type": "object", "description": "copy next from the previous page, keeping the same query; changes to directory metadata invalidate it"},
                },
                "required": ["pattern"],
            },
            "effects": {"reads": [scope], "reversible": true},
        }),
    ]
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
        // Reads, and only reads. Nothing here writes, executes or dials out.
        // The DEFAULT surface — see the note on fs-tools' manifest: these
        // declarations are per-component, and unconfined is the default.
        capabilities: Some(EffectSurface {
            reads: vec!["*".to_string()],
            ..Default::default()
        }),
        implements: vec!["tool-provider".to_string()],
        tools: tool_decls(None),
        prompt: Some(
            "Prefer `Find` and `Grep` over their shell equivalents: they are faster, \
             and they cost only the right to read, where `Run` costs the right to \
             execute anything.\n\
             Use `Find` for file names and `Grep` for text; narrow the directory \
             and file pattern first. Use context and continuation cursors instead \
             of guessing nearby ranges or restarting the same search. Search to \
             identify relevant files, then read only the needed parts; do not \
             open a succession of whole files just to discover which one matters.\n\
             Use `Code` for symbols, definitions, references, and complete symbol bodies \
             when that tool and its local language service are available, rather than \
             inferring them from text searches. If unavailable, say so; text matches \
             are not semantic references. Do not install a service automatically. \
             Follow returned Read arguments to inspect original source before editing; \
             a file version does not certify that the service index is fresh."
                .to_string(),
        ),
        handle_timeout_ms: Some(30_000),
        // Several at once: a model that asks for three files in one turn
        // means them read together, not one after another. Safe here because
        // nothing is kept between deliveries.
        concurrency: Some(8),
    }
}

pub struct SearchTools {
    /// `None` = a search may start anywhere. `Some` = every search starts
    /// inside this directory.
    root: Option<PathBuf>,
    exclusive: bool,
    /// The walk's ceiling ([`MAX_VISITED`] unless the assembly says otherwise).
    /// Configurable so a test can reach it without building a filesystem the
    /// size of the one that found this.
    max_visited: usize,
}

impl SearchTools {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        Self {
            root: get("root").and_then(Value::as_str).map(PathBuf::from),
            exclusive: get("exclusive").and_then(Value::as_bool).unwrap_or(false),
            max_visited: get("maxVisited")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(MAX_VISITED),
        }
    }

    /// Where to start walking. Unconfined, that is whatever the caller named
    /// (or the working directory); confined, the root or a subdirectory of it,
    /// with absolute paths and `..` refused exactly as the file tools refuse
    /// them — a search that could be pointed anywhere would not be a read of
    /// "the root", and there the declaration says the root.
    fn start_dir(&self, rel: Option<&str>) -> Result<PathBuf, String> {
        let Some(root) = self.root.clone() else {
            return Ok(PathBuf::from(rel.unwrap_or(".")));
        };
        let mut resolved = root;
        for component in Path::new(rel.unwrap_or("")).components() {
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
        // Where it REALLY is, not where its spelling suggests. `follow_links`
        // governs links met while walking, but the walk's own starting point
        // is followed regardless — so a link inside the root pointing anywhere
        // (one the agent can create with a shell) was a legal `path` argument
        // that searched the whole disk while the declared surface said "the
        // root". The file tools have checked this all along; this is the same
        // check, in the tool that was missing it.
        let real_root = self
            .root
            .as_ref()
            .expect("confined mode has a root")
            .canonicalize()
            .map_err(|e| format!("tool root is unavailable: {e}"))?;
        if let Ok(real) = resolved.canonicalize() {
            if !real.starts_with(&real_root) {
                return Err("path resolves outside the tool root".to_string());
            }
        }
        Ok(resolved)
    }

    /// The walker both tools share. Symlinks are NOT followed: a link pointing
    /// out of the root would otherwise carry the search out with it, and no
    /// argument check can undo that once it has happened.
    ///
    /// Two separate questions, because they are two questions. Dotfiles are
    /// hidden by convention; ignored files are excluded by a project that said
    /// so. One flag used to answer both, and the cheap-sounding half carried
    /// the expensive one: asking to see dotfiles also stopped honouring every
    /// .gitignore on the way down. On a home directory that is the difference
    /// between 530k entries and 2.2M — measured — and the caller had no way to
    /// know, because the schema said "include hidden/ignored files" as if it
    /// were one thing.
    fn walker(&self, start: &Path, hidden: bool, ignored: bool) -> ignore::Walk {
        let mut builder = ignore::WalkBuilder::new(start);
        builder
            .follow_links(false)
            .hidden(!hidden)
            .git_ignore(!ignored)
            .git_global(!ignored)
            .git_exclude(!ignored)
            .parents(!ignored);
        builder.build()
    }

    /// A path as the caller should see it — and, in both modes, a path the
    /// caller can hand straight back to `Read`. Confined, that means stripping
    /// the root, which also keeps where this agent's root really sits on the
    /// machine out of the answer; unconfined there is nothing to hide and the
    /// path stands as walked, only tidied of a leading `./`.
    fn shown(&self, path: &Path) -> String {
        let shown = match &self.root {
            Some(root) => path.strip_prefix(root).unwrap_or(path),
            None => path.strip_prefix("./").unwrap_or(path),
        };
        shown.to_string_lossy().into_owned()
    }

    /// What a `glob` is matched against: the path relative to where THIS
    /// search started. Anchoring on the start rather than on what comes back
    /// is what makes `find(path: "/some/project", glob: "src/**/*.rs")` mean
    /// what it looks like it means; against an absolute answer it would match
    /// nothing, and the pattern a caller writes should not depend on where the
    /// directory they named happens to live.
    fn matched(start: &Path, path: &Path) -> String {
        path.strip_prefix(start)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn find(&self, args: &Value, cancelled: &dyn Fn() -> bool) -> Value {
        page::search(self, args, false, cancelled)
    }

    fn grep(&self, args: &Value, cancelled: &dyn Fn() -> bool) -> Value {
        page::search(self, args, true, cancelled)
    }
}

/// One line of a hit, bounded: a minified bundle matching a common word must
/// not put a megabyte on the ledger.
fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

mod page;
mod scan;

/// Cancelled part-way. Answered as a CANCELLATION rather than an error or a
/// short result: what was found is not wrong, it is incomplete, and a caller
/// told "here are 3 matches" would read that as all of them.
///
/// Answering at all is the point. This tool never looked at the token, so a
/// search over a large tree ran to the end no matter what: the watchman's
/// deadline passed with nothing stopping, the grace passed, and the whole
/// search component was declared unresponsive — which took `Find` and `Grep`
/// away for the rest of the session. Cancellation only works if the thing
/// being cancelled is looking.
fn cut_short(found: usize) -> Value {
    json!({"status": "cancelled", "result": {
        "note": format!(
            "cancelled part-way through the search; {found} match(es) had been found \
             by then, which is not the whole answer"
        ),
    }})
}

/// Asked to look at more of the filesystem than one search may look at.
///
/// An error rather than a short result: a caller handed "here are 4 matches"
/// would build on it, and the honest thing to say is that this search never
/// covered the ground it was pointed at. It also says what to do — the two
/// arguments that make a search this wide are the two named here.
fn too_wide(found: usize, ignored: bool, cap: usize) -> Value {
    let ignoring = if ignored {
        " Asking for ignored files (\"ignored\": true) turned off every .gitignore \
         on the way down, which on a large tree multiplies what has to be walked."
    } else {
        ""
    };
    err(
        "tool.too_wide",
        &format!(
            "this search would have to walk more than {cap} directory entries, \
             so it was stopped ({found} match(es) had been found by then, which is not \
             the whole answer). Narrow it: point `path` at the directory that matters \
             rather than a whole home or filesystem.{ignoring}"
        ),
    )
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

impl Component for SearchTools {
    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        let mine = matches!(tool, "Find" | "Grep");
        if !mine && !self.exclusive {
            return; // someone else's tool; fan-out convention is silence
        }
        let args = &event.payload["arguments"];
        let mut payload = match tool {
            "Find" => self.find(args, &|| ctx.cancelled()),
            "Grep" => self.grep(args, &|| ctx.cancelled()),
            _ => err("tool.unknown", &format!("unknown tool: {tool}")),
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}

#[cfg(test)]
mod efficiency_tests {
    use super::*;

    #[test]
    fn pages_are_stable_and_refuse_changed_trees() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z.rs", "a.rs", "b.rs"] {
            std::fs::write(dir.path().join(name), "needle\n").unwrap();
        }
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        let mut args = json!({"glob": "*.rs", "limit": 1});
        let first = tools.find(&args, &|| false);
        assert_eq!(first["result"]["paths"], json!(["a.rs"]));
        assert!(first["result"]["next"].is_object(), "{first}");
        args["cursor"] = first["result"]["next"].clone();
        let second = tools.find(&args, &|| false);
        assert_eq!(second["result"]["paths"], json!(["b.rs"]));
        std::fs::write(dir.path().join("new.rs"), "new\n").unwrap();
        assert_eq!(
            tools.find(&args, &|| false)["error"]["code"],
            "tool.stale_cursor"
        );
    }

    #[test]
    fn byte_budget_pages_and_long_line_windows_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{}Needle{}\n", "界".repeat(600), "\\\"".repeat(600));
        std::fs::write(dir.path().join("a.txt"), long.repeat(25)).unwrap();
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        let mut args = json!({"pattern": "needle", "ignoreCase": true, "matchWindow": true, "maxBytes": 4096, "context": 10});
        let first = tools.grep(&args, &|| false);
        assert_eq!(first["status"], "ok", "{first}");
        assert!(
            first["result"].to_string().len() <= 4096,
            "budget includes escaped JSON and context"
        );
        assert_eq!(first["result"]["more"], true);
        let row = &first["result"]["matches"][0];
        assert_eq!(row["matchByte"], 1800);
        assert!(row["text"].as_str().unwrap().contains("Needle"));
        args["cursor"] = first["result"]["next"].clone();
        let next = tools.grep(&args, &|| false);
        assert_eq!(next["status"], "ok", "{next}");
        args["pattern"] = json!("different");
        assert_eq!(
            tools.grep(&args, &|| false)["error"]["code"],
            "tool.stale_cursor"
        );
        assert_eq!(
            tools.grep(&json!({"pattern": "x"}), &|| true)["status"],
            "cancelled"
        );
    }

    #[test]
    fn default_long_lines_still_show_the_opening() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            format!("{}needle", "a".repeat(500)),
        )
        .unwrap();
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        let found = tools.grep(&json!({"pattern": "needle"}), &|| false);
        assert_eq!(
            found["result"]["matches"][0]["text"],
            format!("{}…", "a".repeat(400))
        );
    }

    #[test]
    fn a_byte_match_inside_utf8_still_has_a_valid_text_window() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "é\n").unwrap();
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        let found = tools.grep(
            &json!({"pattern": r"(?-u:\xA9)", "matchWindow": true}),
            &|| false,
        );
        assert_eq!(found["status"], "ok", "{found}");
        assert_eq!(found["result"]["matches"][0]["matchByte"], 1);
        assert_eq!(found["result"]["matches"][0]["text"], "é");
    }

    #[test]
    fn review_context_marks_real_matches_across_page_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle\nneedle\n").unwrap();
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        let first = tools.grep(
            &json!({"pattern":"needle", "limit":1, "context":1}),
            &|| false,
        );
        let rows = first["result"]["matches"].as_array().unwrap();
        assert!(rows.iter().all(|row| row["match"] == true), "{first}");
        assert_eq!(rows.iter().filter(|row| row["selected"] == true).count(), 1);
        let second = tools.grep(
            &json!({"pattern":"needle", "limit":1, "context":1, "cursor":first["result"]["next"]}),
            &|| false,
        );
        assert!(second["result"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["match"] == true));
        assert_eq!(second["result"]["more"], false);
    }

    #[test]
    fn review_context_uses_the_searchers_bom_and_line_end_rules() {
        let dir = tempfile::tempdir().unwrap();
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        for little in [true, false] {
            let mut bytes = if little {
                vec![255, 254]
            } else {
                vec![254, 255]
            };
            for unit in "before\r\nneedle\r\nafter\r\n".encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            std::fs::write(dir.path().join("a.txt"), bytes).unwrap();
            let found = tools.grep(&json!({"pattern":"needle", "context":1}), &|| false);
            assert_eq!(found["status"], "ok", "{found}");
            assert_eq!(found["result"]["matches"][0]["text"], "before\r", "{found}");
            assert_eq!(found["result"]["matches"][2]["text"], "after\r");
        }
    }

    #[cfg(unix)]
    #[test]
    fn review_non_utf8_paths_are_refused_instead_of_merged() {
        use std::os::unix::ffi::OsStringExt;
        // APFS refuses these names before the tool can see them. Exercise the
        // identity guard directly, without assuming the filesystem accepts them.
        let paths: Vec<PathBuf> = [254, 255]
            .into_iter()
            .map(|byte| PathBuf::from(std::ffi::OsString::from_vec(vec![byte])))
            .collect();
        assert_eq!(paths[0].to_string_lossy(), paths[1].to_string_lossy());
        for path in paths {
            let error = page::validate_path(&path).unwrap_err();
            assert_eq!(error["error"]["code"], "tool.non_utf8_path");
        }
    }

    #[test]
    fn review_late_binary_data_never_produces_a_complete_prefix_count() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!("needle\n{}\0needle\n", "plain\n".repeat(20_000));
        std::fs::write(dir.path().join("a.txt"), text).unwrap();
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        let found = tools.grep(&json!({"pattern":"needle", "output":"count"}), &|| false);
        assert_eq!(found["error"]["code"], "tool.binary_data", "{found}");
    }

    #[test]
    fn context_merges_and_modes_preserve_literal_intent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "before\na.b\nbetween\na.b\naXb\nafter\n",
        )
        .unwrap();
        let tools = SearchTools::from_config(Some(&json!({"root": dir.path()})));
        let args = json!({"pattern": "a.b", "fixed": true, "context": 1});
        let found = tools.grep(&args, &|| false);
        let rows = found["result"]["matches"].as_array().unwrap();
        assert_eq!(rows.len(), 5, "overlapping context appears once: {found}");
        assert_eq!(rows.iter().filter(|r| r["match"] == true).count(), 2);
        let count = tools.grep(
            &json!({"pattern": "a.b", "fixed": true, "output": "count"}),
            &|| false,
        );
        assert_eq!(
            count["result"]["counts"],
            json!([{"path": "a.rs", "count": 2}])
        );
        let files = tools.grep(
            &json!({"pattern": "a.b", "fixed": true, "output": "files"}),
            &|| false,
        );
        assert_eq!(files["result"]["paths"], json!(["a.rs"]));
    }
}
