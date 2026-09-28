//! Timer tools — the time-driven wake source: `Schedule` starts a timer that
//! wakes the loop after a delay (once), or on an interval (repeating). This
//! one component covers three capabilities that are really the same shape —
//! something injecting a wake at its own moment:
//!
//! - **cron / delay**: `schedule(delay_ms)` — one wake, later.
//! - **loop**: `schedule(interval_ms, ...)` — a wake each interval; on each,
//!   the agent does its work and calls `Unschedule` when a stop condition holds.
//! - **monitor (polling)**: same repeating schedule; on each wake the agent
//!   checks a condition (with `Run` for anything bash can see — a file, a
//!   process, a URL) and acts + unschedules when it trips.
//!
//! Every fire is a `core.input.wake` (same wire as a background command's
//! finish), caused by the `Schedule` call that started it — so the ledger
//! answers "why did this turn run: timer N, set by that call". A repeating
//! timer is bounded by `max_fires` so a runaway loop cannot self-trigger
//! forever.
//!
//! Timers are pure data, so they survive a restart: `restore` reads the
//! ledger on reopen and re-arms every REPEATING timer that has not been
//! unscheduled or run out its `max_fires`, continuing its fire count where the
//! dead process left off (interval measured from now). A one-shot that already
//! fired is done; an unfired one-shot is fire-and-forget and not resurrected.

use super::subscription_history::{self as recovery, Kind};

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx, Injector};

pub const NAME: &str = "timer-tools";

pub fn tool_decls() -> Vec<Value> {
    vec![
        json!({
            "name": "Schedule",
            "description": "Start a timer that wakes you later. delay_ms: fire once after \
                this many ms. Add interval_ms to repeat every interval_ms (a loop or a \
                poll-monitor); each wake carries `note`. max_fires bounds a repeating \
                timer. Returns a timer id; stop it with unschedule.",
            "parameters": {
                "type": "object",
                "properties": {
                    "delay_ms": {"type": "integer"},
                    "interval_ms": {"type": "integer"},
                    "note": {},
                    "max_fires": {"type": "integer"},
                },
                "required": ["delay_ms"],
            },
            "effects": {"reversible": true},
            // Starts a long-lived wake source
            "async": "always",
        }),
        json!({
            "name": "Unschedule",
            "description": "Cancel a timer by its id; a repeating timer stops firing.",
            "parameters": {
                "type": "object",
                "properties": {"timer": {"type": "integer"}},
                "required": ["timer"],
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
            // A timer fires by waking the loop — wire to the loop's input
            PortDecl::new("wake", &[ce::WAKE]),
        ],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: Some(EffectSurface {
            reversible: true,
            ..Default::default()
        }),
        implements: vec!["tool-provider".to_string()],
        tools: tool_decls(),
        prompt: Some(
            "A timer wakes you with no new message from the user; the wake says where \
             it came from. A repeating one keeps waking you until you unschedule it, so \
             a poll-monitor is: check the condition each wake, unschedule when it trips."
                .to_string(),
        ),
        handle_timeout_ms: None,
        concurrency: None,
    }
}

pub struct TimerTools {
    next_id: u64,
    /// Shared with every timing thread; an id here means "stop firing"
    cancelled: Arc<Mutex<HashSet<u64>>>,
    exclusive: bool,
}

impl TimerTools {
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

    fn schedule(&mut self, args: &Value, started_id: &str, ctx: &mut Ctx) -> Value {
        let delay = args["delay_ms"].as_u64().unwrap_or(0);
        let interval = args["interval_ms"].as_u64();
        let max_fires = args["max_fires"].as_u64().unwrap_or(100).max(1);
        let note = args["note"].clone();
        let id = self.next_id;
        self.next_id += 1;

        let injector = ctx.injector();
        let cancelled = Arc::clone(&self.cancelled);
        let started = started_id.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay));
            if stopped(&cancelled, id) {
                return;
            }
            fire_wake(&injector, &started, id, 1, &note);
            if let Some(interval) = interval {
                let mut n = 1;
                while n < max_fires {
                    std::thread::sleep(Duration::from_millis(interval));
                    if stopped(&cancelled, id) {
                        return;
                    }
                    n += 1;
                    fire_wake(&injector, &started, id, n, &note);
                }
            }
        });

        json!({"status": "ok", "result": {
            "timer": id,
            "repeating": interval.is_some(),
            "note": "scheduled",
        }})
    }

    /// Re-arm a repeating timer on restore: continue firing from `done` up to
    /// `max_fires`, one interval apart, measured from now. Same fire shape and
    /// the same shared cancellation set as a live timer, so a later
    /// `Unschedule` stops it just the same.
    fn rearm(&self, id: u64, done: u64, args: &Value, started_id: &str, ctx: &mut Ctx) {
        let interval = args["interval_ms"].as_u64().unwrap_or(0);
        let max_fires = args["max_fires"].as_u64().unwrap_or(100).max(1);
        let note = args["note"].clone();
        let injector = ctx.injector();
        let cancelled = Arc::clone(&self.cancelled);
        let started = started_id.to_string();
        std::thread::spawn(move || {
            let mut n = done;
            while n < max_fires {
                std::thread::sleep(Duration::from_millis(interval));
                if stopped(&cancelled, id) {
                    return;
                }
                n += 1;
                fire_wake(&injector, &started, id, n, &note);
            }
        });
    }

    fn unschedule(&self, args: &Value) -> Value {
        match args["timer"].as_u64() {
            Some(id) => {
                self.cancelled.lock().unwrap().insert(id);
                json!({"status": "ok", "result": {"timer": id, "note": "cancelled"}})
            }
            None => json!({"status": "error", "error": {
                "code": "tool.bad_arguments",
                "message": "unschedule needs a numeric 'timer' id",
                "blame": "request",
                "retryable": false,
                "transient": false,
            }}),
        }
    }
}

