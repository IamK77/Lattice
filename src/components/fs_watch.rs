//! fs-watch — the event-driven wake source: `Watch` puts an OS file-system
//! notification (inotify / FSEvents / kqueue, via the notify crate) on a path
//! and injects a `core.input.wake` when the path actually changes. The
//! event-driven counterpart of a polling monitor (a repeating timer + a `Run`
//! check): zero empty wake-ups, second-level reaction, no clock.
//!
//! Same two generic capabilities as every wake source — injection wakes the
//! loop, and the injector moves into a background thread. The kernel still
//! knows no word for "monitoring".
//!
//! Every fire is caused by the `Watch` call that armed it, so the ledger
//! answers "why did this turn run: watch N on that path, set by that call".
//! Bursts are coalesced (`debounce_ms`); `max_fires` bounds a noisy path.
//! Watches are standing subscriptions and pure data, so `restore` re-arms
//! every live one (not unwatched, not out of fires, path still present) on
//! reopen, continuing its fire count.
//!
//! [`arm_standing`] is the reusable half: any component may put a watch on
//! its own data directories and have the fires injected through one of its
//! own ports (the skill library watches its skill folders this way — wired
//! back to itself, no model turn involved).

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx, Injector};

pub const NAME: &str = "fs-watch";

pub fn tool_decls() -> Vec<Value> {
    vec![
        json!({
            "name": "Watch",
            "description": "Watch a file or directory: you are woken (like a new message) \
                the moment it actually changes — event-driven, no polling, no empty \
                wake-ups. Use it to monitor a log for new lines, a build directory for \
                artifacts, a dropbox for incoming files. Bursts of changes within \
                debounce_ms (default 200) arrive as ONE wake listing the changed paths. \
                max_fires (default 100) bounds a noisy path. Returns a watch id; stop \
                with unwatch.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "recursive": {"type": "boolean"},
                    "debounce_ms": {"type": "integer"},
                    "max_fires": {"type": "integer"},
                    "note": {},
                },
                "required": ["path"],
            },
            "effects": {"reads": ["<watched path>"], "reversible": true},
            // Starts a long-lived wake source
            "async": "always",
        }),
        json!({
            "name": "Unwatch",
            "description": "Stop a watch by its id.",
            "parameters": {
                "type": "object",
                "properties": {"watch": {"type": "integer"}},
                "required": ["watch"],
            },
            "effects": {"reversible": true},
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
        outputs: vec![
            PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED]),
            // A watch fires by waking the loop — wire to the loop's input
            PortDecl::new("wake", &[ce::WAKE]),
        ],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: Some(EffectSurface {
            reads: vec!["<watched path>".to_string()],
            reversible: true,
            ..Default::default()
        }),
        implements: vec!["tool-provider".to_string()],
        tools: tool_decls(),
        prompt: Some(
            "When what you are waiting for is a file appearing or changing, `Watch` it \
             rather than polling on a timer: it wakes you on the change itself."
                .to_string(),
        ),
        handle_timeout_ms: None,
        concurrency: None,
    }
}

pub struct FsWatch {
    next_id: u64,
    /// Shared with every watch thread; an id here means "stop firing"
    cancelled: Arc<Mutex<HashSet<u64>>>,
    exclusive: bool,
}

