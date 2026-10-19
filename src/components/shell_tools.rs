//! The `Run` tool: execute a bash command. This is the `executes` effect
//! surface made real — the most dangerous kind, and exactly what an
//! effects-policy gate exists to hold. The tool declares `executes: true`;
//! whether it is allowed to run at all is the policy's call, not this
//! component's.
//!
//! No path or command confinement lives here: bash can `cd` and reach
//! anything the process can. There is no sandbox yet (WASM is future work).
//! Cancellation kills a command while its delivery still owns it. A handed
//! off command runs independently; interrupting the conversation does not
//! kill it. Its pid/process group remains available for explicit stopping.

mod capture;
mod recovery;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, EffectSurface, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "shell-tools";

// Both default and workspace-specific fragments keep the same workflow rules.
const RUN_GUIDANCE: &str =
    "A non-zero exit from `Run` comes back as data for you to read, not as an \
     error: the command ran, and what it said is the answer. Do not rerun a \
     command merely to recover omitted output; read its saved logs instead. \
     Run saves complete output itself; do not redirect it merely to preserve a log. \
     By default wait for the result: long commands hand off automatically without \
     another model call just to check progress. Use background=true only when \
     you have independent work to continue, or are starting a persistent service. \
     Completion arrives as a wake; do not poll for completion.";

pub fn tool_decls() -> Vec<Value> {
    vec![json!({
        "name": "Run",
        "description": "Run a bash command and return its stdout, stderr and exit code. \
            A non-zero exit is reported as data, not a tool error. Long output keeps its \
            HEAD and its TAIL; full output is streamed to the returned log paths. \
            Read or Grep those files for omitted output; do not rerun the command to recover it. \
            Logs marked complete=false are partial; background logs remain live until sealed. \
            By default wait for the result. A command still running after a brief foreground \
            window hands off automatically: the conversation waits without model polling, \
            while new user input can resume it. Set background=true only to continue independent \
            work immediately or start a persistent service. Handed-off commands return a job id \
            and pid, and wake you with their result when finished. They are NOT stopped by \
            a conversation interrupt; to stop one, run \
            'kill -TERM -<pid>' (the negative pid kills its whole process group). \
            Every result carries the `cwd` it ran in, and each call starts fresh there — \
            a `cd` inside a command does not survive to the next one. To run somewhere \
            else, pass `cwd` rather than prefixing `cd`.",
        "parameters": {
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "cwd": {"type": "string", "description": "directory to run in; \
                    defaults to the one every result reports back"},
                "background": {"type": "boolean"},
            },
            "required": ["command"],
        },
        // A shell does not merely EXECUTE: through the program it starts it
        // reads, writes and reaches the network too. Declaring only `executes`
        // let a policy that forbade writes forward this tool anyway — the rule
        // never fired, because the declaration never mentioned writing. The
        // surface must name everything the tool can reach, not the narrowest
        // thing it is usually used for; narrowness is earned by a tool that
        // STRUCTURALLY cannot do more (see `Ls`), never by a wide one
        // describing itself modestly.
        "effects": {
            "executes": true,
            "reads": ["*"],
            "writes": ["*"],
            "network": ["*"],
        },
        // Temporality (与作用面同构的元数据，剥离不发供应商): this tool may run
        // in the background — the fifth tool dimension. optional = caller chooses.
        "async": "optional",
    })]
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
            // A background command wakes the loop when it finishes — wire
            // this to the loop's input, same as a user message
            PortDecl::new("wake", &[ce::WAKE]),
        ],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: Some(EffectSurface {
            executes: true,
            reads: vec!["*".to_string()],
            writes: vec!["*".to_string()],
            network: vec!["*".to_string()],
            ..Default::default()
        }),
        implements: vec!["tool-provider".to_string()],
        tools: tool_decls(),
        prompt: Some(
            // Where commands start is now answered where it can be CHECKED —
            // the schema says it and every result reports the directory it ran
            // in — rather than asserted in a prompt the model has no way to
            // verify. Telling it once did not work: 16 of 16 commands in a real
            // session still arrived with `cd <repo> &&` glued to the front.
            RUN_GUIDANCE.to_string(),
        ),
        // The watchman: a command may not run longer than this before the
        // kernel cancels it (and this tool kills the child)
        handle_timeout_ms: Some(120_000),
        // Several at once. A model that asks for three commands in one turn
        // means them to run together; with one worker the second waits for the
        // first, and a real session spent 90 seconds on three 30-second
        // commands. Safe here because nothing is kept between deliveries.
        concurrency: Some(8),
    }
}

