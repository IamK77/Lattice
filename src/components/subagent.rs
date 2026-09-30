//! Subagent — handing a piece of work to a second agent and getting one answer
//! back.
//!
//! This is NOT the scheduling layer the backlog is waiting on, and it is
//! deliberately not named after it. There is no scheduling here: no queue, no
//! priority, no task graph, no arbitration between agents competing for
//! anything. One request opens one stream, immediately. What it did prove is
//! the standing promise that layer will be held to — it connected without
//! changing one line of the four contracts or the envelope.
//!
//! One tool, `ask`, does two things. Name no expert and it answers at once with
//! the list of who is available; name one and it hands the work over. The same
//! sentence describes the work both times, so finding someone and sending them
//! costs one wording, not two.
//!
//! An expert is nothing exotic: a set of tools and a prompt. Which tools it has
//! is not a promise it makes, it is the assembly of the stream it runs in —
//! an expert that reads has no writing component in it at all.
//!
//! Handing work over is shaped exactly like `Run`: foreground waits for the
//! answer, background comes back with a number and wakes you later. The two
//! differ in who writes the call's outcome, because the work does not happen
//! here:
//!
//! - **background** — this component writes the outcome now (a job number),
//!   and the host writes a wake when the answer arrives.
//! - **foreground** — this component writes NOTHING, and the host writes the
//!   outcome when the answer arrives. Until then the call is simply open, the
//!   ordinary state of any tool that takes a while.
//!
//! That difference decides what `restore` may touch. A background job's call
//! was answered long ago, so a reopen has to say separately that the job died —
//! nobody else will. A foreground call is still open on the ledger, and the
//! kernel already has a rule for open calls at reopen: it writes `interrupted`.
//! Writing a second notice here would tell the model the same job died twice.
//! Whether a call is finished is a question this codebase has answered in
//! several places already, and the wrong answers were wrong in opposite
//! directions — so this component answers it for background jobs only.

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope, EventTypeDecl};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "subagent";

/// This component's own letterhead. "Expert" and "job" are ITS words and they
/// stay on its letters — the core never learns them, exactly as it never
/// learned "session".
pub const STREAM_REQUESTED: &str = "subagent.stream.requested";

const ASK: &str = "ask";
pub const CANCEL: &str = "CancelExpert";
pub const CANCEL_REQUESTED: &str = "subagent.stream.cancel_requested";

