//! The host-side half of handing work to a subagent: it opens the streams the
//! in-stream half asks for, hands each one a thread, and lets it answer on its
//! own.
//!
//! The split into halves is forced, not chosen. A component cannot open a
//! stream — only whoever holds the `StreamHost` can, and the kernel does not
//! know a `StreamHost` exists. Letting the component open one would mean
//! handing it a `StreamHost` through the kernel, which is precisely the hook
//! the core refuses to keep for anybody. So the component asks by emitting an
//! event and this half answers, which buys the thing that made those two real
//! runs auditable afterwards: the request and the answer are both on the
//! ledger, causally joined. This file could be deleted without the core
//! noticing.
//!
//! An expert is a stream template registered under the expert's name. What an
//! expert may do is therefore not a claim it makes but the set of components
//! in it: an expert that only reads has no writing component to reach for.
//!
//! **Each expert runs on a thread of its own and reports by injecting.** That
//! is the same path a finished background command travels, and it is what
//! makes "background" mean anything: the turn that handed the work over ends
//! immediately, the person gets their prompt back, and several experts run at
//! once because nothing is taking turns. Running them on the
//! host's thread would freeze the conversation for as long as the work took —
//! no typing, no interrupting — which is exactly what background is not.
//!
//! An expert answers once and is gone. There is no asking it a follow-up: that
//! is what you do with a peer, not with something you sent. What it did stays
//! readable — its ledger is a file, and the answer says where.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::kernel::host::StopHandle;

use serde_json::{json, Value};

use crate::components::subagent::{CANCEL_REQUESTED, STREAM_REQUESTED};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Injector, Kernel};
use crate::kernel::stream_host::StreamHost;

/// Default instance name of the subagent component in a parent stream — the
/// injector every answer travels back through.
pub const SUBAGENT_INSTANCE: &str = "subagent";

/// Default instance name that speaks the first message into an expert's stream.
pub const ENTRY_INSTANCE: &str = "ui";

/// A request scan advances over unrelated events without copying their bodies.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RequestBatch {
    pub through: u64,
    pub requests: Vec<EventEnvelope>,
}

fn request_batch(reader: &crate::LogReader, from_seq: u64) -> Result<RequestBatch, String> {
    let through = reader.snapshot_end();
    let mut ids = Vec::new();
    reader
        .scan_back_headers(|header, _| {
            if header.seq > through {
                return Ok(None);
            }
            // Stop at the lifetime boundary rather than traversing old lives
            // and then discarding them. Restart appends this boundary itself.
            if header.seq < from_seq
                || (header.event_type == ce::STREAM_RESUMED && header.source == "core")
            {
                return Ok(Some(()));
            }
            if matches!(
                header.event_type.as_str(),
                STREAM_REQUESTED | CANCEL_REQUESTED
            ) {
                ids.push(header.id.clone());
            }
            Ok(None)
        })
        .map_err(|e| e.to_string())?;
    ids.reverse();
    let requests = ids
        .into_iter()
        .map(|id| {
            reader
                .get(&id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("subagent request {id} missing from history"))
        })
        .collect::<Result<_, _>>()?;
    Ok(RequestBatch { through, requests })
}

/// What this half needs from whatever is holding the streams.
///
/// A trait and not `StreamHost` directly because the two hosts hold their
/// streams differently: the TUI keeps its conversation in a kernel it drives
/// itself, and the daemon gives each attached stream a thread. Neither should
/// have to change how it runs its own conversation in order to gain subagents.
pub trait Streams {
    /// Streams whose ledgers should be watched for requests.
    fn open_streams(&self) -> Vec<String>;
    /// Only subagent requests in the current lifetime, from `from_seq` onward.
    /// The cursor covers unrelated events too; a closed stream returns an empty batch.
    fn requests_from(&self, stream: &str, from_seq: u64) -> Result<RequestBatch, String>;
    /// Open a stream and hand its kernel straight over. Detached, because an
    /// expert runs on its own thread — the host never drives it.
    fn open_detached(&mut self, stream: &str, template: &str) -> Result<Kernel, String>;
    /// A captured request must never fall back to a mutable named template.
    fn open_captured(&mut self, _stream: &str, _snapshot: &Value) -> Result<Kernel, String> {
        Err("this host does not support captured expert execution".into())
    }
    /// Where that stream's record is written, so the answer can say where to
    /// go and read what it did.
    fn ledger_of(&self, stream: &str) -> Option<String>;
    /// An injector bound to one instance in a stream, owned, so it can be moved
    /// onto the expert's thread and used when the work is done.
    fn injector_for(&self, stream: &str, instance: &str) -> Option<Injector>;
}

