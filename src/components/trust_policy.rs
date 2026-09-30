//! trust-policy — the admission gate: install-class tool calls (the ones
//! whose declared surface carries the `admits` marker: they introduce new
//! code or new instructions into the runtime) pass only if the user has
//! granted them. Everything else is forwarded untouched.
//!
//! The trust record = the canonical fingerprint of the admitting call's
//! arguments + the surface the tool declared when granted. Content-addressed
//! and appetite-sensitive: the same git source or the same inline code
//! matches; a changed source or a changed declared surface does not — it is
//! asked (or refused) afresh. Grants live in the user directory
//! (schemas/trust_grants.json is the canon): a user-level, cross-session
//! fact; each granting is also a decision event on the conversation's ledger.
//!
//! Two stances for the ungranted case:
//! - `deny` (default, unattended-safe): a reasoned decision + an error
//!   verdict telling the user exactly how to grant.
//! - `ask`: the authorization EVENT PAIR — the gate emits
//!   `trust.authorization_requested` and answers nothing; the turn waits.
//!   A frontend (or any wired answerer) shows the request to the human and
//!   injects `trust.authorization_answered` naming the request event;
//!   approval records the grant and forwards the original call with JOINED
//!   causes (request + answer), refusal answers an error verdict. All state
//!   is read off the ledger — no memory, restart-safe (a restart while
//!   pending settles the dangling call as interrupted, the standard way).

use std::path::PathBuf;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "trust-policy";
pub const DECISION: &str = "trust.gate.decision";
pub const AUTH_REQUESTED: &str = "trust.authorization_requested";
/// The answer rides `core.input.external` (channel = this value) instead of
/// a gate-owned event type, so any frontend can declare an answer OUTPUT
/// port without its assembly needing the gate registered — core types are
/// registered everywhere, a gate-owned type only where the gate is.
pub const AUTH_CHANNEL: &str = "trust.authorization";

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![
            PortDecl::new("review", &[ce::TOOL_EXEC_STARTED]),
            // The human's side of the event pair comes back here, as external
            // input on the authorization channel
            PortDecl::new("answer", &[ce::EXTERNAL_INPUT]),
        ],
        outputs: vec![
            PortDecl::new("forward", &[ce::TOOL_EXEC_STARTED]),
            PortDecl::new("verdict", &[ce::TOOL_EXEC_COMPLETED]),
            // The gate's own letters, beyond the generic profile
            PortDecl::new("decision", &[DECISION]),
            PortDecl::new("request", &[AUTH_REQUESTED]),
        ],
        events: vec![
            EventTypeDecl::decision(DECISION, "The trust gate granted or refused an admission")
                .with_schema(json!({
                    "type": "object",
                    "required": ["verdict", "key"],
                    "properties": {
                        "verdict": {"enum": ["granted", "denied"]},
                        "key": {"type": "string"},
                        "admits": {"type": "string"},
                    },
                })),
            EventTypeDecl::new(
                AUTH_REQUESTED,
                "An ungranted admission awaits the user's authorization",
            )
            .with_schema(json!({
                "type": "object",
                "required": ["request", "key", "tool"],
                "properties": {
                    "request": {"type": "string",
                                "description": "id of the reviewed tool request"},
                    "key": {"type": "string"},
                    "tool": {"type": "string"},
                    "admits": {"type": "string"},
                    "effects": {"type": "object"},
                    "summary": {"type": "string"},
                },
            })),
        ],
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["policy".to_string()],
        tools: Vec::new(),
        // The gate explains the gate. Without this the model meets a refusal
        // with no idea a human made it, and does the one thing that wastes
        // everyone's turn: tries again, or goes looking for a way around.
        prompt: Some(
            "Some calls stop and wait for the user to approve them. If one comes back \
             refused, that is the user's decision, not a fault to work around: do not \
             retry it and do not look for another tool that does the same thing. Say \
             what you wanted to do and why."
                .to_string(),
        ),
        handle_timeout_ms: None,
        concurrency: None,
    }
}