pub fn tool_decls() -> Vec<Value> {
    vec![
        json!({
            "name": ASK,
            "description": "Find an expert, or hand work to one. Call it with only `prompt` and \
                it answers immediately with the experts available and what each is for; call it \
                again naming one and that expert does the work. An expert is a second agent with \
                its own fresh context and its own tools — it CANNOT see this conversation, so \
                `prompt` must stand on its own: what to do, where to look, what to report back. \
                By default the work runs in the background and its answer reaches you later as a \
                wake; pass background=false to wait for it instead. Hand over work whose PROCESS \
                you do not need to see — a wide search, reading many files to settle one \
                question. Do not hand over work where watching each step is the point, such as \
                editing code.",
            "parameters": {
                "type": "object",
                "properties": {
                    "prompt": {"type": "string"},
                    "expert": {"type": "string", "description": "Omit to list the experts"},
                    "background": {"type": "boolean", "description": "Default true"},
                },
                "required": ["prompt"],
            },
            // An expert reaches wherever its own assembly reaches, and this
            // declaration is shared by all of them. Declaring anything narrower
            // would show a policy gate a harmless-looking errand; what actually
            // bounds one expert is the set of components in its stream.
            "effects": {
                "reads": ["*"],
                "writes": ["*"],
                "network": ["*"],
                "executes": true,
            },
            // Either way, decided per call — the same as `Run`
            "async": "optional",
        }),
        json!({
            "name": CANCEL,
            "description": "Request cancellation of an expert job delegated by this stream. Requires its job number and a reason. Acceptance is not completion: the expert reports its final disposition separately. Already performed actions are not rolled back; uncooperative in-process work may have an unknown outcome.",
            "parameters": {
                "type": "object",
                "properties": {
                    "job": {"type": "integer", "minimum": 1},
                    "reason": {"type": "string", "minLength": 1}
                },
                "required": ["job", "reason"],
                "additionalProperties": false
            },
            "effects": {"reads": [], "writes": ["expert jobs in this stream"], "network": [], "executes": false, "reversible": false}
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
            // Written here for a listing, for a refusal, and for a background
            // hand-off; written by the HOST for a foreground one.
            PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED]),
            // Read by whoever owns the StreamHost — no wire carries it
            PortDecl::new("request", &[STREAM_REQUESTED, CANCEL_REQUESTED]),
            // A background answer arrives the way a finished command's does
            PortDecl::new("wake", &[ce::WAKE]),
        ],
        events: vec![
            EventTypeDecl::new(
                STREAM_REQUESTED,
                "A stream is wanted so an expert can do this work",
            )
            .with_schema(json!({
                "type": "object",
                "required": ["job", "expert", "prompt", "background", "call"],
                "properties": {
                    "job": {"type": "integer"},
                    "expert": {"type": "string"},
                    "prompt": {"type": "string"},
                    "background": {"type": "boolean"},
                    "call": {},
                    "execution": {"type": "object"},
                },
            })),
            EventTypeDecl::decision(
                CANCEL_REQUESTED,
                "The parent requests cancellation of its own expert job",
            )
            .with_schema(json!({
                "type": "object",
                "required": ["job", "call", "reason"],
                "properties": {
                    "job": {"type": "integer", "minimum": 1},
                    "call": {},
                    "reason": {"type": "string", "minLength": 1}
                }
            })),
        ],
        default_wiring: Vec::new(),
        capabilities: Some(EffectSurface {
            reads: vec!["*".to_string()],
            writes: vec!["*".to_string()],
            network: vec!["*".to_string()],
            executes: true,
            ..Default::default()
        }),
        implements: vec!["tool-provider".to_string()],
        tools: tool_decls(),
        prompt: Some(
            "An expert starts with a blank context and cannot read this conversation, so \
             everything it needs has to be in the prompt you hand it. Ask with no expert \
             named first if you do not know who to send. An expert cannot ask for another \
             expert."
                .to_string(),
        ),
        handle_timeout_ms: None,
        concurrency: None,
    }
}