impl Streams for StreamHost {
    fn open_streams(&self) -> Vec<String> {
        self.open_stream_ids()
    }

    fn requests_from(&self, stream: &str, from_seq: u64) -> Result<RequestBatch, String> {
        self.kernel(stream)
            .map(|k| request_batch(&k.log().reader(), from_seq))
            .transpose()
            .map(|batch| batch.unwrap_or_default())
    }

    fn open_detached(&mut self, stream: &str, template: &str) -> Result<Kernel, String> {
        self.open(stream, template).map_err(|e| e.to_string())?;
        self.take(stream)
            .ok_or_else(|| format!("{stream} vanished after opening"))
    }

    fn open_captured(&mut self, stream: &str, snapshot: &Value) -> Result<Kernel, String> {
        let execution: crate::experts::execution::Execution =
            serde_json::from_value(snapshot.clone()).map_err(|error| error.to_string())?;
        execution.open(stream, self.ledger_for(stream), self.kernel_options(stream))
    }

    fn ledger_of(&self, stream: &str) -> Option<String> {
        self.ledger_for(stream).map(|p| p.display().to_string())
    }

    fn injector_for(&self, stream: &str, instance: &str) -> Option<Injector> {
        self.injector(stream, instance)
    }
}

#[derive(Default)]
struct JobState {
    finished: bool,
    cancellation: Option<String>,
}

struct RunningJob {
    stop: StopHandle,
    state: Arc<Mutex<JobState>>,
    thread: std::thread::JoinHandle<()>,
}

pub struct SubagentHost {
    subagent_instance: String,
    entry_instance: String,
    /// How far each stream's ledger has been read. Without this every poll
    /// would re-read the whole ledger and hand out every request ever made a
    /// second time — a request stays on the record forever, and the record
    /// cannot express "this one has been taken".
    read_to: HashMap<String, u64>,
    running: HashMap<(String, u64), RunningJob>,
}

impl Default for SubagentHost {
    fn default() -> Self {
        Self::new()
    }
}

impl SubagentHost {
    pub fn new() -> Self {
        Self {
            subagent_instance: SUBAGENT_INSTANCE.to_string(),
            entry_instance: ENTRY_INSTANCE.to_string(),
            read_to: HashMap::new(),
            running: HashMap::new(),
        }
    }

    pub fn with_instances(mut self, subagent: impl Into<String>, entry: impl Into<String>) -> Self {
        self.subagent_instance = subagent.into();
        self.entry_instance = entry.into();
        self
    }

    /// Hand out every request that has appeared since the last pass.
    ///
    /// Returns without waiting for anything: each expert is now running on its
    /// own thread and will report by injecting into the stream that sent it.
    pub fn poll(&mut self, host: &mut dyn Streams) -> Result<(), String> {
        self.running.retain(|_, job| !job.thread.is_finished());
        for parent in host.open_streams() {
            let from = self.read_to.get(&parent).copied().unwrap_or(1);
            let batch = host
                .requests_from(&parent, from)
                .map_err(|e| format!("reading subagent requests in {parent}: {e}"))?;
            self.read_to
                .insert(parent.clone(), batch.through.saturating_add(1));

            for event in batch
                .requests
                .iter()
                .filter(|e| e.source == self.subagent_instance)
                .collect::<Vec<_>>()
            {
                let started = event.causes.first().cloned().unwrap_or_default();
                if event.event_type == CANCEL_REQUESTED {
                    self.cancel(host, &parent, event, &started);
                } else {
                    self.begin(host, &parent, &event.payload, &started);
                }
            }
        }
        Ok(())
    }