/// The fragment for an instance that was pointed at a specific directory. The
/// manifest's own wording answers the default case (commands start where the
/// process does); a confined instance starts somewhere the environment
/// fragment never named, so it has to name it itself.
pub fn prompt_for(cwd: &str) -> String {
    format!(
        "`Run` starts its commands in {cwd}, which is not your own working directory. \
         That is the one thing here a result cannot teach you before the first call.\n\
         {RUN_GUIDANCE}"
    )
}

pub struct ShellTools {
    /// Working directory commands start in (not a boundary — bash can leave)
    cwd: String,
    /// Largest stdout/stderr returned; extra is truncated with a marker.
    ///
    /// This is a context budget, not a safety limit. It was 1 MiB, which is
    /// roughly 350k tokens — larger than the window the result has to fit
    /// into, so a single command could put the conversation over the edge
    /// before anything downstream got a say (`tail -20` of a ledger did
    /// exactly that: 131 KB in one result, and the next call's cache hit rate
    /// fell from 99.5% to 26.4%). The context gate only trims once the WHOLE
    /// context crosses its threshold; it has no opinion about one oversized
    /// result, so the bound belongs here.
    max_bytes: usize,
    exclusive: bool,
    yield_after: Duration,
}

struct RunRequest<'a> {
    command: &'a str,
    cwd: &'a str,
    call: Value,
    started_id: &'a str,
    background: bool,
}

struct RunningCommand {
    child: Child,
    out: capture::Reader,
    err: capture::Reader,
    live_logs: Value,
}