pub struct TrustPolicy {
    /// "deny" (unattended-safe default) or "ask" (needs a wired answerer)
    ask: bool,
    grants_path: PathBuf,
    /// Reviewed calls this gate has already acted on, since it started.
    ///
    /// The ledger is still the authority — a restarted gate reads it and
    /// nothing here survives — but the ledger cannot answer this question
    /// fast enough. `Ctx::emit` buffers; the forward is appended only after
    /// `handle` returns. Two answers to the same card sitting in the mailbox
    /// together therefore both read a ledger with no forward on it, and both
    /// forwarded: the call ran twice, and two grants were written. One
    /// frontend cannot do that (it clears its card on the keystroke), but a
    /// daemon with two clients folds the same card for each of them, and
    /// nothing about "install this component" is safe to do twice.
    settled: std::collections::HashSet<String>,
    /// Calls still waiting on this gate after their mailbox delivery ended.
    waiting: std::collections::HashSet<String>,
}

#[cfg(test)]
mod audit_tests;

impl TrustPolicy {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        let grants_path = get("grants")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(default_grants_path);
        Self {
            ask: get("stance").and_then(Value::as_str) == Some("ask"),
            grants_path,
            settled: Default::default(),
            waiting: Default::default(),
        }
    }

    fn granted(&self, key: &str, effects: &Value) -> bool {
        let Ok(text) = std::fs::read_to_string(&self.grants_path) else {
            return false; // no file yet = nothing granted
        };
        let Ok(store) = serde_json::from_str::<Value>(&text) else {
            return false; // an unreadable store grants nothing
        };
        store["grants"].as_array().is_some_and(|grants| {
            grants
                .iter()
                .any(|g| g["key"] == key && &g["effects"] == effects)
        })
    }

    fn record_grant(
        &self,
        key: &str,
        admits: &str,
        effects: &Value,
        summary: &str,
        by: &str,
    ) -> Result<(), String> {
        use std::io::Write;
        let save = || -> Result<(), String> {
            if let Some(dir) = self
                .grants_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            // Lock a stable sidecar, not the inode replaced by rename. This
            // serializes the entire read-modify-write across streams/processes.
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(self.grants_path.with_extension("json.lock"))
                .map_err(|e| e.to_string())?;
            lock.lock().map_err(|e| e.to_string())?;
            let mut store: Value = match std::fs::read_to_string(&self.grants_path) {
                Ok(text) => {
                    serde_json::from_str(&text).map_err(|e| format!("invalid grants file: {e}"))?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"grants": []}),
                Err(e) => return Err(e.to_string()),
            };
            let grants = store["grants"]
                .as_array_mut()
                .ok_or("grants must be an array")?;
            grants.push(json!({"key": key, "admits": admits, "effects": effects,
                "summary": summary, "granted_by": by}));
            let temporary = self.grants_path.with_extension("json.writing");
            let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
            file.write_all(
                serde_json::to_string_pretty(&store)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, &self.grants_path).map_err(|e| e.to_string())?;
            Ok(())
        };
        save().map_err(|e| {
            format!(
                "could not persist authorization to {}: {e}",
                self.grants_path.display()
            )
        })
    }

    /// The surface this tool was declared with on the model call that asked
    /// for THIS request — judged on the record, never on the name, and never
    /// on a declaration some other component put there (see
    /// [`ce::declared_effects`]).
    fn fail_history(&self, ctx: &mut Ctx, error: impl ToString) {
        ctx.fail(
            "read admission history",
            error.to_string(),
            &self.waiting.iter().cloned().collect::<Vec<_>>(),
        );
    }

    fn declared_effects(
        &self,
        ctx: &Ctx,
        request_id: &str,
        tool: &str,
    ) -> Result<Option<Value>, String> {
        // By id, so the walk touches the request's few ancestors instead of
        // copying the conversation to find them.
        //
        // The tool list is a document: past a size it lives in a file beside
        // the ledger, and a real one is 13 KB, so this is the normal case
        // rather than the exception. Read the declarations, not the reference
        // to them — a gate that cannot see a declaration treats the call as
        // undeclared, which is the safe direction but the wrong answer.
        let offered = ce::try_declared_effects::<String>(
            |id| {
                let Some(mut event) = ctx.log().get(id).map_err(|e| e.to_string())? else {
                    return Ok(None);
                };
                event.payload["tools"] = ctx.document(&event.payload["tools"])?;
                Ok(Some(event))
            },
            request_id,
            tool,
        )?;
        // When no historical surface was offered (including direct frontend
        // calls), use the actual assembled provider, never caller-supplied
        // effects. A non-null historical surface always wins.
        Ok(offered.or_else(|| {
            ctx.tool_decls()
                .into_iter()
                .find(|declaration| declaration["name"] == tool)
                .and_then(|declaration| declaration.get("effects").cloned())
                .filter(|effects| !effects.is_null())
        }))
    }

    fn review(&mut self, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("").to_string();
        let effects = match self.declared_effects(ctx, &event.id, &tool) {
            Ok(effects) => effects.unwrap_or(Value::Null),
            Err(error) => {
                self.fail_history(ctx, error);
                return;
            }
        };
        let Some(admits) = effects["admits"].as_str().map(str::to_string) else {
            // Not an admission: this gate has no opinion — forward untouched
            // (a re-emission with a causal link, the audit-visible hop)
            ctx.emit(
                "forward",
                EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], event.payload.clone()),
            );
            return;
        };

        let key = admission_key(&event.payload["arguments"]);
        let summary = event.payload["admissionReview"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| admission_summary(&tool, &event.payload["arguments"]));
        if self.granted(&key, &effects) {
            ctx.emit(
                "forward",
                EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], event.payload.clone()),
            );
            return;
        }

        if self.ask {
            self.waiting.insert(event.id.clone());
            // The event pair, first half: put the question on the record and
            // answer nothing — the turn waits for the human
            ctx.emit(
                "request",
                EventDraft::new(
                    AUTH_REQUESTED,
                    &[&event.id],
                    json!({
                        "request": event.id,
                        "key": key,
                        "tool": tool,
                        "admits": admits,
                        "effects": effects,
                        "summary": summary,
                    }),
                ),
            );
            return;
        }

        // Unattended stance: refuse, and say exactly how to grant
        ctx.emit(
            "decision",
            EventDraft::new(
                DECISION,
                &[&event.id],
                json!({"verdict": "denied", "key": key, "admits": admits}),
            )
            .with_reason(&format!(
                "ungranted admission ({summary}); the unattended stance refuses rather than asks"
            )),
        );
        ctx.emit(
            "verdict",
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&event.id],
                json!({
                    "call": event.payload["call"],
                    "status": "error",
                    "error": {
                        "code": "trust.not_granted",
                        "message": format!(
                            "not granted: {summary}. Ask the user to add a grant with key \
                             {key:?} to {} (canon: schemas/trust_grants.json), or run an \
                             assembly whose trust stance is \"ask\" with a frontend that \
                             can answer.",
                            self.grants_path.display()
                        ),
                        "blame": "request",
                        "retryable": false,
                        "transient": false,
                    },
                }),
            ),
        );
    }

    /// The pair's second half. Everything is read off the ledger: the answer
    /// names the request event, the request event names the reviewed call,
    /// and a call already forwarded or answered is not acted on twice.
    fn answer(&mut self, event: &EventEnvelope, ctx: &mut Ctx) {
        if event.payload["channel"] != AUTH_CHANNEL {
            return; // external input on some other channel is not for us
        }
        let Some(request_id) = event.payload["request"].as_str() else {
            return; // malformed answers are noise, not verdicts
        };
        let requested = match ctx.log().get(request_id) {
            Ok(Some(requested)) => requested,
            Ok(None) => return,
            Err(error) => {
                self.fail_history(ctx, error);
                return;
            }
        };
        if requested.event_type != AUTH_REQUESTED {
            return;
        }
        let reviewed = match requested.payload["request"]
            .as_str()
            .map(|id| ctx.log().get(id))
            .transpose()
        {
            Ok(Some(Some(reviewed))) => reviewed,
            Ok(_) => return,
            Err(error) => {
                self.fail_history(ctx, error);
                return;
            }
        };
        // Idempotence: one reviewed call, one outcome. Off the ledger, which
        // survives restarts — and off memory, which the ledger cannot help
        // with, because what this handler is about to emit is not appended
        // until it returns. See `TrustPolicy::settled`.
        let settled = (|| -> std::io::Result<bool> {
            Ok(self.settled.contains(&reviewed.id)
                || ctx.log().has_outcome(&reviewed.id)?
                || ctx.log().any_header(|e| {
                    e.causes.contains(&reviewed.id)
                        && e.event_type == ce::TOOL_EXEC_STARTED
                        && e.id != requested.id
                })?)
        })();
        let already_settled = match settled {
            Ok(settled) => settled,
            Err(error) => {
                self.fail_history(ctx, error);
                return;
            }
        };
        self.waiting.remove(&reviewed.id);
        if already_settled {
            return;
        }
        self.settled.insert(reviewed.id.clone());

        let key = requested.payload["key"].as_str().unwrap_or("").to_string();
        let admits = requested.payload["admits"].as_str().unwrap_or("");
        let summary = requested.payload["summary"].as_str().unwrap_or("");
        let causes = [reviewed.id.as_str(), event.id.as_str()];

        if event.payload["approve"] == true {
            if let Err(problem) = self.record_grant(
                &key,
                admits,
                &requested.payload["effects"],
                summary,
                &event.id,
            ) {
                ctx.emit(
                    "verdict",
                    EventDraft::new(
                        ce::TOOL_EXEC_COMPLETED,
                        &causes,
                        json!({"call": reviewed.payload["call"], "status": "error", "error": {
                            "code": "trust.persistence", "message": problem, "blame": "environment",
                            "retryable": false, "transient": false
                        }}),
                    ),
                );
                return;
            }
            ctx.emit(
                "decision",
                EventDraft::new(
                    DECISION,
                    &causes,
                    json!({"verdict": "granted", "key": key, "admits": admits}),
                )
                .with_reason(&format!("the user authorized this admission ({summary})")),
            );
            // The forward joins the reviewed call and the human's answer —
            // two causes converging on one consequence
            ctx.emit(
                "forward",
                EventDraft::new(ce::TOOL_EXEC_STARTED, &causes, reviewed.payload.clone()),
            );
        } else {
            ctx.emit(
                "decision",
                EventDraft::new(
                    DECISION,
                    &causes,
                    json!({"verdict": "denied", "key": key, "admits": admits}),
                )
                .with_reason(&format!("the user refused this admission ({summary})")),
            );
            ctx.emit(
                "verdict",
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &causes,
                    json!({
                        "call": reviewed.payload["call"],
                        "status": "error",
                        "error": {
                            "code": "trust.refused",
                            "message": format!("the user refused this admission ({summary})"),
                            "blame": "request",
                            "retryable": false,
                            "transient": false,
                        },
                    }),
                ),
            );
        }
    }
}

impl Component for TrustPolicy {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        match port {
            "answer" => self.answer(event, ctx),
            _ => self.review(event, ctx),
        }
    }
}

// ── Helpers ─────────────────────────────────────────────

fn default_grants_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".lattice").join("trust.json")
}

/// The admission's identity: sha256 of the call's arguments in canonical
/// (key-sorted) JSON, `reason` excluded — the reason explains, it does not
/// identify. Content-addressed: the same source string or the same inline
/// code yields the same key, whatever the call was named.
pub fn admission_key(arguments: &Value) -> String {
    let mut args = arguments.clone();
    if let Some(map) = args.as_object_mut() {
        map.remove("reason");
    }
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(&args));
    format!("sha256:{:x}", hasher.finalize())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<&String, String> =
                map.iter().map(|(k, v)| (k, canonical_json(v))).collect();
            let inner: Vec<String> = sorted
                .into_iter()
                .map(|(k, v)| format!("{}:{v}", Value::String(k.clone())))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// One human-readable line naming what is being admitted.
fn admission_summary(tool: &str, arguments: &Value) -> String {
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| arguments[k].as_str())
            .unwrap_or("<inline content>")
            .to_string()
    };
    format!("{tool}: {}", pick(&["source", "instance", "tool_name"]))
}