    fn cancel(&mut self, host: &dyn Streams, parent: &str, event: &EventEnvelope, started: &str) {
        let Some(back) = host.injector_for(parent, &self.subagent_instance) else {
            return;
        };
        let job = event.payload["job"].as_u64().unwrap_or(0);
        let reason = event.payload["reason"]
            .as_str()
            .unwrap_or("expert cancelled");
        let status = self
            .running
            .get(&(parent.to_string(), job))
            .and_then(|running| {
                let mut state = running.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.finished {
                    return None;
                }
                if state.cancellation.is_some() {
                    return Some("already_requested");
                }
                let origin = Some(crate::StreamRef {
                    stream: parent.to_string(),
                    event: event.id.clone(),
                });
                if !running.stop.request(reason.to_string(), origin) {
                    return None;
                }
                state.cancellation = Some(reason.to_string());
                Some("requested")
            });
        let mut payload = match status {
            Some(status) => json!({"status": "ok", "result": {"job": job, "cancellation": status}}),
            None => json!({"status": "error", "error": {
                "code": "expert.not_running", "message": "no running expert job with this number belongs to this stream",
                "blame": "request", "retryable": false, "transient": false
            }}),
        };
        payload["call"] = event.payload["call"].clone();
        back.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[started], payload),
        );
    }

    /// Start one expert: open its stream, take its kernel, and give it a thread.
    fn begin(&mut self, host: &mut dyn Streams, parent: &str, body: &Value, started: &str) {
        let (Some(job), Some(expert), Some(prompt)) = (
            body["job"].as_u64(),
            body["expert"].as_str(),
            body["prompt"].as_str(),
        ) else {
            return;
        };
        let work = Work {
            job,
            background: body["background"].as_bool().unwrap_or(true),
            call: body["call"].clone(),
            started: started.to_string(),
            parent: parent.to_string(),
            child: format!("{parent}-sub-{job}"),
        };
        // The answer travels back through the requesting component's own port,
        // so no assembly has to know this file exists.
        let Some(back) = host.injector_for(parent, &self.subagent_instance) else {
            return;
        };
        // Captured recipes are authoritative. Invalid snapshots fail rather
        // than silently falling back to today's same-named template. Neither
        // path hands the child a reader on its parent's conversation.
        let opened = match body.get("execution") {
            Some(snapshot) => host.open_captured(&work.child, snapshot),
            None => host.open_detached(&work.child, expert),
        };
        let kernel = match opened {
            Ok(kernel) => kernel,
            Err(problem) => {
                // A call always ends. Failing to open is an ending like any
                // other, and saying nothing would leave the round waiting on an
                // expert that was never there.
                back.emit_now(
                    &work,
                    None,
                    ExpertReport::failure("ask.no_stream", &problem),
                );
                return;
            }
        };
        let entry = kernel.injector(&self.entry_instance);
        let ledger = host.ledger_of(&work.child);
        let prompt = prompt.to_string();
        let stop = kernel.stop_handle();
        let state = Arc::new(Mutex::new(JobState::default()));
        let worker_state = state.clone();
        let thread = std::thread::spawn(move || {
            run_expert(kernel, entry, back, work, prompt, ledger, worker_state)
        });
        self.running.insert(
            (parent.to_string(), job),
            RunningJob {
                stop,
                state,
                thread,
            },
        );
    }
}

impl SubagentHost {
    /// Signal all children before waiting for any one of them.
    pub fn request_shutdown(&self) {
        for running in self.running.values() {
            let mut state = running.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.finished && state.cancellation.is_none() {
                let reason = "parent host is shutting down".to_string();
                running.stop.request(reason.clone(), None);
                state.cancellation = Some(reason);
            }
        }
    }
}

impl Drop for SubagentHost {
    fn drop(&mut self) {
        self.request_shutdown();
        for (_, running) in self.running.drain() {
            let _ = running.thread.join();
        }
    }
}

#[cfg(test)]
mod audit_tests;

/// One errand, as the thread doing it needs to know it.
struct Work {
    job: u64,
    background: bool,
    /// The call id the model gave. An outcome without it cannot be matched to
    /// the call it answers.
    call: Value,
    /// The `ask` call in the parent — the cause of whatever comes back.
    started: String,
    parent: String,
    child: String,
}

/// Keep the child's disposition separate from the presence of reply text.
/// These are local host states, not new event types or an error taxonomy.
#[derive(Debug)]
enum ExpertOutcome {
    Success(Value),
    Failure(Value),
    Cancelled(String),
}

#[derive(Debug)]
struct ExpertReport {
    origin: Option<String>,
    outcome: ExpertOutcome,
}

impl ExpertReport {
    fn from_reply(event: &EventEnvelope) -> Self {
        let payload = &event.payload;
        let outcome = if payload["cancelled"] == true {
            ExpertOutcome::Cancelled("expert reported cancellation".into())
        } else if let Some(error) = payload.get("error").filter(|error| !error.is_null()) {
            // Preserve the original judgment fields, including optional hints.
            ExpertOutcome::Failure(error.clone())
        } else {
            ExpertOutcome::Success(payload["text"].clone())
        };
        Self {
            origin: Some(event.id.clone()),
            outcome,
        }
    }