/// Emit one timer fire — the WAKE shape shared by a fresh schedule and a
/// restored re-arm, so the two can never drift.
fn fire_wake(injector: &Injector, started: &str, id: u64, n: u64, note: &Value) {
    injector.emit(
        "wake",
        EventDraft::new(
            ce::WAKE,
            &[started],
            json!({
                "source": format!("timer:{id}"),
                "summary": format!("timer {id} fired (#{n})"),
                "body": {"timer": id, "fire": n, "note": note},
            }),
        ),
    );
}

/// Has this timer been unscheduled?
fn stopped(cancelled: &Arc<Mutex<HashSet<u64>>>, id: u64) -> bool {
    cancelled.lock().unwrap().contains(&id)
}

impl Component for TimerTools {
    /// Timers are pure data, so a restart is not a loss: read the ledger and
    /// re-arm every repeating timer that is still live (not unscheduled, not
    /// out of fires), continuing its count where the dead process stopped.
    fn restore(&mut self, ctx: &mut Ctx) {
        let restored = match recovery::recover(ctx.log(), Kind::Timer) {
            Ok(restored) => restored,
            Err(error) => {
                ctx.fail("restore timers", error.to_string(), &[]);
                return;
            }
        };
        if let Some(reason) = restored.cold_reason {
            eprintln!("slow recovery for timers: {reason}");
        }
        let state = restored.state;
        // No background activity starts until every required read succeeds.
        for timer in state.live {
            self.rearm(timer.id, timer.fired, &timer.arguments, &timer.cause, ctx);
        }
        self.next_id = state.next_id;
    }

    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        let mine = matches!(tool, "Schedule" | "Unschedule");
        if !mine && !self.exclusive {
            return; // fan-out convention: silence on foreign tools
        }
        let args = &event.payload["arguments"];
        let mut payload = match tool {
            "Schedule" => self.schedule(args, &event.id, ctx),
            "Unschedule" => self.unschedule(args),
            _ => json!({"status": "error", "error": {
                "code": "tool.unknown",
                "message": format!("unknown tool: {tool}"),
                "blame": "request",
                "retryable": false,
                "transient": false,
            }}),
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}