/// What the model is shown when it asks who is available. Comes from the
/// instance config, because who exists is the assembler's knowledge: the host
/// is what registers a stream template per expert, so the host is what knows
/// which names will actually open.
fn experts(config: Option<&Value>) -> Vec<Value> {
    config
        .and_then(|c| c.get("experts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub struct Subagent {
    next_job: u64,
    /// How deep this stream already is. An expert's own stream is configured
    /// `depth: 1`, and at that depth handing work on is refused. One sentence
    /// could otherwise open a tree, while neither the bill nor the panel that
    /// stops things is ready to catch one.
    depth: u64,
    roster: Vec<Value>,
    catalog: Option<Result<crate::experts::catalog::Catalog, String>>,
    exclusive: bool,
}

impl Subagent {
    pub fn from_config(config: Option<&Value>) -> Self {
        Self {
            next_job: 1,
            depth: config
                .and_then(|c| c.get("depth"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            roster: experts(config),
            catalog: config
                .and_then(|c| c.get("definitions"))
                .filter(|value| !value.is_null())
                .map(|value| {
                    serde_json::from_value::<crate::experts::catalog::Config>(value.clone())
                        .map_err(|error| error.to_string())
                        .and_then(crate::experts::catalog::Catalog::new)
                }),
            exclusive: config
                .and_then(|c| c.get("exclusive"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    fn knows(&self, name: &str) -> bool {
        self.roster.iter().any(|e| e["name"].as_str() == Some(name))
    }

    /// Decide what this call produces. Emits nothing, so the caller controls
    /// what lands and in which order.
    ///
    /// `None` for the outcome means this call is answered by the host later —
    /// the foreground hand-off, and the only path that leaves the call open.
    fn decide_with_catalog(
        &mut self,
        args: &Value,
        call: &Value,
        reader: &crate::LogReader,
    ) -> (Option<Value>, Option<Value>) {
        if self.catalog.is_none()
            || self.depth > 0
            || args["prompt"]
                .as_str()
                .is_none_or(|prompt| prompt.trim().is_empty())
        {
            return self.decide(args, call);
        }
        let requested = args["expert"].as_str().unwrap_or("").to_owned();
        if let Some(builtin) = requested.strip_prefix("builtin:") {
            let mut args = args.clone();
            args["expert"] = json!(builtin);
            return self.decide(&args, call);
        }
        let catalog = match self.catalog.as_ref().expect("catalog presence checked") {
            Ok(catalog) => catalog,
            Err(error) => return (Some(refusal("expert.unavailable", error)), None),
        };
        if requested.is_empty() {
            let mut roster = self.roster.clone();
            for entry in &mut roster {
                if let Some(name) = entry["name"].as_str() {
                    entry["name"] = json!(format!("builtin:{name}"));
                }
            }
            let mut result = json!({"experts":roster});
            match catalog.listing(Some(reader)) {
                Ok(entries) => result["experts"].as_array_mut().unwrap().extend(entries),
                Err(error) => result["customExpertError"] = json!(error),
            }
            return (Some(json!({"status":"ok","result":result})), None);
        }
        let prepared = (|| -> Result<(Value, Option<Value>), String> {
            let name = catalog.qualify(&requested, self.knows(&requested))?;
            let mut args = args.clone();
            if let Some(builtin) = name.strip_prefix("builtin:") {
                args["expert"] = json!(builtin);
                Ok((args, None))
            } else {
                let execution = catalog.resolve(&name, Some(reader))?;
                args["expert"] = json!(name);
                Ok((
                    args,
                    Some(serde_json::to_value(execution).map_err(|error| error.to_string())?),
                ))
            }
        })();
        match prepared {
            Ok((args, execution)) => self.decide_prepared(&args, call, execution),
            Err(error) => (Some(refusal("expert.unavailable", &error)), None),
        }
    }

    fn decide(&mut self, args: &Value, call: &Value) -> (Option<Value>, Option<Value>) {
        self.decide_prepared(args, call, None)
    }

    fn decide_prepared(
        &mut self,
        args: &Value,
        call: &Value,
        execution: Option<Value>,
    ) -> (Option<Value>, Option<Value>) {
        let prompt = args["prompt"].as_str().unwrap_or("").trim();
        if prompt.is_empty() {
            return (
                Some(refusal(
                    "ask.no_prompt",
                    "an expert starts with a blank context, so `prompt` cannot be empty",
                )),
                None,
            );
        }
        let Some(expert) = args["expert"].as_str().filter(|n| !n.is_empty()) else {
            // No name: this is the question "who is there", and it is answered
            // here and now. Nothing is handed over.
            return (
                Some(json!({"status": "ok", "result": {"experts": self.roster}})),
                None,
            );
        };
        if self.depth > 0 {
            return (
                Some(refusal(
                    "ask.too_deep",
                    "an expert cannot ask for another expert; do this work yourself",
                )),
                None,
            );
        }
        if execution.is_none() && !self.knows(expert) {
            return (
                Some(refusal(
                    "ask.unknown_expert",
                    &format!("no expert named {expert}; ask with no expert to see the list"),
                )),
                None,
            );
        }
        // Background unless the call says otherwise: an expert takes as long
        // as it takes, and waiting is the exception.
        let background = args["background"].as_bool().unwrap_or(true);
        let job = self.next_job;
        self.next_job += 1;
        let mut wanted = json!({
            "job": job,
            "expert": expert,
            "prompt": prompt,
            "background": background,
            "call": call,
        });
        if let Some(execution) = execution {
            wanted["execution"] = execution;
        }
        let outcome = background.then(|| json!({"status": "ok", "result": {"job": job}}));
        (outcome, Some(wanted))
    }
}

fn refusal(code: &str, message: &str) -> Value {
    json!({"status": "error", "error": {
        "code": code,
        "message": message,
        "blame": "request",
        "retryable": false,
        "transient": false,
    }})
}

impl Component for Subagent {
    /// Settle BACKGROUND jobs that never reported back, and only those.
    ///
    /// A foreground call is still open on the ledger, and the kernel settles
    /// open calls on reopen. Saying it again here would be the second notice
    /// of the same death.
    ///
    /// `ctx.emit`, not the injector: this costs no model call and rides out
    /// with the next turn, the same bargain background commands strike.
    fn restore(&mut self, ctx: &mut Ctx) {
        let mut last_job = 0u64;
        let mut requests = Vec::new();
        let mut ended = std::collections::HashSet::new();
        // Keep identities, never the full text of every historical reply.
        if let Err(error) = ctx
            .log()
            .scan_back_types(&[STREAM_REQUESTED, ce::WAKE], |event, _| {
                if event.event_type == STREAM_REQUESTED {
                    if let Some(job) = event.payload["job"].as_u64() {
                        last_job = last_job.max(job);
                        if event.payload["background"] == true {
                            requests.push((job, event.causes.first().cloned().unwrap_or_default()));
                        }
                    }
                } else if let Some(job) = event.payload["body"]["job"].as_u64() {
                    ended.insert(job);
                }
                Ok(None::<()>)
            })
        {
            ctx.fail("restore expert jobs", error.to_string(), &[]);
            return;
        }
        let Some(next_job) = last_job.checked_add(1) else {
            ctx.fail("restore expert jobs", "job identifier space exhausted", &[]);
            return;
        };
        self.next_job = next_job;
        // A wake ends a job even when it reports an interruption.
        for (job, started) in requests.into_iter().rev() {
            if ended.contains(&job) {
                continue;
            }
            ctx.emit(
                "wake",
                EventDraft::new(
                    ce::WAKE,
                    &[&started],
                    json!({
                        "source": format!("expert:{job}"),
                        "summary": format!("job {job} was cut off by a restart"),
                        "body": {
                            "job": job,
                            "interrupted": "restart",
                            "text": Value::Null,
                        },
                    }),
                ),
            );
        }
    }

    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        if tool != ASK && tool != CANCEL && !self.exclusive {
            return; // fan-out convention: silence on foreign tools
        }
        let call = event.payload["call"].clone();
        if tool == CANCEL {
            let args = &event.payload["arguments"];
            let job = args["job"].as_u64().filter(|job| *job > 0);
            let reason = args["reason"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if let (Some(job), Some(reason)) = (job, reason) {
                let mut draft = EventDraft::new(
                    CANCEL_REQUESTED,
                    &[&event.id],
                    json!({"job": job, "reason": reason, "call": call}),
                );
                draft.reason = Some(reason.to_string());
                ctx.emit("request", draft);
            } else {
                let mut payload = refusal(
                    "expert.invalid_cancel",
                    "a positive job number and nonempty reason are required",
                );
                payload["call"] = call;
                ctx.emit(
                    "outcome",
                    EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
                );
            }
            return;
        }
        let (outcome, wanted) = if tool == ASK {
            self.decide_with_catalog(&event.payload["arguments"], &call, ctx.log())
        } else {
            (
                Some(refusal("tool.unknown", &format!("unknown tool: {tool}"))),
                None,
            )
        };
        // The outcome lands FIRST where there is one. The host acts on the
        // request below, and an expert that finished before the outcome was
        // written would answer a round still waiting to hear its call was
        // accepted.
        if let Some(mut payload) = outcome {
            payload["call"] = call;
            ctx.emit(
                "outcome",
                EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
            );
        }
        if let Some(body) = wanted {
            ctx.emit(
                "request",
                EventDraft::new(STREAM_REQUESTED, &[&event.id], body),
            );
        }
    }
}