    fn failure(code: &str, message: &str) -> Self {
        Self {
            origin: None,
            outcome: ExpertOutcome::Failure(json!({
                "code": code, "message": message, "blame": "environment",
                "retryable": false, "transient": false,
            })),
        }
    }
}

/// Run an expert to its answer, report, and let its stream go.
fn run_expert(
    mut kernel: Kernel,
    entry: Injector,
    back: Injector,
    work: Work,
    prompt: String,
    ledger: Option<String>,
    state: Arc<Mutex<JobState>>,
) {
    // The first thing said in a stream is its root event, and origin is what
    // ties it back to the conversation that asked for it. The kernel
    // deliberately does not check this link, so where nobody writes it there
    // simply is none.
    entry.emit(
        "user",
        StreamHost::derived_root(
            ce::USER_MESSAGE,
            &work.parent,
            &work.started,
            json!({"text": prompt}),
        ),
    );

    let wake_rx = kernel.take_wake_receiver();
    let answer = run_expert_turn(&mut kernel, &state, |_| {
        wake_rx.as_ref().is_some_and(|rx| rx.recv().is_ok())
    });

    // Seal the completion/cancellation race before closing the kernel. An
    // accepted cancellation is consumed even if the answer just won dispatch.
    let cancellation = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.finished = true;
        state.cancellation.clone()
    };
    if cancellation.is_some() {
        let _ = kernel.run_until_quiescent();
    }
    let log = kernel.shutdown();
    let mut report = if let Some(reason) = cancellation {
        ExpertReport {
            origin: None,
            outcome: ExpertOutcome::Cancelled(reason),
        }
    } else {
        answer.unwrap_or_else(|error| ExpertReport::failure("ask.execution_failed", &error))
    };
    // A reply keeps its exact origin. A host failure, missing reply, or accepted
    // cancellation still points at the final recorded state of the child.
    if report.origin.is_none() {
        report.origin = log.reader().latest_id();
    }
    back.emit_now(&work, ledger, report);
    // The final report is sent after shutdown, not merely on accepting cancel.
}

fn run_expert_turn(
    kernel: &mut Kernel,
    state: &Mutex<JobState>,
    mut wait_for_input: impl FnMut(&Kernel) -> bool,
) -> Result<ExpertReport, String> {
    loop {
        let dispatched = kernel.run_until_quiescent();
        if let Some(reason) = state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancellation
            .clone()
        {
            return Ok(ExpertReport {
                origin: None,
                outcome: ExpertOutcome::Cancelled(reason),
            });
        }
        dispatched.map_err(|e| e.to_string())?;
        if kernel
            .log()
            .reader()
            .any_header(|e| e.event_type == ce::TURN_COMPLETED)
            .map_err(|error| error.to_string())?
        {
            return kernel
                .log()
                .reader()
                .scan_back_types(&[ce::OUTPUT_REPLY], |event, _| {
                    Ok(Some(ExpertReport::from_reply(event)))
                })
                .map(|reply| {
                    reply.unwrap_or_else(|| {
                        ExpertReport::failure(
                            "ask.no_reply",
                            "expert turn completed without a reply",
                        )
                    })
                })
                .map_err(|e| e.to_string());
        }
        if kernel.is_stopping() {
            return Err("expert stream stopped before producing a reply".into());
        }
        // Quiescence is not completion: handed-off work can inject later.
        // The host wake also carries cancellation, so an idle expert remains
        // cancellable without polling or depending on a particular loop type.
        if !wait_for_input(kernel) {
            return Ok(ExpertReport::failure(
                "ask.no_reply",
                "expert input closed before producing a reply",
            ));
        }
    }
}

/// Put one answer on the parent's ledger — as the call's outcome if the call is
/// still open, as a wake if it was answered with a job number long ago.
trait Report {
    fn emit_now(&self, work: &Work, ledger: Option<String>, report: ExpertReport);
}