impl ShellTools {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        Self {
            cwd: get("cwd")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string(),
            max_bytes: get("maxBytes").and_then(Value::as_u64).unwrap_or(16_384) as usize,
            exclusive: get("exclusive").and_then(Value::as_bool).unwrap_or(false),
            // Hand off before the delivery watchdog. Zero is useful for
            // deterministic handoff tests; callers do not need a timeout knob.
            yield_after: Duration::from_millis(
                get("yieldAfterMs")
                    .and_then(Value::as_u64)
                    .unwrap_or(1_000)
                    .min(30_000),
            ),
        }
    }

    fn run(&self, request: &RunRequest<'_>, ctx: &mut Ctx) -> Option<Value> {
        let command = request.command;
        let cwd = request.cwd;
        let (out_log, err_log) = match captures(ctx, self.max_bytes) {
            Ok(logs) => logs,
            Err(error) => return Some(error),
        };
        let live_logs =
            json!({"stdout": out_log.live_reference(), "stderr": err_log.live_reference()});
        let mut child = match spawn(command, cwd) {
            Ok(child) => child,
            Err(e) => {
                return Some(json!({"status": "error", "error": {
                    "code": "tool.spawn_failed",
                    "message": e.to_string(),
                    "blame": "environment",
                    "retryable": false,
                    "transient": false,
                }}))
            }
        };

        // Drain both pipes on their own threads so a chatty command cannot
        // deadlock by filling a pipe while we wait.
        //
        // Into a SHARED buffer, a chunk at a time, rather than reading to the
        // end and handing back the whole thing. The end of a pipe is not the
        // end of the command: `something &` leaves a grandchild holding the
        // same stdout after bash has exited, so a reader waiting for EOF waits
        // for the background process — minutes, or forever, on a call the
        // model thinks is a quick one. That wait outlived the watchman's
        // deadline, ignored the cancellation it raised (there is no way to
        // interrupt a blocking read), and ended with the whole shell instance
        // declared unresponsive: no `Run` for the rest of the session, from
        // one command with an ampersand on the end.
        let out = capture::drain(child.stdout.take().expect("stdout piped"), out_log);
        let errp = capture::drain(child.stderr.take().expect("stderr piped"), err_log);

        // An explicit background call yields immediately. Otherwise give a
        // short command its ordinary result without an intermediate receipt.
        if request.background {
            self.handoff(
                RunningCommand {
                    child,
                    out,
                    err: errp,
                    live_logs,
                },
                request,
                ctx,
            );
            return None;
        }
        let began = Instant::now();
        let cancelled = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Err(status),
                Ok(None) => {
                    if ctx.cancelled() {
                        kill_group(&mut child);
                        break Ok(());
                    }
                    if began.elapsed() >= self.yield_after {
                        self.handoff(
                            RunningCommand {
                                child,
                                out,
                                err: errp,
                                live_logs,
                            },
                            request,
                            ctx,
                        );
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    kill_group(&mut child);
                    let (stdout, stderr, logs, _) = capture::finish(&out, &errp);
                    return Some(json!({"status": "error", "error": {
                        "code": "tool.wait_failed",
                        "message": e.to_string(),
                        "blame": "environment",
                        "retryable": false,
                        "transient": false,
                    }, "result": {"cwd":cwd,"stdout":stdout,"stderr":stderr,"logs":logs}}));
                }
            }
        };

        // The command is over; the pipe may not be. Give the readers a moment
        // to finish the tail, then take whatever has arrived and say so —
        // waiting on a background process is what must not happen here.
        let (stdout, stderr, logs, still_open) = capture::finish(&out, &errp);
        // Said plainly, because the alternative is a model concluding from an
        // empty result that its command printed nothing.
        let left_running = still_open.then_some(
            "the command left something running in the background; \
             output after this point is not captured",
        );
        Some(match cancelled {
            Ok(()) => {
                json!({"status": "cancelled", "result": {
                    "cwd": cwd, "stdout": stdout, "stderr": stderr,
                    "logs": logs, "note": left_running,
                }})
            }
            // The directory is reported, not merely configured. A model
            // cannot check a claim in its prompt, and a `cd <repo> &&` prefix
            // costs it nothing — so it prefixed one onto every command it ever
            // ran. Telling it where the command actually ran turns that from
            // something it is told into something it has seen. It is also the
            // audit answer: the ledger could not previously say WHERE a
            // command executed.
            Err(status) => json!({"status": "ok", "result": {
                "exit_code": status.code().unwrap_or(-1),
                "cwd": cwd,
                "stdout": stdout,
                "stderr": stderr,
                "logs": logs, "note": left_running,
            }}),
        })
    }

    /// Complete the tool call with a receipt, then deliver the process result
    /// as a wake. Waiting belongs to the loop, not an unresolved tool call:
    /// a person can speak while the process continues on its own.
    fn handoff(&self, running: RunningCommand, request: &RunRequest<'_>, ctx: &mut Ctx) {
        let RunningCommand {
            mut child,
            out,
            err: errp,
            live_logs,
        } = running;
        let cwd = request.cwd;
        let call = &request.call;
        let started_id = request.started_id;
        let job = call.as_str().unwrap_or(started_id).to_string();
        let command_preview: String = request
            .command
            .chars()
            .take(160)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let pid = child.id();
        let mut ack = json!({"status": "ok", "result": {
            "logs": live_logs,
            "background": true,
            "job": job,
            "pid": pid,
            "cwd": cwd,
            "note": "running in the background; you will be woken when it finishes. \
                     Stop it with run(\"kill -TERM -<pid>\").",
        }});
        ack["call"] = call.clone();
        ack["result"]["commandPreview"] = json!(command_preview);
        if !request.background {
            ack["continuation"] = json!("wait");
            ack["result"]["note"] = json!("Waiting for completion without polling. New input can resume the conversation; the process keeps running. Stop it with Run(\"kill -TERM -<pid>\").");
        }
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[started_id], ack),
        );

        // The detached waiter: on exit, wake the loop with the result
        let injector = ctx.injector();
        let cwd = cwd.to_string();
        let started = started_id.to_string();
        std::thread::spawn(move || {
            let output = child.wait();
            let (stdout, stderr, logs, still_open) = capture::finish(&out, &errp);
            let (summary, body) = match output {
                Ok(status) => {
                    let code = status.code().unwrap_or(-1);
                    (
                        format!("background command finished: exit {code} — {command_preview}"),
                        json!({
                            "job": job,
                            "commandPreview": command_preview,
                            "exit_code": code,
                            "stdout": stdout, "stderr": stderr, "logs": logs, "cwd": cwd,
                            "note": still_open.then_some("inherited pipes remain open; sealed logs contain only output captured before completion"),
                        }),
                    )
                }
                Err(e) => (
                    format!("background command failed to run: {e}"),
                    json!({"job": job, "error": e.to_string(), "stdout": stdout, "stderr": stderr, "logs": logs, "cwd": cwd}),
                ),
            };
            injector.emit(
                "wake",
                EventDraft::new(
                    ce::WAKE,
                    &[&started],
                    json!({"source": format!("background:{job}"), "summary": summary, "body": body}),
                ),
            );
        });
    }
}