impl FsWatch {
    pub fn from_config(config: Option<&Value>) -> Self {
        Self {
            next_id: 1,
            cancelled: Arc::default(),
            exclusive: config
                .and_then(|c| c.get("exclusive"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    fn watch(&mut self, args: &Value, started_id: &str, ctx: &mut Ctx) -> Value {
        let Some(path) = args["path"].as_str() else {
            return error("tool.bad_arguments", "watch needs a 'path'", "request");
        };
        if !Path::new(path).exists() {
            return error(
                "watch.no_such_path",
                &format!("path does not exist: {path}"),
                "request",
            );
        }
        let id = self.next_id;
        self.next_id += 1;
        match self.arm(id, 0, args, path, Some(started_id.to_string()), ctx) {
            Ok(()) => json!({"status": "ok", "result": {
                "watch": id,
                "path": path,
                "note": "watching — a change on this path wakes you",
            }}),
            Err(e) => error(
                "watch.failed",
                &format!("cannot watch {path}: {e}"),
                "environment",
            ),
        }
    }

    /// Put the OS notification on the path and hand everything to a firing
    /// thread. One shape for a fresh `Watch` and a restored re-arm, so the
    /// two can never drift.
    fn arm(
        &self,
        id: u64,
        fired_already: u64,
        args: &Value,
        path: &str,
        started: Option<String>,
        ctx: &mut Ctx,
    ) -> Result<(), notify::Error> {
        let recursive = if args["recursive"].as_bool().unwrap_or(true) {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        let (watcher, rx) = os_watch(&[path.to_string()], recursive)?;
        let injector = ctx.injector();
        let cancelled = Arc::clone(&self.cancelled);
        let debounce = args["debounce_ms"].as_u64().unwrap_or(200);
        let max_fires = args["max_fires"].as_u64().unwrap_or(100).max(1);
        let note = args["note"].clone();
        let path = path.to_string();
        std::thread::spawn(move || {
            fire_loop(
                watcher,
                rx,
                &injector,
                "wake",
                FireIdentity {
                    watch: json!(id),
                    source: format!("watch:{id}"),
                    path,
                    causes: started,
                    note,
                },
                debounce,
                fired_already,
                max_fires,
                move || cancelled.lock().unwrap().contains(&id),
            );
        });
        Ok(())
    }

    fn unwatch(&self, args: &Value) -> Value {
        match args["watch"].as_u64() {
            Some(id) => {
                self.cancelled.lock().unwrap().insert(id);
                json!({"status": "ok", "result": {"watch": id, "note": "stopped"}})
            }
            None => error(
                "tool.bad_arguments",
                "unwatch needs a numeric 'watch' id",
                "request",
            ),
        }
    }
}

impl Component for FsWatch {
    /// Watches are standing subscriptions and pure data: on reopen, re-arm
    /// every one that is still live — not unwatched, not out of fires, its
    /// path still present — continuing the fire count where the dead process
    /// stopped. (Timers resurrect only their repeating kind; a watch IS the
    /// repeating kind by nature.)
    fn restore(&mut self, ctx: &mut Ctx) {
        let restored = match super::subscription_history::recover(
            ctx.log(),
            super::subscription_history::Kind::Watch,
        ) {
            Ok(restored) => restored,
            Err(error) => {
                ctx.fail("restore file watches", error.to_string(), &[]);
                return;
            }
        };
        if let Some(reason) = restored.cold_reason {
            eprintln!("slow recovery for file watches: {reason}");
        }
        let state = restored.state;
        // Required history reads finish before any OS watch is armed.
        for watch in state.live {
            let Some(path) = watch.arguments["path"].as_str() else {
                continue;
            };
            if !Path::new(path).exists() {
                continue;
            }
            let _ = self.arm(
                watch.id,
                watch.fired,
                &watch.arguments,
                path,
                Some(watch.cause),
                ctx,
            );
        }
        self.next_id = state.next_id;
    }

    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        let mine = matches!(tool, "Watch" | "Unwatch");
        if !mine && !self.exclusive {
            return; // fan-out convention: silence on foreign tools
        }
        let args = &event.payload["arguments"];
        let mut payload = match tool {
            "Watch" => self.watch(args, &event.id, ctx),
            "Unwatch" => self.unwatch(args),
            _ => error("tool.unknown", &format!("unknown tool: {tool}"), "request"),
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}

// ── The reusable half ───────────────────────────────────

/// Arm a standing, uncancellable watch over several paths for the lifetime
/// of the process, injecting each coalesced burst as a WAKE through `port`
/// of the injector's own instance. For components watching their own data
/// directories (the skill library watching its skill folders); missing paths
/// are skipped. Fires carry no causes (nothing on the ledger armed them —
/// they are root events, like user input).
pub fn arm_standing(injector: Injector, port: &str, paths: &[String], debounce_ms: u64) {
    let existing: Vec<String> = paths
        .iter()
        .filter(|p| Path::new(p).exists())
        .cloned()
        .collect();
    if existing.is_empty() {
        return;
    }
    let Ok((watcher, rx)) = os_watch(&existing, RecursiveMode::Recursive) else {
        return; // no notification backend: the standing watch is best-effort
    };
    let port = port.to_string();
    let label = existing.join(", ");
    std::thread::spawn(move || {
        fire_loop(
            watcher,
            rx,
            &injector,
            &port,
            FireIdentity {
                watch: Value::Null,
                source: "standing".to_string(),
                path: label,
                causes: None,
                note: Value::Null,
            },
            debounce_ms,
            0,
            10_000,
            || false,
        );
    });
}

// ── Shared machinery ────────────────────────────────────

struct FireIdentity {
    /// The tool-assigned id (Null for a standing watch)
    watch: Value,
    source: String,
    path: String,
    /// The watch call this fire is caused by (None = root event)
    causes: Option<String>,
    note: Value,
}

/// Put the OS notification on the paths; events arrive on the receiver.
fn os_watch(
    paths: &[String],
    mode: RecursiveMode,
) -> Result<(RecommendedWatcher, Receiver<notify::Result<notify::Event>>), notify::Error> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    for path in paths {
        watcher.watch(Path::new(path), mode)?;
    }
    Ok((watcher, rx))
}

/// Only content-affecting changes fire; reads and metadata chatter do not.
fn relevant(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// The firing loop shared by tool watches, restored re-arms and standing
/// watches — one shape, so they can never drift. Blocks on the OS channel
/// (checking `stop` twice a second), coalesces each burst for `debounce_ms`,
/// then injects ONE wake naming the changed paths.
#[allow(clippy::too_many_arguments)]
fn fire_loop(
    watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
    injector: &Injector,
    port: &str,
    identity: FireIdentity,
    debounce_ms: u64,
    fired_already: u64,
    max_fires: u64,
    stop: impl Fn() -> bool,
) {
    // Owned by this thread; dropping it removes the OS subscription
    let _watcher = watcher;
    let mut fires = fired_already;
    loop {
        // Wait for the first relevant change, polling the stop flag
        let first = loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Ok(event)) if relevant(&event) => break event,
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    if stop() {
                        return;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        // Coalesce the burst: everything within a FIXED window from the first
        // event is one fire. Fixed, not sliding — a path under continuous
        // change (a busy log) still fires once per window instead of waiting
        // forever for a lull.
        let mut changed: BTreeSet<String> = BTreeSet::new();
        collect_paths(&mut changed, &first);
        let window_closes = std::time::Instant::now() + Duration::from_millis(debounce_ms);
        loop {
            let now = std::time::Instant::now();
            if now >= window_closes {
                break;
            }
            match rx.recv_timeout(window_closes - now) {
                Ok(Ok(event)) if relevant(&event) => collect_paths(&mut changed, &event),
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        if stop() {
            return;
        }
        fires += 1;
        let listed: Vec<&String> = changed.iter().take(20).collect();
        let causes: Vec<&str> = identity.causes.iter().map(String::as_str).collect();
        injector.emit(
            port,
            EventDraft::new(
                ce::WAKE,
                &causes,
                json!({
                    "source": format!("fswatch:{}", identity.source),
                    "summary": format!(
                        "{} changed (#{fires}): {} path(s)",
                        identity.path,
                        changed.len()
                    ),
                    "body": {
                        "watch": identity.watch,
                        "path": identity.path,
                        "fire": fires,
                        "changed": listed,
                        "note": identity.note,
                    },
                }),
            ),
        );
        if fires >= max_fires {
            return;
        }
    }
}

fn collect_paths(into: &mut BTreeSet<String>, event: &notify::Event) {
    for path in &event.paths {
        into.insert(path.display().to_string());
    }
}

fn error(code: &str, message: &str, blame: &str) -> Value {
    json!({"status": "error", "error": {
        "code": code,
        "message": message,
        "blame": blame,
        "retryable": false,
        "transient": false,
    }})
}