impl Report for Injector {
    fn emit_now(&self, work: &Work, ledger: Option<String>, report: ExpertReport) {
        let job = work.job;
        let ExpertReport {
            origin: from,
            outcome,
        } = report;
        let (text, error, cancellation) = match outcome {
            ExpertOutcome::Success(text) => (text, None, None),
            ExpertOutcome::Failure(error) => (Value::Null, Some(error), None),
            ExpertOutcome::Cancelled(reason) => (Value::Null, None, Some(reason)),
        };
        let failed = error.as_ref().and_then(|e| e["message"].as_str());
        // Keep the historical fields and add the full error. Foreground errors
        // carry this context in details; adapters must still render the error.
        let body = json!({
            "job": job,
            "stream": work.child,
            "ledger": ledger,
            "text": text,
            "failed": failed,
            "error": error,
            "cancellation": cancellation.as_ref().map(|reason| json!({
                "reason": reason,
                "dispatchStopped": true,
                "outcome": "unknown for in-flight side effects; uncooperative in-process work may still be running"
            })),
            "interrupted": cancellation.as_ref().map(|_| "cancelled"),
        });
        // Across streams the tie is `origin` and only `origin` — `causes` is
        // checked against what the emitting stream witnessed, and an event in
        // another stream was witnessed by nobody there.
        let stamp = |draft: EventDraft| match &from {
            Some(event) => draft.with_origin(&work.child, event),
            None => draft,
        };
        if work.background {
            let summary = match failed {
                Some(problem) if error.as_ref().is_some_and(|e| e["code"] == "ask.no_stream") => {
                    format!("job {job} could not start: {problem}")
                }
                Some(problem) => format!("job {job} failed: {problem}"),
                None if cancellation.is_some() => {
                    format!("job {job} cancelled; in-flight effects may be unknown")
                }
                None => format!("job {job} finished"),
            };
            self.emit(
                "wake",
                stamp(EventDraft::new(
                    ce::WAKE,
                    &[&work.started],
                    json!({
                        "source": format!("expert:{job}"),
                        "summary": summary,
                        "body": body,
                    }),
                )),
            );
            return;
        }
        let payload = match error {
            Some(mut error) => {
                // Existing adapters render only error.message. Include the
                // judgment fields and audit pointer there without changing the
                // shared formatter; details.error retains the original object.
                error["message"] = json!(format!("Expert call failed: {body}"));
                json!({
                    "call": work.call,
                    "status": "error",
                    "error": error,
                    "details": body,
                })
            }
            None => json!({
                "call": work.call,
                "status": if body["interrupted"].as_str().is_some() { "cancelled" } else { "ok" },
                "result": body,
            }),
        };
        self.emit(
            "outcome",
            stamp(EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&work.started],
                payload,
            )),
        );
    }
}

/// A `Streams` view for a host that drives ONE stream itself and keeps the
/// its streams somewhere else.
///
/// This is the shape both hosts are in. Neither should have to become a
/// `StreamHost` to gain subagents — so the conversation stays where it is, and
/// the `StreamHost` beside it is used only as a factory: streams are opened
/// from it and taken straight out onto their own threads.
pub struct MainAndChildren<'a> {
    /// The stream id the conversation goes by
    pub main_id: &'a str,
    pub main: &'a mut Kernel,
    pub children: &'a mut StreamHost,
}

impl Streams for MainAndChildren<'_> {
    fn open_streams(&self) -> Vec<String> {
        // Only the conversation: an expert's stream leaves the host the moment
        // it opens, and an expert cannot hand work on anyway.
        vec![self.main_id.to_string()]
    }

    fn requests_from(&self, stream: &str, from_seq: u64) -> Result<RequestBatch, String> {
        if stream == self.main_id {
            return request_batch(&self.main.log().reader(), from_seq);
        }
        self.children.requests_from(stream, from_seq)
    }

    fn open_detached(&mut self, stream: &str, template: &str) -> Result<Kernel, String> {
        if stream == self.main_id {
            // A child id colliding with the conversation would take it over.
            return Err(format!("{stream} is the conversation itself"));
        }
        self.children.open_detached(stream, template)
    }

    fn open_captured(&mut self, stream: &str, snapshot: &Value) -> Result<Kernel, String> {
        if stream == self.main_id {
            return Err(format!("{stream} is the conversation itself"));
        }
        self.children.open_captured(stream, snapshot)
    }

    fn ledger_of(&self, stream: &str) -> Option<String> {
        self.children.ledger_of(stream)
    }

    fn injector_for(&self, stream: &str, instance: &str) -> Option<Injector> {
        if stream == self.main_id {
            return Some(self.main.injector(instance));
        }
        self.children.injector_for(stream, instance)
    }
}