/// Spawn `bash -c <command>` in its OWN process group, so a kill (foreground
/// cancel, or a later `kill -<pid>` from the agent) reaches the whole tree,
/// not just the bash parent. The child's pid is then also its group id.
fn spawn(command: &str, cwd: &str) -> std::io::Result<Child> {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
}

/// Kill a child's whole process group and reap it. On unix that is
/// `kill(-pid, SIGKILL)`; elsewhere, best-effort kill of the child.
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

fn captures(ctx: &Ctx, max: usize) -> Result<(capture::Capture, capture::Capture), Value> {
    let create = || -> Result<_, String> {
        let dir = super::tool_artifacts::directory(ctx.ledger_path())?;
        Ok((
            capture::Capture::new(&dir, max.min(65_536), "stdout-")?,
            capture::Capture::new(&dir, max.min(65_536), "stderr-")?,
        ))
    };
    create().map_err(|e| {
        json!({"status": "error", "error": {
            "code": "tool.capture_setup", "message": e, "blame": "environment",
            "retryable": false, "transient": false,
        }})
    })
}

impl Component for ShellTools {
    /// On reopen, settle any background job whose completion wake never came:
    /// the waiter thread died with the process, and its orphaned child cannot
    /// be re-waited (a foreign pid has no exit code for us). Re-running would
    /// redo a side effect, so we do not — we emit a wake that tells the agent
    /// the job was cut off (outcome unknown). It fires no PROACTIVE wake, so a
    /// restart costs no model call; the settle rides the next turn. Idempotent:
    /// any wake already caused by the start (a real finish OR a prior settle)
    /// means the job is resolved and is left alone.
    fn restore(&mut self, ctx: &mut Ctx) {
        let restored = match recovery::recover(ctx.log()) {
            Ok(restored) => restored,
            Err(error) => {
                ctx.fail("restore background commands", error.to_string(), &[]);
                return;
            }
        };
        if let Some(reason) = restored.cold_reason {
            eprintln!("slow recovery for background commands: {reason}");
        }
        for recovery::Job {
            started: started_id,
            job,
            pid,
            command,
        } in restored.jobs
        {
            ctx.emit(
                "wake",
                EventDraft::new(
                    ce::WAKE,
                    &[started_id.as_str()],
                    json!({
                        "source": format!("background:{}", job.as_str().unwrap_or("job")),
                        "summary": "background command cut off by restart; outcome unknown",
                        "body": {
                            "job": job,
                            "interrupted": "restart",
                            "pid": pid,
                            "command": command,
                        },
                    }),
                ),
            );
        }
    }

    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or("");
        if tool != "Run" {
            if self.exclusive {
                let mut payload = json!({"status": "error", "error": {
                    "code": "tool.unknown",
                    "message": format!("unknown tool: {tool}"),
                    "blame": "request",
                    "retryable": false,
                    "transient": false,
                }});
                payload["call"] = event.payload["call"].clone();
                ctx.emit(
                    "outcome",
                    EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
                );
            }
            return; // fan-out convention: silence on foreign tools
        }
        let command = event.payload["arguments"]["command"].as_str().unwrap_or("");
        // A directory is a parameter, not something to splice into the command
        // string: `cd x && ...` hides the destination inside an opaque blob,
        // while this lands on the ledger as its own field.
        let cwd = event.payload["arguments"]["cwd"]
            .as_str()
            .unwrap_or(&self.cwd)
            .to_string();
        let background = event.payload["arguments"]["background"]
            .as_bool()
            .unwrap_or(false);
        let request = RunRequest {
            command,
            cwd: &cwd,
            call: event.payload["call"].clone(),
            started_id: &event.id,
            background,
        };
        if let Some(mut payload) = self.run(&request, ctx) {
            payload["call"] = request.call;
            ctx.emit(
                "outcome",
                EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
            );
        }
    }
}
