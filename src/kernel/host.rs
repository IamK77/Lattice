use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::contracts::assembly::{AssemblyManifest, ComponentInstance, Wire};
use crate::contracts::component::{ComponentManifest, RuntimeKind};
use crate::contracts::core_events::{self as ce, core_event_decls};
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::inspect::{inspect_assembly, InspectionIssue};
use crate::kernel::log::{EventLog, LogReader};
use crate::kernel::router::Router;

#[cfg(test)]
#[path = "host_failure_tests.rs"]
mod failure_tests;

#[cfg(test)]
#[path = "host_delivery_tests.rs"]
mod delivery_tests;

#[path = "host_bridge.rs"]
mod bridge;

pub use bridge::run_bridge_child;
use bridge::{kill_group, signal_registered_groups, spawn_process_bridge, BridgeSeat};

/// Reserved source name for events the kernel records on its own behalf
/// (errors, deadline interrupts, crash records). Assemblies may not use it.
pub const KERNEL_SOURCE: &str = "core";

/// The in-process component API.
///
/// `handle` runs on the component's own thread: blocking is allowed and only
/// occupies that component. Long waits should select on `ctx.cancellation()`
/// (Go-context style, zero polling); pure-CPU stretches should check
/// `ctx.cancelled()` periodically. Emissions are collected during `handle`
/// and dispatched afterwards — recording always precedes delivery. Notices
/// (`ctx.notify`) flow immediately and are never recorded: they are the
/// live-display bypass for streaming fragments.
pub trait Component: Send {
    fn handle(&mut self, port: &str, event: &EventEnvelope, ctx: &mut Ctx);

    /// Called once at startup, after the ledger is opened and before the run
    /// loop, so a component can rebuild in-memory state from its OWN past
    /// events (`ctx.log()`) and re-arm background wake sources it lost when
    /// the process died — a repeating timer resumes, an in-flight background
    /// job is settled. Recovery only ever SPEAKS and re-arms; it never re-runs
    /// a past side effect ("replay looks, never does"). A cause pointing at a
    /// pre-reopen event is accepted here — the durable ledger is the witnessed
    /// past. Default: nothing to restore.
    fn restore(&mut self, ctx: &mut Ctx) {
        let _ = ctx;
    }
}

/// Read-only handles onto OTHER streams this stream may observe (e.g. a
/// sidechannel observing the main conversation). Keyed by stream id.
pub type ForeignReaders = std::sync::Arc<HashMap<String, LogReader>>;

/// The prompt fragments contributed by assembled components — (instance,
/// text), sorted by instance name for a deterministic, cache-stable order.
/// Shared: hot-installing a component adds its fragment for later calls.
type PromptFragments = Arc<Mutex<Vec<(String, String)>>>;

/// The tool declarations contributed by assembled components, each stamped
/// with its provider instance ("provider" field). Initial order: by instance
/// name, then declaration order; hot installs append (prefix stays cache
/// -stable). Declarations travel WITH their implementation.
type ToolDecls = Arc<Mutex<Vec<Value>>>;

/// Everything a component may touch while handling one delivered event
pub struct Ctx {
    source: String,
    cancellation: CancellationToken,
    log: LogReader,
    foreign: ForeignReaders,
    prompts: PromptFragments,
    tools: ToolDecls,
    central: mpsc::Sender<Message>,
    wake: mpsc::Sender<()>,
    out: Vec<(String, EventDraft)>,
    failure: Option<ComponentFailure>,
    ledger_path: Option<PathBuf>,
}

#[derive(Debug)]
struct ComponentFailure {
    operation: String,
    message: String,
    affected: Vec<String>,
}

impl Ctx {
    /// Retire this instance when its state can no longer be trusted. Callers
    /// must return immediately and must stage recovery before side effects.
    /// Buffered emissions are discarded; already performed work is not undone.
    pub fn fail(&mut self, operation: &str, message: impl Into<String>, affected: &[String]) {
        if self.failure.is_none() {
            self.failure = Some(ComponentFailure {
                operation: operation.to_string(),
                message: message.into(),
                affected: affected.to_vec(),
            });
        }
        self.out.clear();
    }

    /// Emit a follow-up event from one of this component's output ports
    pub fn emit(&mut self, port: &str, draft: EventDraft) {
        self.out.push((port.to_string(), draft));
    }

    /// Has this delivery been cancelled (by a human interrupt, a deadline,
    /// or another trigger)? Cheap; poll it in CPU-bound loops.
    pub fn cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// The waitable cancellation token for this delivery — select on
    /// `token.cancelled()` alongside I/O futures to abort in-flight work
    /// the moment cancellation arrives.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Read-only access to this stream's ledger (dereference material
    /// pointers, look back at history)
    pub fn log(&self) -> &LogReader {
        &self.log
    }

    /// A payload field, whether it was left inline or moved out to a document
    /// beside the ledger.
    ///
    /// Fields the event type declares as documents (a system prompt, a tool
    /// list) move to a file once they grow, so a component that USES one has
    /// to follow the reference. Inline values pass through untouched, which is
    /// what makes this safe to call on every field and on every ledger,
    /// including ones written before documents existed.
    pub fn document(&self, value: &Value) -> Result<Value, String> {
        let dir = self
            .ledger_path
            .as_deref()
            .map(crate::contracts::document::documents_dir);
        crate::contracts::document::resolve(value, dir.as_deref())
    }

    /// Where this stream's ledger is written, if it is written at all.
    ///
    /// A component that reads the ledger has [`Ctx::log`] and needs nothing
    /// else. This is for the ones that have to NAME it: the agent's own
    /// record is a file, and telling it so without an address left it
    /// searching `git log` for its own conversation.
    pub fn ledger_path(&self) -> Option<&std::path::Path> {
        self.ledger_path.as_deref()
    }

    /// Read-only view onto another stream this one may observe (a sidechannel
    /// watching the main conversation). None if not granted. Observer only:
    /// there is no way to emit into a foreign stream from here.
    pub fn foreign_log(&self, stream: &str) -> Option<&LogReader> {
        self.foreign.get(stream)
    }

    /// The prompt fragments of every assembled component, as (instance,
    /// text) sorted by instance name — "installed = the model is told".
    /// The context gate merges these into the system prompt it forwards.
    pub fn prompt_fragments(&self) -> Vec<(String, String)> {
        self.prompts.lock().unwrap().clone()
    }

    /// Replace (or with None, withdraw) THIS instance's prompt fragment.
    /// For components whose fragment reflects mutable state — a skill
    /// library's listing, a status line. Takes effect on later calls only;
    /// the context gate defers adopting a changed system prompt until the
    /// provider cache is cold anyway, so updating here never costs a warm
    /// prefix — the deferred discipline, same as hot-installed tools.
    pub fn set_prompt(&self, text: Option<String>) {
        let mut prompts = self.prompts.lock().unwrap();
        prompts.retain(|(instance, _)| instance != &self.source);
        if let Some(text) = text {
            prompts.push((self.source.clone(), text));
            prompts.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }

    /// Every tool declared by assembled components, provider-stamped. The
    /// loop offers this list to the model: wiring a provider IS declaring
    /// its tools; hand-copied lists in assembly config are extras only.
    pub fn tool_decls(&self) -> Vec<Value> {
        self.tools.lock().unwrap().clone()
    }

    /// The ids of every foreign stream this one may observe (for a derived
    /// sidechannel: its parent). Sorted, for deterministic iteration.
    pub fn foreign_streams(&self) -> Vec<String> {
        let mut streams: Vec<String> = self.foreign.keys().cloned().collect();
        streams.sort();
        streams
    }

    /// Fire a transient notice (streaming fragment, progress hint): delivered
    /// immediately to the host's notice handler, never recorded in the log
    pub fn notify(&self, payload: Value) {
        let _ = self.central.send(Message::Notice {
            source: self.source.clone(),
            payload,
        });
    }

    /// An injector bound to THIS component's instance, safe to move into a
    /// background thread. It outlives the current `handle` call, so a
    /// long-running side task (a background command, a timer, a monitor) can
    /// inject a wake event whenever its moment comes — and the injection
    /// fires a wake so the host runs a turn. This is how any component
    /// becomes a wake source without the kernel knowing the word.
    pub fn injector(&self) -> Injector {
        Injector {
            instance: self.source.clone(),
            tx: self.central.clone(),
            wake: self.wake.clone(),
        }
    }
}

/// Builds a component instance from its manifest config
pub type Factory = Box<dyn FnMut(Option<&Value>) -> Box<dyn Component> + Send>;

/// Startup options
pub struct KernelOptions {
    /// The stream this kernel run belongs to (one kernel run = one stream for
    /// now; multi-stream hosting arrives with frontends/scheduling).
    /// None generates a fresh stream id.
    pub stream: Option<String>,
    pub log_file: Option<PathBuf>,
    /// Hard brake for runaway cascades: the maximum number of emissions one
    /// `run_until_quiescent` call may dispatch. Exceeding it records a
    /// core.control.error and stops delivery.
    pub max_dispatch_per_run: usize,
    /// Fallback wait when work is in flight but nothing is due: only reached
    /// when no component has a handle deadline pending. Records an error and
    /// returns if it expires.
    pub stall_timeout: Duration,
    /// After a delivery is cancelled, how long the component gets to wind
    /// down before it is declared unresponsive (crashed)
    pub grace_period: Duration,
    /// The whole of shutdown: how long every component together gets to wind
    /// down before the ones still running are left to the process exit.
    ///
    /// A limit exists because a component with no per-delivery deadline can
    /// block inside its handler, and waiting on it without one made the
    /// session unquittable.
    pub shutdown_timeout: Duration,
    /// Environment variables to withhold from component subprocesses.
    ///
    /// A child inherits the parent's whole environment unless told otherwise,
    /// and a process-form component is foreign code running next door: one
    /// line reading its environment hands it every credential this process
    /// was started with. The kernel is not told what these variables mean —
    /// only that they are not to be passed on — because which ones are
    /// secrets is the assembler's knowledge, not the kernel's.
    pub child_env_deny: Vec<String>,
    /// What only the host knows about this stream, merged into the
    /// `core.stream.opened` (or `resumed`) event: which program opened it,
    /// which model, which workspace.
    ///
    /// The kernel records what it can see — its own version and the assembly
    /// it was handed — and knows nothing about "a TUI" or "DeepSeek". Rather
    /// than teach it, the host says so itself.
    pub stream_note: Option<Value>,
    /// Strings that must never reach the ledger — API keys, in practice.
    ///
    /// Values rather than names, and enforced at the ledger's door rather
    /// than at each place a secret might appear, because the places are
    /// unbounded: a config file the agent reads, the output of `env`, a
    /// stack trace, an error quoting a URL. The kernel is told the strings
    /// and nothing about them.
    pub redact: Vec<String>,
}

impl Default for KernelOptions {
    fn default() -> Self {
        Self {
            stream: None,
            log_file: None,
            max_dispatch_per_run: 10_000,
            stall_timeout: Duration::from_secs(10),
            grace_period: Duration::from_secs(5),
            shutdown_timeout: Duration::from_secs(10),
            child_env_deny: Vec::new(),
            stream_note: None,
            redact: Vec::new(),
        }
    }
}

/// Errors that prevent the kernel from starting at all. Runtime faults (bad
/// emissions, audit violations, budget exhaustion, component crashes) are NOT
/// errors in this sense — they are recorded into the log as core.control
/// events and the system keeps going.
#[derive(Debug)]
pub enum KernelError {
    /// The assembly failed inspection; the system must not start
    Inspection(Vec<InspectionIssue>),
    /// No factory registered for a component named in the assembly
    MissingFactory(String),
    Io(std::io::Error),
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(issues) => {
                writeln!(f, "assembly failed inspection ({} issues):", issues.len())?;
                for issue in issues {
                    writeln!(f, "  {}: {}", issue.location, issue.problem)?;
                }
                Ok(())
            }
            Self::MissingFactory(component) => write!(f, "no factory for component: {component}"),
            Self::Io(err) => write!(f, "log IO error: {err}"),
        }
    }
}

impl std::error::Error for KernelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

/// One delivery on its way into a component's mailbox:
/// (input port, the recorded event, this delivery's cancellation token)
type Delivery = (String, EventEnvelope, CancellationToken);

enum Message {
    /// A host-owned cutoff for the final stopped queue drain.
    DrainBoundary,
    Stop {
        reason: String,
        origin: Option<crate::StreamRef>,
    },
    Emission {
        source: String,
        port: String,
        draft: EventDraft,
    },
    /// A component finished handling one delivered event
    /// A delivery finished. The EVENT is named because a component may be
    /// handling several at once, and then "the one at the front" is not an
    /// answer to which finished.
    Processed { instance: String, event: String },
    /// A component panicked while handling an event; its thread is gone
    Crashed { instance: String, event_id: String },
    Failed {
        instance: String,
        event_id: Option<String>,
        failure: ComponentFailure,
    },
    /// A transient, non-recorded notice from a component (streaming bypass)
    Notice { source: String, payload: Value },
}

/// One delivery a component has not finished yet
struct Outstanding {
    event_id: String,
    token: CancellationToken,
    /// When this delivery must be done (from the manifest's handleTimeoutMs)
    deadline: Option<Instant>,
    /// Set once cancelled: when the wind-down grace expires
    grace_deadline: Option<Instant>,
}

/// Handle for feeding events into the kernel from outside the component world
/// (frontends' host side, tests). Bound to one instance name at creation:
/// whoever holds an injector can speak only as that instance.
#[derive(Clone, Debug)]
pub struct Injector {
    instance: String,
    tx: mpsc::Sender<Message>,
    wake: mpsc::Sender<()>,
}

impl Injector {
    /// Queue an emission from the bound instance's output `port`. Validation
    /// happens at dispatch; violations become core.control.error events.
    ///
    /// Also fires a wake signal: an injection is an EXTERNAL event that may
    /// arrive while the kernel is idle (a user message, a background task
    /// completing, a timer), so it must be able to trigger a fresh turn.
    /// A host runs a push loop over `take_wake_receiver`. (Emissions from a
    /// component's own `ctx.emit` during a turn do NOT go through here — they
    /// are collected and dispatched within the running turn, so they never
    /// need to wake anyone.)
    pub fn emit(&self, port: &str, draft: EventDraft) {
        let _ = self.tx.send(Message::Emission {
            source: self.instance.clone(),
            port: port.to_string(),
            draft,
        });
        let _ = self.wake.send(());
    }
}

/// Host-owned control channel. Not an instance capability and never handed
/// to components. A stop is stream-wide and does not depend on assembly wires.
#[derive(Clone)]
pub struct StopHandle {
    tx: mpsc::Sender<Message>,
    wake: mpsc::Sender<()>,
}

impl StopHandle {
    pub fn request(&self, reason: String, origin: Option<crate::StreamRef>) -> bool {
        let sent = self.tx.send(Message::Stop { reason, origin }).is_ok();
        if sent {
            let _ = self.wake.send(());
        }
        sent
    }
}

type NoticeHandler = Box<dyn FnMut(&str, &Value) + Send>;

/// The kernel as a runnable whole: executes the assembly (duty #2), keeps the
/// log (duty #1), delivers events along the wires (duty #3) and hosts
/// in-process components (the in-process part of duty #4).
///
/// Execution model: mailbox concurrency. Every component instance runs on its
/// own thread with a FIFO mailbox; the dispatcher (running inside
/// `run_until_quiescent` on the caller's thread) validates, records, routes,
/// and keeps the clocks: handle deadlines auto-cancel overdue deliveries
/// (the watchman when nobody is at the keyboard), the grace period turns
/// unresponsiveness into a recorded crash, and component panics are caught
/// and recorded without touching the kernel.
///
/// Future work of this same duty: separate-process hosting (child processes
/// speaking one envelope JSON per line), WASM sandbox hosting with
/// per-capability grants, and a stop protocol for graceful shutdown.
pub struct Kernel {
    log: EventLog,
    startup_cost: crate::startup::Timings,
    router: Router,
    /// The live assembly and registry — hot installs extend them
    assembly: AssemblyManifest,
    registry: HashMap<String, ComponentManifest>,
    manifests: HashMap<String, ComponentManifest>,
    mailboxes: HashMap<String, mpsc::Sender<Delivery>>,
    /// Per instance: ids of events it has received or emitted. Enforcement
    /// layer three — a cause may only point at a witnessed event, so the
    /// causal chain cannot lie. Grows with the log; pruning comes with
    /// persistence grouping.
    witnessed: HashMap<String, HashSet<String>>,
    /// The durable prefix shared by all instances after restart settlement.
    /// Membership requires an actual indexed event at or below this boundary;
    /// an event id's spelling is not evidence that it exists. New events still
    /// follow the normal per-instance witness rule.
    reopened_through: u64,
    /// Per instance: deliveries not yet finished (front = being handled)
    outstanding: HashMap<String, VecDeque<Outstanding>>,
    threads: HashMap<String, JoinHandle<()>>,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    /// Fired on every external injection; a host's push loop waits on the
    /// paired receiver to know a fresh turn is due (see `take_wake_receiver`)
    wake_tx: mpsc::Sender<()>,
    wake_rx: Option<mpsc::Receiver<()>>,
    in_flight: usize,
    /// Once set, emissions are still audited but never delivered again.
    stopping: Option<String>,
    max_dispatch_per_run: usize,
    stall_timeout: Duration,
    grace_period: Duration,
    /// The whole of shutdown (see [`KernelOptions::shutdown_timeout`])
    shutdown_timeout: Duration,
    /// Variables never handed to a component subprocess (see
    /// [`KernelOptions::child_env_deny`])
    child_env_deny: Vec<String>,
    /// The process group of each running process-form component, published by
    /// its bridge as soon as the child exists.
    ///
    /// Declaring a component unresponsive is the moment its process must go:
    /// the kernel has already given it a cancellation and a grace period, and
    /// what follows is not another wait. Leaving the killing to the bridge —
    /// which is where it used to be, on a second grace of its own — meant the
    /// kill landed after the kernel had already stopped caring, and never at
    /// all if the host exited in between.
    child_groups: Arc<Mutex<HashMap<String, i32>>>,
    notice_handler: Option<NoticeHandler>,
    prompts: PromptFragments,
    tools: ToolDecls,
    /// How to build each in-process component — empty unless a host handed
    /// them over with [`Kernel::adopt_factories`].
    ///
    /// The kernel could always hot-install a PROCESS component, because making
    /// one needs nothing but the entry string in its manifest. The in-process
    /// form needs a factory, and `start` only ever borrowed them — so an
    /// asymmetry that looked like a rule ("only process components can be
    /// installed hot") was really just a thing the kernel had never been given.
    factories: HashMap<String, Factory>,
    /// The reader and the foreign handles a fresh instance needs, kept for
    /// the same reason.
    reader: LogReader,
    foreign: ForeignReaders,
    /// Where the ledger is written, for components that name it.
    ledger_path: Option<PathBuf>,
}

/// The fragment one instance contributes: the assembly may override per
/// instance (config "prompt": a string replaces the manifest's, null
/// suppresses it), else the manifest's own.
fn effective_prompt(manifest: &ComponentManifest, config: Option<&Value>) -> Option<String> {
    match config.and_then(|c| c.get("prompt")) {
        Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        _ => manifest.prompt.clone(),
    }
}

/// The stream says what it is, as its own first event.
///
/// Written before anything else so that `Read from=1 limit=1` answers "what
/// is this conversation" — which used to require inferring it from whatever
/// happened to come first. A ledger that already has events gets `resumed`
/// instead: same facts, plus where the previous life stopped, so a reader can
/// tell one process from the next.
///
/// Before the hanging chains are settled, so the ledger reads in the order
/// things happened: this process arrived, THEN it cleared what the last one
/// left in flight.
fn record_stream_start(
    log: &mut EventLog,
    assembly: &AssemblyManifest,
    note: Option<&Value>,
) -> std::io::Result<()> {
    let instances: serde_json::Map<String, Value> = assembly
        .instances
        .iter()
        .map(|(name, instance)| (name.clone(), Value::String(instance.component.clone())))
        .collect();
    let last = (!log.is_empty()).then(|| log.reader().snapshot_end());
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut payload = match last {
        None => json!({
            "lattice": env!("CARGO_PKG_VERSION"),
            "started": now,
            "instances": instances,
        }),
        Some(seq) => json!({
            "lattice": env!("CARGO_PKG_VERSION"),
            "resumed": now,
            "fromSeq": seq,
            "instances": instances,
        }),
    };
    // What only the host knows — which program, which model, which directory.
    if let (Some(fields), Some(into)) = (note.and_then(Value::as_object), payload.as_object_mut()) {
        for (key, value) in fields {
            into.insert(key.clone(), value.clone());
        }
    }
    let event_type = if last.is_none() {
        ce::STREAM_OPENED
    } else {
        ce::STREAM_RESUMED
    };
    let draft = EventDraft {
        event_type: event_type.to_string(),
        causes: Vec::new(),
        origin: None,
        payload,
        reason: None,
    };
    log.append(draft, KERNEL_SOURCE)
        .map_err(std::io::Error::other)?;
    Ok(())
}

/// A reopened ledger can end mid-flight: a started call whose completion
/// never came was severed by the previous process's death. Each such chain
/// is settled with an interrupt event on reopen — recovery only ever SPEAKS
/// (appends new events); it never silently re-executes side effects
/// ("replay looks, never does"). Settling is idempotent: a chain already
/// closed by a completion OR by a prior settle is left alone.
///
/// "Chain", not "event" — see [`ce::hanging_chain_heads`]. A gate forwards a
/// request by appending its own copy, and the outcome answers that copy, so
/// judging each event on its own marks every relayed call as hanging.
fn settle_hanging_chains(log: &mut EventLog) -> std::io::Result<()> {
    for started_type in [ce::MODEL_CALL_STARTED, ce::TOOL_EXEC_STARTED] {
        let hanging = log.hanging(started_type)?;
        for id in hanging {
            let draft = EventDraft {
                event_type: ce::INTERRUPTED.to_string(),
                causes: vec![id],
                origin: None,
                payload: json!({"by": "restart"}),
                reason: Some(
                    "in flight when this stream's process ended; settled on reopen".to_string(),
                ),
            };
            log.append(draft, KERNEL_SOURCE)
                .map_err(std::io::Error::other)?;
        }
    }
    Ok(())
}

impl Kernel {
    /// Inspect the assembly, register event types (core + components'),
    /// open the log, spawn every component on its own thread.
    /// Any inspection issue prevents startup.
    pub fn start(
        assembly: &AssemblyManifest,
        registry: &HashMap<String, ComponentManifest>,
        factories: &mut HashMap<String, Factory>,
        options: KernelOptions,
    ) -> Result<Self, KernelError> {
        Self::start_with_foreign(
            assembly,
            registry,
            factories,
            options,
            ForeignReaders::default(),
        )
    }

    /// Like `start`, but grants components read-only handles onto other
    /// streams (a sidechannel observing the main conversation). The multi-
    /// stream host uses this; a lone kernel passes an empty map.
    pub fn start_with_foreign(
        assembly: &AssemblyManifest,
        registry: &HashMap<String, ComponentManifest>,
        factories: &mut HashMap<String, Factory>,
        options: KernelOptions,
        foreign: ForeignReaders,
    ) -> Result<Self, KernelError> {
        let mut startup = crate::startup::PhaseTimer::start();
        let issues = inspect_assembly(assembly, registry);
        if !issues.is_empty() {
            return Err(KernelError::Inspection(issues));
        }

        let mut types = core_event_decls();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for instance in assembly.instances.values() {
            if seen.insert(&instance.component) {
                types.extend(registry[&instance.component].events.iter().cloned());
            }
        }
        // Kept before the options move into the log: components that must
        // NAME the ledger (rather than read it) are told where it is.
        let ledger_path = options.log_file.clone();
        let stream = options.stream.unwrap_or_else(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before 1970")
                .as_nanos();
            format!("st_{nanos:x}")
        });
        startup.checkpoint("prepare");
        let mut log =
            EventLog::open(types, stream.clone(), options.log_file).map_err(KernelError::Io)?;
        startup.checkpoint("log_open");
        // Set before the first append, including the settling below: the
        // ledger can keep a secret out, but cannot take one back.
        log.redact(crate::kernel::log::Redactor::new(options.redact.clone()));
        record_stream_start(&mut log, assembly, options.stream_note.as_ref())
            .map_err(KernelError::Io)?;
        settle_hanging_chains(&mut log).map_err(KernelError::Io)?;
        startup.checkpoint("settle");
        // The durable past: every event present at reopen. A restoring
        // component may cause new work off any of these (its own timer's
        // schedule call, its background job's ack), so seed them as witnessed
        // -by-all — the ledger is shared, established fact.
        let reopened_through = log.len() as u64;
        let reader = log.reader();
        startup.checkpoint("witness_seed");

        let (tx, rx) = mpsc::channel();
        let (wake_tx, wake_rx) = mpsc::channel();
        let mut mailboxes = HashMap::new();
        let mut manifests = HashMap::new();
        let child_groups: Arc<Mutex<HashMap<String, i32>>> = Arc::default();
        let mut witnessed = HashMap::new();
        let mut outstanding = HashMap::new();
        let mut threads = HashMap::new();
        // `restore` runs on each component's own thread, but the trait says it
        // happens "before the run loop" — and callers rely on that: a fragment
        // or a tool declaration published during restore must be there for the
        // FIRST call, not the second. Without a barrier a slow restore (one
        // that reads a file) loses the race to a fast one, and the system
        // prompt of turn one differs from turn two, which is the one thing the
        // cached prefix cannot survive. Each thread reports in; start waits.
        let (restored_tx, restored_rx) = mpsc::channel::<(String, bool)>();
        let mut restoring = 0usize;

        // Collect every instance's prompt fragment (manifest-borne, assembly
        // -overridable) — the "installed = the model is told" mechanism
        let mut fragments: Vec<(String, String)> = assembly
            .instances
            .iter()
            .filter_map(|(name, instance)| {
                effective_prompt(&registry[&instance.component], instance.config.as_ref())
                    .map(|text| (name.clone(), text))
            })
            .collect();
        fragments.sort();
        let prompts: PromptFragments = Arc::new(Mutex::new(fragments));

        // Collect every instance's tool declarations, stamped with their
        // provider — declarations travel with their implementation
        let mut instance_names: Vec<&String> = assembly.instances.keys().collect();
        instance_names.sort();
        let mut decls: Vec<Value> = Vec::new();
        for name in instance_names {
            let component = &assembly.instances[name].component;
            for decl in &registry[component].tools {
                let mut stamped = decl.clone();
                stamped["provider"] = json!(name);
                decls.push(stamped);
            }
        }
        let tools: ToolDecls = Arc::new(Mutex::new(decls));

        for (name, instance) in &assembly.instances {
            let manifest = &registry[&instance.component];
            if manifest.runtime == RuntimeKind::Process {
                // `lazy` is an ASSEMBLY decision, not the component's: the
                // same component is worth starting eagerly in one deployment
                // and deferring in another, and only the assembly knows which.
                let lazy = instance
                    .config
                    .as_ref()
                    .and_then(|c| c.get("lazy"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let (mail_tx, bridge) = spawn_process_bridge(
                    BridgeSeat {
                        instance: name.clone(),
                        entry: manifest.entry.clone(),
                        config: instance.config.clone(),
                        stream: stream.clone(),
                        lazy,
                        env_deny: options.child_env_deny.clone(),
                    },
                    tx.clone(),
                    wake_tx.clone(),
                    child_groups.clone(),
                )
                .map_err(KernelError::Io)?;
                mailboxes.insert(name.clone(), mail_tx);
                manifests.insert(name.clone(), manifest.clone());
                witnessed.insert(name.clone(), HashSet::new());
                outstanding.insert(name.clone(), VecDeque::new());
                threads.insert(name.clone(), bridge);
                continue;
            }
            let factory = factories
                .get_mut(&instance.component)
                .ok_or_else(|| KernelError::MissingFactory(instance.component.clone()))?;
            // One component per worker. They share a mailbox, not state: a
            // component that wants several deliveries at once must be able to
            // handle them independently, and the cleanest way to guarantee
            // that is to give each worker its own.
            let workers = registry[&instance.component]
                .concurrency
                .unwrap_or(1)
                .max(1);
            let crew: Vec<Box<dyn Component>> = (0..workers)
                .map(|_| factory(instance.config.as_ref()))
                .collect();

            restoring += 1;
            let (mail_tx, thread) = spawn_inproc(
                crew,
                name.clone(),
                tx.clone(),
                reader.clone(),
                foreign.clone(),
                Arc::clone(&prompts),
                Arc::clone(&tools),
                wake_tx.clone(),
                restored_tx.clone(),
                ledger_path.clone(),
            );
            threads.insert(name.clone(), thread);

            mailboxes.insert(name.clone(), mail_tx);
            manifests.insert(name.clone(), registry[&instance.component].clone());
            witnessed.insert(name.clone(), HashSet::new());
            outstanding.insert(name.clone(), VecDeque::new());
        }

        // Every in-process instance acknowledges success or failure once.
        // Failure details are queued on the central channel; other workers
        // retaining senders must not prevent the barrier from completing.
        // Process-form components restore through their own bridge.
        drop(restored_tx);
        for _ in 0..restoring {
            if restored_rx.recv().is_err() {
                break;
            }
        }

        startup.checkpoint("components_start");
        let mut kernel = Self {
            log,
            startup_cost: crate::startup::Timings::default(),
            ledger_path,
            router: Router::new(assembly),
            assembly: assembly.clone(),
            registry: registry.clone(),
            manifests,
            mailboxes,
            witnessed,
            reopened_through,
            outstanding,
            threads,
            tx,
            rx,
            wake_tx,
            wake_rx: Some(wake_rx),
            in_flight: 0,
            stopping: None,
            max_dispatch_per_run: options.max_dispatch_per_run,
            stall_timeout: options.stall_timeout,
            grace_period: options.grace_period,
            shutdown_timeout: options.shutdown_timeout,
            child_env_deny: options.child_env_deny.clone(),
            child_groups: child_groups.clone(),
            notice_handler: None,
            prompts,
            tools,
            // Not taken from the caller: one factory map builds MANY kernels
            // (a stream template opens a stream per conversation), and
            // emptying it here left the second stream with no way to build
            // anything. Handing them over is a separate, deliberate act.
            factories: HashMap::new(),
            reader,
            foreign,
        };
        kernel.startup_cost = startup.finish("finalize");
        Ok(kernel)
    }

    /// Startup bookkeeping and component construction, measured once. The
    /// component span includes the in-process restore barrier, not readiness
    /// of process-form components. This is diagnostic data, not a UI hook.
    pub fn startup_cost(&self) -> &crate::startup::Timings {
        &self.startup_cost
    }

    /// Install a component into the RUNNING assembly — no restart, the
    /// conversation keeps going. v1 accepts Process components only: foreign
    /// code lives next door, per the trust decision. The extended assembly is
    /// fully re-inspected first; the installation is recorded as a
    /// decision-class event (reason mandatory), caused by `causes`.
    pub fn install(
        &mut self,
        component: ComponentManifest,
        instance: &str,
        config: Option<Value>,
        wires: &[Wire],
        reason: &str,
        causes: &[&str],
    ) -> Result<(), KernelError> {
        let causes = self.known_causes(causes).map_err(KernelError::Io)?;
        if component.runtime != RuntimeKind::Process {
            return Err(KernelError::Inspection(vec![InspectionIssue {
                location: format!("instance {instance}"),
                problem: "hot install accepts process components only in v1".to_string(),
            }]));
        }
        let mut registry = self.registry.clone();
        registry.insert(component.name.clone(), component.clone());
        let mut assembly = self.assembly.clone();
        assembly.instances.insert(
            instance.to_string(),
            ComponentInstance {
                component: component.name.clone(),
                config: config.clone(),
                requires: Vec::new(),
            },
        );
        assembly.wires.extend(wires.iter().cloned());
        let issues = inspect_assembly(&assembly, &registry);
        if !issues.is_empty() {
            return Err(KernelError::Inspection(issues));
        }

        self.log
            .register_types(&component.events)
            .map_err(KernelError::Io)?;
        let lazy = config
            .as_ref()
            .and_then(|c| c.get("lazy"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (mail_tx, bridge) = spawn_process_bridge(
            BridgeSeat {
                instance: instance.to_string(),
                entry: component.entry.clone(),
                config,
                stream: self.log.stream().to_string(),
                lazy,
                env_deny: self.child_env_deny.clone(),
            },
            self.tx.clone(),
            self.wake_tx.clone(),
            self.child_groups.clone(),
        )
        .map_err(KernelError::Io)?;
        self.mailboxes.insert(instance.to_string(), mail_tx);
        self.manifests
            .insert(instance.to_string(), component.clone());
        self.witnessed.insert(instance.to_string(), HashSet::new());
        self.outstanding
            .insert(instance.to_string(), VecDeque::new());
        self.threads.insert(instance.to_string(), bridge);
        for wire in wires {
            self.router.add_wire(wire);
        }
        self.assembly = assembly;
        self.registry = registry;
        if let Some(text) = effective_prompt(
            &component,
            self.assembly.instances[instance].config.as_ref(),
        ) {
            // Appended, not sorted in: an install must not reshuffle the
            // existing prompt prefix (provider caches hash the prefix — the
            // new fragment landing last costs one miss for the tail only).
            // A restart re-collects in sorted order: one more one-time miss,
            // accepted.
            self.prompts
                .lock()
                .unwrap()
                .push((instance.to_string(), text));
        }
        // Its tools join the offered list (appended: the existing prefix
        // stays byte-stable for prompt caching)
        {
            let mut tools = self.tools.lock().unwrap();
            for decl in &component.tools {
                let mut stamped = decl.clone();
                stamped["provider"] = json!(instance);
                // Hot-installed tools start DEFERRED: kept out of the model
                // -visible schema until a moment when the prompt cache is
                // cold anyway (the context gate decides the promotion)
                stamped["installed"] = json!(true);
                tools.push(stamped);
            }
        }

        let draft = EventDraft {
            event_type: ce::COMPONENT_INSTALLED.to_string(),
            causes,
            origin: None,
            payload: json!({
                "component": component.name,
                "instance": instance,
                "wires": wires,
            }),
            reason: Some(reason.to_string()),
        };
        // Recording can be refused — a blank reason on a decision event is the
        // reachable one, and it arrives from a tool argument no schema can
        // forbid. Returning it beats the `expect` that used to be here, which
        // took down whichever thread owns the kernel.
        self.log.append(draft, KERNEL_SOURCE).map_err(|e| {
            KernelError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("the install happened but could not be recorded: {e}"),
            ))
        })?;
        Ok(())
    }

    /// Create an injection handle bound to one instance name.
    pub fn injector(&self, instance: &str) -> Injector {
        Injector {
            instance: instance.to_string(),
            tx: self.tx.clone(),
            wake: self.wake_tx.clone(),
        }
    }

    /// A host may stop this entire stream without relying on component ports.
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle {
            tx: self.tx.clone(),
            wake: self.wake_tx.clone(),
        }
    }

    /// Take the wake receiver (once). A host's run loop waits on it: every
    /// external injection fires a wake, so the loop knows to run a turn —
    /// this is what turns the host from pull ("run when told") into push
    /// ("run whenever anything is injected"), the basis for background tasks,
    /// timers and monitors all being ordinary injecting components.
    pub fn take_wake_receiver(&mut self) -> Option<mpsc::Receiver<()>> {
        self.wake_rx.take()
    }

    /// Receive transient notices (streaming fragments etc.). One handler;
    /// replaces any previous one.
    pub fn set_notice_handler(&mut self, handler: impl FnMut(&str, &Value) + Send + 'static) {
        self.notice_handler = Some(Box::new(handler));
    }

    /// Process queued emissions until the system is quiescent: no component is
    /// mid-handle and no emission is waiting. Runtime faults are recorded as
    /// core.control events; only IO-level failures surface as Err.
    pub fn run_until_quiescent(&mut self) -> Result<(), KernelError> {
        let mut dispatched = 0usize;
        let mut stall_reported = false;
        loop {
            if self.stopping.is_some() && self.in_flight == 0 {
                // Per-worker FIFO puts handle outcomes before Processed.
                // Untracked background injections must not keep a stopped
                // stream alive forever once all deliveries have drained.
                return Ok(());
            }
            if self.in_flight == 0 {
                // Nobody is working: whatever is in the channel is all there is
                match self.rx.try_recv() {
                    Ok(message) => {
                        if !self.process(message, &mut dispatched) {
                            return Ok(());
                        }
                    }
                    Err(_) => return Ok(()),
                }
                continue;
            }

            let now = Instant::now();
            if let Some((instance, event_id, due)) = self.next_deadline() {
                if due <= now {
                    self.handle_expiry(&instance, &event_id);
                    continue;
                }
                match self.rx.recv_timeout(due - now) {
                    Ok(message) => {
                        if !self.process(message, &mut dispatched) {
                            return Ok(());
                        }
                    }
                    Err(_) => {
                        self.handle_expiry(&instance, &event_id);
                    }
                }
            } else {
                match self.rx.recv_timeout(self.stall_timeout) {
                    Ok(message) => {
                        stall_reported = false;
                        if !self.process(message, &mut dispatched) {
                            return Ok(());
                        }
                    }
                    Err(_) => {
                        // A quiet mailbox is not a quiet kernel: the worker
                        // still owns a delivery. Keep receiving its eventual
                        // result (or a host stop), rather than stranding it and
                        // telling the frontend that the turn has finished.
                        if !stall_reported {
                            self.record_kernel_error(
                                "core.dispatch_stalled",
                                "dispatch stalled with no deadline pending; still waiting for the component or a host stop",
                                &[],
                                json!({"inFlight": self.in_flight}),
                            );
                            stall_reported = true;
                        }
                    }
                }
            }
        }
    }

    /// Whether a host stop has been accepted; no new work may be dispatched.
    pub fn is_stopping(&self) -> bool {
        self.stopping.is_some()
    }

    /// After host-side producers have joined, record messages preceding a
    /// queue cutoff. Late results use normal validation, without restarting
    /// work. A continuously emitting component cannot extend this boundary.
    pub fn drain_stopped_inputs(&mut self) {
        if !self.is_stopping() {
            return;
        }
        let _ = self.tx.send(Message::DrainBoundary);
        let mut dispatched = 0;
        while let Ok(message) = self.rx.recv() {
            if matches!(message, Message::DrainBoundary) {
                break;
            }
            self.process(message, &mut dispatched);
        }
    }

    /// Returns false when the run must stop (budget exhausted)
    fn process(&mut self, message: Message, dispatched: &mut usize) -> bool {
        match message {
            Message::DrainBoundary => {}
            Message::Stop { reason, origin } => {
                if self.stopping.is_none() {
                    let event = self
                        .log
                        .append(
                            EventDraft {
                                event_type: ce::INTERRUPTED.into(),
                                causes: Vec::new(),
                                origin,
                                reason: Some(reason),
                                payload: json!({"by": "host", "scope": "stream"}),
                            },
                            KERNEL_SOURCE,
                        )
                        .expect("host stop is a valid control event");
                    self.stopping = Some(event.id);
                    let deadline = Instant::now() + self.grace_period;
                    for queue in self.outstanding.values_mut() {
                        for delivery in queue {
                            delivery.token.cancel();
                            delivery.deadline = None;
                            delivery.grace_deadline = Some(deadline);
                        }
                    }
                }
            }
            Message::Processed { instance, event } => {
                let manifest = self.manifests.get(&instance);
                let timeout = manifest.and_then(|m| m.handle_timeout_ms);
                let workers = manifest.and_then(|m| m.concurrency).unwrap_or(1).max(1);
                if let Some(queue) = self.outstanding.get_mut(&instance) {
                    // Remove the one that finished, by name. With one worker
                    // that is always the front; with several it is whichever
                    // came back first, which is the whole point of having them.
                    if let Some(at) = queue.iter().position(|o| o.event_id == event) {
                        queue.remove(at);
                        self.in_flight -= 1;
                        // The clock measures HANDLING, not queueing. The first
                        // `workers` entries are the ones being handled, so a
                        // delivery starts its deadline when it moves into that
                        // window — not when it was queued behind others.
                        if let Some(ms) = timeout {
                            for waiting in queue.iter_mut().take(workers) {
                                if waiting.grace_deadline.is_none() && waiting.deadline.is_none() {
                                    waiting.deadline =
                                        Some(Instant::now() + Duration::from_millis(ms));
                                }
                            }
                        }
                    }
                }
            }
            Message::Crashed { instance, event_id } => {
                self.declare_dead(&instance, Some(&event_id), "component panicked");
            }
            Message::Failed {
                instance,
                event_id,
                failure,
            } => {
                self.component_failed(&instance, event_id.as_deref(), failure);
            }
            Message::Notice { source, payload } => {
                if let Some(handler) = &mut self.notice_handler {
                    handler(&source, &payload);
                }
            }
            Message::Emission {
                source,
                port,
                draft,
            } => {
                *dispatched += 1;
                if *dispatched > self.max_dispatch_per_run {
                    self.record_kernel_error(
                        "core.dispatch_budget_exceeded",
                        "dispatch budget exceeded; stopping delivery",
                        &draft.causes,
                        json!({"budget": self.max_dispatch_per_run}),
                    );
                    return false;
                }
                self.dispatch(source, port, draft);
            }
        }
        true
    }

    /// New deliveries spend handling time only in an available worker slot.
    /// Both ordinary events and kernel settlements use this rule; queued
    /// deliveries get their clock when a Processed message promotes them.
    fn delivery_deadline(&self, instance: &str) -> Option<Instant> {
        let manifest = self.manifests.get(instance)?;
        let workers = manifest.concurrency.unwrap_or(1).max(1);
        self.outstanding
            .get(instance)
            .filter(|queue| queue.len() < workers)
            .and(manifest.handle_timeout_ms)
            .map(|ms| Instant::now() + Duration::from_millis(ms))
    }

    /// The earliest pending deadline (handle timeout or cancellation grace)
    /// across every delivery being handled — the first `concurrency` of each
    /// instance's queue, since those are the ones whose clock is running.
    fn next_deadline(&self) -> Option<(String, String, Instant)> {
        let mut earliest: Option<(String, String, Instant)> = None;
        for (instance, queue) in &self.outstanding {
            let workers = self
                .manifests
                .get(instance)
                .and_then(|m| m.concurrency)
                .unwrap_or(1)
                .max(1);
            for active in queue.iter().take(workers) {
                let Some(due) = active.grace_deadline.or(active.deadline) else {
                    continue;
                };
                if earliest.as_ref().is_none_or(|(_, _, e)| due < *e) {
                    earliest = Some((instance.clone(), active.event_id.clone(), due));
                }
            }
        }
        earliest
    }

    /// A front delivery's clock ran out: first the deadline (auto-cancel,
    /// recorded as a deadline interrupt), then the grace (declared crashed)
    /// Whichever delivery's clock ran out — by id, and by asking which clock,
    /// rather than by taking the queue's front and reading its fields.
    ///
    /// With one worker those are the same thing. With several they are not:
    /// two deliveries can come due together, and the old form then cancelled
    /// the front one (setting its grace), immediately saw the second expiry,
    /// found a grace field on the front and declared the whole instance dead
    /// — with no grace at all, and without the second delivery ever being
    /// told to stop. One shell command finishing next to two slow ones was
    /// enough.
    fn handle_expiry(&mut self, instance: &str, event_id: &str) {
        let now = Instant::now();
        let grace = self.grace_period;
        let Some(queue) = self.outstanding.get_mut(instance) else {
            return;
        };
        let Some(active) = queue.iter_mut().find(|o| o.event_id == event_id) else {
            return; // it finished while we were getting here
        };
        let expired_grace = active.grace_deadline.is_some_and(|due| due <= now);
        if expired_grace {
            self.declare_dead(
                instance,
                Some(event_id),
                "unresponsive after cancellation grace period",
            );
            return;
        }
        if active.deadline.is_some_and(|due| due <= now) {
            // The watchman: nobody at the keyboard, the deadline cancels.
            active.token.cancel();
            active.deadline = None;
            active.grace_deadline = Some(now + grace);
            // DELIVERED, not merely recorded — the same as every other way a
            // call can end. This was the one ending that was only written
            // down: the requester went on waiting for a result the kernel had
            // already declared would never come, and the round never closed.
            // The component's own death does not rescue it either, because by
            // then the call HAS an ending (this one) and is not settled twice.
            self.settle_chain(
                event_id,
                "deadline",
                "this call ran past its deadline and was cancelled",
                json!({"component": instance}),
            );
        }
    }

    /// Stop admission and cancel every delivery, including queued work.
    fn retire(&mut self, instance: &str) -> Option<Vec<String>> {
        let queue = self.outstanding.remove(instance)?;
        for delivery in &queue {
            delivery.token.cancel();
        }
        let held = queue
            .iter()
            .map(|delivery| delivery.event_id.clone())
            .collect();
        self.mailboxes.remove(instance);
        self.manifests.remove(instance);
        self.witnessed.remove(instance);
        self.threads.remove(instance);
        self.in_flight -= queue.len();
        Some(held)
    }

    fn component_failed(
        &mut self,
        instance: &str,
        processing: Option<&str>,
        failure: ComponentFailure,
    ) {
        // Responsibility is declared by the component, but identities and
        // witnessing are checked before retiring it removes the witness set.
        let mut affected = Vec::new();
        for id in &failure.affected {
            if let Some(true) = self.history_read(
                self.witnessed_by(instance, id),
                "check failed component responsibility",
            ) {
                affected.push(id.clone());
            }
        }
        if let Some(held) = self.retire(instance) {
            affected.extend(held);
        }
        affected.sort();
        affected.dedup();
        let causes = self
            .history_read(
                self.known_causes(&processing.into_iter().collect::<Vec<_>>()),
                "record component failure",
            )
            .unwrap_or_default();
        self.record_kernel_error(
            "core.component_failed",
            &failure.message,
            &causes,
            json!({"component": instance, "operation": failure.operation,
                "phase": if processing.is_some() { "handle" } else { "restore" },
                "affected": affected, "ledger": self.ledger_path}),
        );
        self.settle_held(
            instance,
            &affected,
            "component_failure",
            "the responsible component became unavailable; whether the work happened is unknown",
        );
        self.kill_child_of(instance);
    }

    /// Remove a dead component: record the crash, drop its mailbox (the
    /// thread is orphaned if stuck), reconcile in-flight accounting. The
    /// kernel and every other component keep running.
    fn declare_dead(&mut self, instance: &str, processing: Option<&str>, why: &str) {
        let Some(held) = self.retire(instance) else {
            return;
        };
        let causes = self
            .history_read(
                self.known_causes(&processing.into_iter().collect::<Vec<_>>()),
                "record component crash",
            )
            .unwrap_or_default();
        self.record_kernel_event(
            ce::COMPONENT_CRASHED,
            &causes,
            json!({"component": instance, "processing": processing, "detail": why}),
        );
        self.settle_held(
            instance,
            &held,
            "crash",
            "the component holding this call died; whether the work happened is unknown",
        );
        self.kill_child_of(instance);
    }

    /// End the process of a component just declared unresponsive.
    ///
    /// This is the moment for it: the kernel has already sent a cancellation
    /// and waited out the grace, and what follows a declaration of death is
    /// not another wait. Nothing here used to kill anything — the only
    /// SIGTERM/SIGKILL in the codebase sat on the ordinary shutdown path,
    /// which a component declared dead never reaches — so the process, and
    /// every grandchild in its group, simply went on running.
    #[cfg(unix)]
    fn kill_child_of(&mut self, instance: &str) {
        signal_registered_groups(&self.child_groups, Some(instance), kill_group);
    }

    #[cfg(not(unix))]
    fn kill_child_of(&mut self, _instance: &str) {}

    /// Give an ending to every call this instance is holding as it leaves.
    ///
    /// Both kinds of call, which is the whole point of it being one function.
    /// Death and removal used to settle tool calls only, while replacement
    /// settled anything — and the comment on the third said in as many words
    /// that a model adapter leaving mid-call owes that call an ending just as
    /// much. It did; the other two were not giving it. An adapter that died
    /// with a call in its hands left the loop waiting for an answer nobody
    /// was going to send, and the conversation was over until restart.
    ///
    /// Only calls: a delivery can also be an input or an interruption being
    /// handled, and "interrupted" said of a user's message means nothing.
    fn settle_held(&mut self, instance: &str, held: &[String], by: &str, reason: &str) {
        for started in held {
            let Some(header) = self.history_read(self.log.header(started), "settle held call")
            else {
                continue;
            };
            let is_call = header.is_some_and(|e| {
                e.event_type == ce::TOOL_EXEC_STARTED || e.event_type == ce::MODEL_CALL_STARTED
            });
            if !is_call {
                continue;
            }
            let Some(ended) = self.history_read(self.has_outcome(started), "settle held call")
            else {
                continue;
            };
            if !ended {
                self.settle_chain(started, by, reason, json!({"component": instance}));
            }
        }
    }

    /// The ids of the calls `instance` is holding right now.
    fn held_by(&self, instance: &str) -> Vec<String> {
        self.outstanding
            .get(instance)
            .map(|queue| queue.iter().map(|o| o.event_id.clone()).collect())
            .unwrap_or_default()
    }

    /// Whether some later event already closed this chain — its own completion,
    /// or an interruption. Settling twice would be a second answer.
    fn has_outcome(&self, started_id: &str) -> std::io::Result<bool> {
        self.log.has_outcome(started_id)
    }

    /// One request, every copy of it: this start plus the starts it was
    /// relayed from. A station that passes a request on re-emits it citing
    /// what it received, so the requester witnessed an earlier copy than the
    /// one an ending lands on.
    ///
    /// The walk stops at the first cause that is not another copy — which is
    /// the entire reason it is written this way rather than as a general
    /// ancestor walk. Ancestry runs back through the model call that asked for
    /// the tool and on to the user's message, so "witnessed any ancestor"
    /// would make the model adapter an addressee for a tool's death. Refusing
    /// exactly that is what witnessing is for.
    fn relay_chain(&self, started_id: &str) -> std::io::Result<Vec<String>> {
        let Some(start) = self.log.header(started_id)? else {
            return Ok(vec![started_id.to_string()]);
        };
        let kind = start.event_type.clone();
        let mut chain = vec![start.id.clone()];
        let mut frontier = vec![start];
        while let Some(event) = frontier.pop() {
            for cause in &event.causes {
                let Some(prior) = self.log.header(cause)? else {
                    continue;
                };
                if prior.event_type == kind && !chain.contains(&prior.id) {
                    chain.push(prior.id.clone());
                    frontier.push(prior);
                }
            }
        }
        Ok(chain)
    }

    /// Take a component back out of the running assembly — the inverse of
    /// `install`, and the thing whose absence made installing a ONE-WAY DOOR:
    /// a bad install could only be undone by hand-editing the overlay, and a
    /// component that crashed on every start came back on every start.
    ///
    /// What it does NOT do is rewrite history. Everything this component did
    /// stays on the ledger; the removal is one more event on it. And the calls
    /// it was holding are settled, not abandoned — the same sentence used when
    /// a component dies under them, because from the caller's side it is the
    /// same situation.
    ///
    /// The kernel does not judge WHICH components may be removed. That rule
    /// lives with whoever services the request, because it is a rule about
    /// where a component came from, and the kernel does not know that.
    pub fn uninstall(
        &mut self,
        instance: &str,
        reason: &str,
        causes: &[&str],
    ) -> Result<(), String> {
        if !self.assembly.instances.contains_key(instance) {
            return Err(format!("no instance named \"{instance}\" is assembled"));
        }
        let causes = self
            .known_causes(causes)
            .map_err(|error| error.to_string())?;
        let component = self
            .manifests
            .get(instance)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let wires: Vec<Wire> = self
            .assembly
            .wires
            .iter()
            .filter(|wire| {
                wire.from.starts_with(&format!("{instance}."))
                    || wire.to.starts_with(&format!("{instance}."))
            })
            .cloned()
            .collect();

        // Whatever it was holding gets its ending first, while its witness set
        // still exists — the same ordering `declare_dead` needs
        self.settle_held(
            instance,
            &self.held_by(instance),
            "removed",
            "the component holding this call was taken out of the assembly",
        );

        // Dropping the mailbox is what tells a component to wind down: the
        // in-process thread's loop ends, and the process bridge sends `stop`
        // before its signals. Same shutdown path, one component at a time.
        if let Some(queue) = self.outstanding.remove(instance) {
            self.in_flight -= queue.len();
        }
        self.mailboxes.remove(instance);
        self.manifests.remove(instance);
        self.witnessed.remove(instance);
        self.threads.remove(instance);
        self.router.forget_instance(instance);
        self.assembly.instances.remove(instance);
        let leaving = format!("{instance}.");
        self.assembly
            .wires
            .retain(|wire| !wire.from.starts_with(&leaving) && !wire.to.starts_with(&leaving));
        // The definition goes with the last instance standing on it, so what
        // the kernel holds says the same thing the overlay will
        if !component.is_empty()
            && !self
                .assembly
                .instances
                .values()
                .any(|spec| spec.component == component)
        {
            self.registry.remove(&component);
        }
        self.tools
            .lock()
            .unwrap()
            .retain(|decl| decl["provider"] != instance);
        self.prompts
            .lock()
            .unwrap()
            .retain(|(owner, _)| owner != instance);

        let draft = EventDraft {
            event_type: ce::COMPONENT_REMOVED.to_string(),
            causes,
            origin: None,
            payload: json!({"component": component, "instance": instance, "wires": wires}),
            reason: Some(reason.to_string()),
        };
        self.log
            .append(draft, KERNEL_SOURCE)
            .map_err(|e| format!("the removal happened but could not be recorded: {e}"))?;
        Ok(())
    }

    /// Hand the kernel the recipes for building in-process components, so it
    /// can build one AFTER startup (see [`Kernel::replace`]).
    ///
    /// Separate from `start` because a factory map is not always the kernel's
    /// to keep: a stream template opens one stream per conversation from the
    /// same map, so a kernel that swallowed it would leave the next stream
    /// unable to build anything. A host that owns its map for one kernel hands
    /// it over here; one that shares it does not, and that kernel can then
    /// only say plainly that it has no recipe.
    pub fn adopt_factories(&mut self, factories: HashMap<String, Factory>) {
        self.factories = factories;
    }

    /// Swap the component and configuration behind ONE instance, leaving its
    /// name and every wire it sits on exactly as they were.
    ///
    /// This is what changing the model is. Not a per-call field: the endpoint
    /// and the API key are read once when an adapter is built and belong to
    /// the instance, and a different DIALECT is a different component
    /// altogether — so "use a different model" cannot be a value travelling on
    /// a call the way the effort setting does. It is the same seat in the
    /// assembly with a different occupant.
    ///
    /// The wires surviving is not luck. Both model adapters declare the same
    /// ports and claim the `model-adapter` port profile, and the re-inspection
    /// below is what enforces it: swap in something that does not fit the
    /// wires already attached to this seat and the replacement is refused
    /// before anything is torn down. That guarantee is exactly what port
    /// profiles were minted for, and this is its first real use.
    ///
    /// What it does not do, deliberately: rewrite anything. Everything the
    /// previous occupant did stays on the ledger, and the swap is one more
    /// event on it. Calls it was holding are settled rather than abandoned —
    /// from the caller's side, an occupant that left mid-call is the same
    /// situation as one that died mid-call.
    ///
    /// As with `uninstall`, the kernel does not judge WHICH instances may be
    /// replaced; that rule belongs to whoever services the request.
    pub fn replace(
        &mut self,
        instance: &str,
        component: &str,
        config: Option<Value>,
        reason: &str,
        causes: &[&str],
    ) -> Result<(), String> {
        let Some(spec) = self.assembly.instances.get(instance) else {
            return Err(format!("no instance named \"{instance}\" is assembled"));
        };
        let was = spec.component.clone();
        let Some(manifest) = self.registry.get(component).cloned() else {
            return Err(format!(
                "this build has no component called \"{component}\""
            ));
        };

        // Try the whole thing on paper first. A component that does not carry
        // what the wires into this seat deliver must be refused while the
        // current one is still running, not discovered after it is gone.
        let mut assembly = self.assembly.clone();
        assembly.instances.insert(
            instance.to_string(),
            ComponentInstance {
                component: component.to_string(),
                config: config.clone(),
                requires: spec.requires.clone(),
            },
        );
        let issues = inspect_assembly(&assembly, &self.registry);
        if !issues.is_empty() {
            let said: Vec<String> = issues
                .iter()
                .map(|issue| format!("{}: {}", issue.location, issue.problem))
                .collect();
            return Err(format!(
                "\"{component}\" does not fit where \"{instance}\" sits — {}",
                said.join("; ")
            ));
        }
        let causes = self
            .known_causes(causes)
            .map_err(|error| error.to_string())?;
        // Build the replacement BEFORE the current occupant is touched. The
        // port check above is not the only way this can be refused — a kernel
        // whose host shares its factories has no recipe to build from — and a
        // refusal discovered after the running instance had been stopped would
        // leave the conversation with an empty seat. Constructing a component
        // is inert: it reads its config, and nothing else has happened yet.
        let crew: Option<Vec<Box<dyn Component>>> = if manifest.runtime == RuntimeKind::Process {
            None // a process component is built by spawning, which cannot be undone
        } else {
            let workers = manifest.concurrency.unwrap_or(1).max(1);
            let factory = self.factories.get_mut(component).ok_or_else(|| {
                format!(
                    "this kernel was not given a recipe for \"{component}\" — its host \
                     shares its factories and cannot hand them over"
                )
            })?;
            Some((0..workers).map(|_| factory(config.as_ref())).collect())
        };
        self.log
            .register_types(&manifest.events)
            .map_err(|e| e.to_string())?;

        // Whatever it is holding gets its ending first, while its witness set
        // still exists. Not only tool calls: a model adapter leaving mid-call
        // owes that call an ending just as much, and "a call always ends" does
        // not have an exception for the ones this kernel finds inconvenient.
        self.settle_held(
            instance,
            &self.held_by(instance),
            "replaced",
            "the component holding this call was replaced",
        );

        // Out with the old. Dropping the mailbox is what winds it down; the
        // router is NOT told to forget the instance, which is the whole point.
        if let Some(queue) = self.outstanding.remove(instance) {
            self.in_flight -= queue.len();
        }
        self.mailboxes.remove(instance);
        self.threads.remove(instance);
        self.tools
            .lock()
            .unwrap()
            .retain(|decl| decl["provider"] != instance);
        self.prompts
            .lock()
            .unwrap()
            .retain(|(owner, _)| owner != instance);

        // In with the new, by whichever route its form requires.
        let (mail_tx, thread) = if manifest.runtime == RuntimeKind::Process {
            let lazy = config
                .as_ref()
                .and_then(|c| c.get("lazy"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            spawn_process_bridge(
                BridgeSeat {
                    instance: instance.to_string(),
                    entry: manifest.entry.clone(),
                    config: config.clone(),
                    stream: self.log.stream().to_string(),
                    lazy,
                    env_deny: self.child_env_deny.clone(),
                },
                self.tx.clone(),
                self.wake_tx.clone(),
                self.child_groups.clone(),
            )
            .map_err(|e| e.to_string())?
        } else {
            let crew = crew.expect("built above for every non-process component");
            let (restored_tx, restored_rx) = mpsc::channel::<(String, bool)>();
            let spawned = spawn_inproc(
                crew,
                instance.to_string(),
                self.tx.clone(),
                self.reader.clone(),
                self.foreign.clone(),
                Arc::clone(&self.prompts),
                Arc::clone(&self.tools),
                self.wake_tx.clone(),
                restored_tx,
                self.ledger_path.clone(),
            );
            // A failed replacement is never announced as installed.
            if !matches!(restored_rx.recv(), Ok((_, true))) {
                drop(spawned.0);
                let _ = spawned.1.join();
                return Err(format!(
                    "component {instance} could not restore; replacement is unavailable"
                ));
            }
            spawned
        };
        self.mailboxes.insert(instance.to_string(), mail_tx);
        self.threads.insert(instance.to_string(), thread);
        self.manifests
            .insert(instance.to_string(), manifest.clone());
        // A fresh occupant has witnessed nothing of this process. It does not
        // need to: what it will answer, it will have been delivered.
        self.witnessed.insert(instance.to_string(), HashSet::new());
        self.outstanding
            .insert(instance.to_string(), VecDeque::new());
        if let Some(text) = effective_prompt(&manifest, config.as_ref()) {
            self.prompts
                .lock()
                .unwrap()
                .push((instance.to_string(), text));
            self.prompts.lock().unwrap().sort_by(|a, b| a.0.cmp(&b.0));
        }
        {
            let mut tools = self.tools.lock().unwrap();
            for decl in &manifest.tools {
                let mut stamped = decl.clone();
                stamped["provider"] = json!(instance);
                tools.push(stamped);
            }
        }
        self.assembly = assembly;

        let draft = EventDraft {
            event_type: ce::COMPONENT_REPLACED.to_string(),
            causes,
            origin: None,
            payload: json!({
                "instance": instance,
                "from": was,
                "to": component,
                "config": config,
            }),
            reason: Some(reason.to_string()),
        };
        self.log
            .append(draft, KERNEL_SOURCE)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Settle a chain that will never get an outcome of its own.
    ///
    /// Not a fabricated RESULT — the truth is usually "outcome unknown": a
    /// shell may already have spawned, a file may already be gone. The one
    /// honest thing to say is that the chain was interrupted, which is the
    /// same sentence a restart already writes for chains it finds hanging
    /// (`settle_hanging_chains`). Here it is said at three more moments:
    /// a component dying under it, and nobody providing the tool at all.
    ///
    /// Delivered, not merely recorded, to whoever WITNESSED the request and
    /// declares an input for it. Witnessing is what makes the addressee right
    /// without a wire: the loop emitted the request, so it saw it; a model
    /// adapter never did, so a crashed tool cannot cancel its call.
    ///
    /// "The request" is the whole relay chain, not the one copy that got
    /// settled — see [`Kernel::relay_chain`]. A gate re-emits what it forwards,
    /// so in any assembly with a gate the copy that reaches nobody is the
    /// gate's, which the requester never saw. Matching on that copy alone left
    /// the ending recorded and delivered to no one: a hallucinated tool name,
    /// or an executor dying mid-call, hung the round forever in exactly the
    /// assembly the product ships, while every test — all wired straight from
    /// the loop to the tools — passed.
    fn settle_chain(&mut self, started_id: &str, by: &str, reason: &str, extra: Value) {
        let Some(exists) = self.history_read(self.log.contains_id(started_id), "settle call chain")
        else {
            return;
        };
        if !exists {
            return;
        }
        // A call that already ended is not settled again. Not a tidiness
        // rule: a second ending is a second ANSWER, and the two are both
        // materialized for the same call id, which every wire format refuses.
        // It happens for real — a tool that emits its result and then hangs
        // on the way out is past its deadline with the work already done, and
        // the watchman would write "interrupted" over a finished call.
        // Asked of the whole relay chain, because the result answers whatever
        // copy of the request the gate handed on, not this one.
        let Some(chain) = self.history_read(self.relay_chain(started_id), "settle call chain")
        else {
            return;
        };
        for copy in &chain {
            let Some(ended) = self.history_read(self.log.has_outcome(copy), "settle call chain")
            else {
                return;
            };
            if ended {
                return;
            }
        }
        let mut payload = json!({"by": by});
        if let Some(fields) = extra.as_object() {
            for (key, value) in fields {
                payload[key] = value.clone();
            }
        }
        let draft = EventDraft {
            event_type: ce::INTERRUPTED.to_string(),
            causes: vec![started_id.to_string()],
            origin: None,
            payload,
            reason: Some(reason.to_string()),
        };
        let Ok(event) = self.log.append(draft, KERNEL_SOURCE) else {
            return;
        };
        if self.stopping.is_some() {
            return;
        }
        let addressees: Vec<String> = self
            .manifests
            .iter()
            .filter(|(instance, manifest)| {
                manifest
                    .inputs
                    .iter()
                    .any(|port| port.events.iter().any(|e| e == ce::INTERRUPTED))
                    && self
                        .witnessed
                        .get(*instance)
                        .is_some_and(|seen| chain.iter().any(|copy| seen.contains(copy)))
            })
            .map(|(instance, _)| instance.clone())
            .collect();
        for instance in addressees {
            let port = self.manifests[&instance]
                .inputs
                .iter()
                .find(|port| port.events.iter().any(|e| e == ce::INTERRUPTED))
                .map(|port| port.name.clone())
                .expect("filtered on having such a port");
            let Some(mailbox) = self.mailboxes.get(&instance) else {
                continue;
            };
            self.witnessed
                .get_mut(&instance)
                .expect("witness set exists for every instance")
                .insert(event.id.clone());
            // One token, held at both ends. Two were being made here — the
            // component got one, the bookkeeping kept the other — so
            // cancelling this delivery cancelled a token nobody was holding.
            let token = CancellationToken::new();
            let deadline = self.delivery_deadline(&instance);
            if mailbox.send((port, event.clone(), token.clone())).is_ok() {
                self.outstanding
                    .entry(instance.clone())
                    .or_default()
                    .push_back(Outstanding {
                        event_id: event.id.clone(),
                        token,
                        deadline,
                        grace_deadline: None,
                    });
                self.in_flight += 1;
            }
        }
    }

    /// A refused emission may have been an ENDING.
    ///
    /// The four checks above turn a bad emission into an error event and drop
    /// it — but if what was dropped was a completion, the tool had already
    /// done the work and the caller is still waiting for an answer that was
    /// thrown away. The most likely way to reach this is a foreign component
    /// whose letter drifts from the schema by one field: the work happens,
    /// the result is refused, and the round waits forever.
    ///
    /// Only causes this emitter really WITNESSED are settled, and strictly —
    /// no reopen-time leniency. Ending a call is not something a component
    /// with a wrong idea of what it is answering may do to somebody else.
    fn settle_refused(&mut self, source: &str, event_type: &str, causes: &[String]) {
        if !matches!(
            event_type,
            ce::TOOL_EXEC_COMPLETED | ce::MODEL_CALL_COMPLETED
        ) {
            return;
        }
        let seen: Vec<String> = causes
            .iter()
            .filter(|cause| {
                self.witnessed
                    .get(source)
                    .is_some_and(|w| w.contains(cause.as_str()))
            })
            .cloned()
            .collect();
        for cause in seen {
            let Some(header) =
                self.history_read(self.log.header(&cause), "settle refused completion")
            else {
                continue;
            };
            let is_call = header.is_some_and(|e| {
                e.event_type == ce::TOOL_EXEC_STARTED || e.event_type == ce::MODEL_CALL_STARTED
            });
            if !is_call {
                continue;
            }
            let Some(ended) =
                self.history_read(self.has_outcome(&cause), "settle refused completion")
            else {
                continue;
            };
            if !ended {
                self.settle_chain(
                    &cause,
                    "rejected",
                    "the answer to this call was refused at the kernel's door and could \
                     not be recorded",
                    json!({"component": source}),
                );
            }
        }
    }

    /// Validate, record, deliver. Violations become core.control.error events
    /// — the kernel's own faults are audit-visible like everything else.
    fn dispatch(&mut self, source: String, port: String, draft: EventDraft) {
        let Some(manifest) = self.manifests.get(&source) else {
            let detail = json!({"source": source, "port": port, "eventType": draft.event_type});
            self.record_kernel_error(
                "core.unknown_instance",
                "emission from unknown instance",
                &draft.causes,
                detail,
            );
            return;
        };
        let Some(port_decl) = manifest.outputs.iter().find(|p| p.name == port) else {
            let detail = json!({"source": source, "port": port, "eventType": draft.event_type});
            self.record_kernel_error(
                "core.unknown_output_port",
                "emission from unknown output port",
                &draft.causes,
                detail,
            );
            self.settle_refused(&source, &draft.event_type, &draft.causes);
            return;
        };
        if !port_decl.events.contains(&draft.event_type) {
            let detail = json!({"source": source, "port": port, "eventType": draft.event_type});
            self.record_kernel_error(
                "core.undeclared_emission",
                "output port never declared this event type",
                &draft.causes,
                detail,
            );
            self.settle_refused(&source, &draft.event_type, &draft.causes);
            return;
        }
        for cause in &draft.causes {
            let Some(witnessed) = self.history_read(
                self.witnessed_by(&source, cause),
                "validate emission witness",
            ) else {
                return;
            };
            if !witnessed {
                let detail = json!({"source": source, "port": port, "cause": cause});
                self.record_kernel_error(
                    "core.cause_not_witnessed",
                    "cause not witnessed by emitter; the causal chain may not lie",
                    &[],
                    detail,
                );
                return;
            }
        }

        // Kept because `append` consumes the draft, and a refusal needs to
        // know what was refused.
        let refused = (draft.event_type.clone(), draft.causes.clone());
        let event = match self.log.append(draft, &source) {
            Ok(event) => event,
            Err(violation) => {
                self.record_kernel_error(
                    "core.audit_rejected",
                    &violation.to_string(),
                    &[],
                    json!({"source": source}),
                );
                self.settle_refused(&source, &refused.0, &refused.1);
                return;
            }
        };
        self.witnessed
            .get_mut(&source)
            .expect("witness set exists for every validated source")
            .insert(event.id.clone());

        if self.stopping.is_some() {
            return; // preserve late outcomes, but never start more work
        }
        let routes: Vec<(String, String)> = self.router.routes_from(&source, &port).to_vec();

        let mut delivered = 0usize;
        for (dest, dest_port) in routes {
            let Some(mailbox) = self.mailboxes.get(&dest) else {
                continue; // dead or unknown; inspection rules out unknown
            };
            // A tool request is ADDRESSED, not broadcast. Inspection already
            // guarantees each tool name has exactly one provider, so every
            // request has exactly one addressee — and the kernel already keeps
            // that map, because it is what offers the tool list to the model.
            //
            // Sending it to the others made every provider responsible for
            // recognising and discarding other people's mail. That is a
            // convention each one can get wrong, and across a process pipe it
            // is worse than wrong: staying quiet there is indistinguishable
            // from still working, so one call hung every other provider until
            // the watchman killed them.
            //
            // A component declaring NO tools is not an addressee at all: gates
            // and observers sit on this wire precisely to see everything, and
            // still do. A request nobody takes is settled below rather than
            // broadcast in the hope that somebody says "no such tool" — that
            // hope only paid off in assemblies containing a component willing
            // to say it, and the standard one contains none.
            if event.event_type == ce::TOOL_EXEC_STARTED {
                let addressed_elsewhere = self.manifests.get(&dest).is_some_and(|m| {
                    !m.tools.is_empty()
                        && !m
                            .tools
                            .iter()
                            .any(|decl| decl["name"] == event.payload["tool"])
                });
                if addressed_elsewhere {
                    continue;
                }
            }
            // A human interrupt must not wait in line: cancel the work the
            // destination is doing right now, before queueing the event
            if event.event_type == ce::INTERRUPTED {
                // Everything that instance is doing RIGHT NOW, which with
                // several workers is several things. Stopping only one of
                // three running commands is not stopping.
                let workers = self
                    .manifests
                    .get(&dest)
                    .and_then(|m| m.concurrency)
                    .unwrap_or(1)
                    .max(1);
                if let Some(queue) = self.outstanding.get_mut(&dest) {
                    for active in queue.iter_mut().take(workers) {
                        active.token.cancel();
                        active.deadline = None;
                        active.grace_deadline = Some(Instant::now() + self.grace_period);
                    }
                }
            }
            let token = CancellationToken::new();
            let deadline = self.delivery_deadline(&dest);
            self.witnessed
                .get_mut(&dest)
                .expect("witness set exists for every instance")
                .insert(event.id.clone());
            if mailbox
                .send((dest_port, event.clone(), token.clone()))
                .is_ok()
            {
                self.outstanding
                    .get_mut(&dest)
                    .expect("outstanding queue exists for every live instance")
                    .push_back(Outstanding {
                        event_id: event.id.clone(),
                        token,
                        deadline,
                        grace_deadline: None,
                    });
                self.in_flight += 1;
                delivered += 1;
            }
        }
        // A tool request that reached nobody. Whoever asked for it is counting
        // on an answer, and none is coming: no component provides that tool,
        // or the one that did has died. Broadcasting it in the hope that
        // somebody would answer "no such tool" only worked in assemblies that
        // happened to contain a component willing to say so — the standard one
        // contains none, so the round simply hung forever.
        //
        // A model call that reached nobody is the same fact about a different
        // family, and worth as much: once an adapter has died, every question
        // the loop asks from then on goes nowhere, and without this each one
        // waits forever for an answer that has no sender.
        if delivered == 0 {
            let (kind, reason) = match event.event_type.as_str() {
                ce::TOOL_EXEC_STARTED => ("tool", "no assembled component took this tool request"),
                ce::MODEL_CALL_STARTED => ("model", "no assembled component took this model call"),
                _ => ("", ""),
            };
            if !kind.is_empty() {
                let detail = json!({"tool": event.payload["tool"], "call": event.payload["call"]});
                self.settle_chain(&event.id, "no_provider", reason, detail);
            }
        }
    }

    fn known_causes(&self, causes: &[impl AsRef<str>]) -> std::io::Result<Vec<String>> {
        let mut known = Vec::new();
        for cause in causes {
            let id = cause.as_ref();
            if self.log.contains_id(id)? {
                known.push(id.to_string());
            }
        }
        Ok(known)
    }

    fn history_read<T>(&mut self, result: std::io::Result<T>, operation: &str) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.record_kernel_error(
                    "core.history_read_failed",
                    &error.to_string(),
                    &[],
                    json!({"operation": operation}),
                );
                None
            }
        }
    }

    /// The kernel's own faults must land in the log, with a machine code
    fn record_kernel_error(&mut self, code: &str, message: &str, causes: &[String], detail: Value) {
        let (causes, detail) = match self.known_causes(causes) {
            Ok(known) => (known, detail),
            Err(error) => (
                Vec::new(),
                json!({"original": detail, "unverifiedCauses": causes, "historyReadError": error.to_string()}),
            ),
        };
        let payload = json!({"code": code, "message": message, "detail": detail});
        self.record_kernel_event(ce::ERROR, &causes, payload);
    }

    /// Append an event on the kernel's own behalf. Even registered types and
    /// previously verified causes can be refused if historical reads fail.
    fn record_kernel_event(
        &mut self,
        event_type: &str,
        causes: &[impl AsRef<str>],
        payload: Value,
    ) {
        let draft = EventDraft {
            event_type: event_type.to_string(),
            causes: causes.iter().map(|c| c.as_ref().to_string()).collect(),
            origin: None,
            payload,
            reason: None,
        };
        if let Err(problem) = self.log.append(draft, KERNEL_SOURCE) {
            // Reading unavailable metadata is not a write failure, and must
            // not kill the kernel. The original event was refused, not retried.
            let fallback = EventDraft::new(
                ce::ERROR,
                &[],
                json!({
                    "code": "core.control_record_failed", "message": problem.to_string(),
                    "detail": {"eventType": event_type, "unverifiedCauses": causes.iter().map(AsRef::as_ref).collect::<Vec<_>>()},
                }),
            );
            if let Err(error) = self.log.append(fallback, KERNEL_SOURCE) {
                eprintln!("cannot record control event {event_type}: {problem}; diagnostic append also refused: {error}");
            }
        }
    }

    /// The prompt fragments the assembled components currently contribute,
    /// in the order they are stitched into the system prompt. Read-only, and
    /// the same view the context gate builds from — so "what exactly is this
    /// model being told" is answerable without making a call.
    pub fn prompt_fragments(&self) -> Vec<(String, String)> {
        self.prompts.lock().unwrap().clone()
    }

    /// Every tool currently offered to the model, provider-stamped. Read-only,
    /// and the same list the loop hands over — so "what can this agent do
    /// right now" is answerable without making a call.
    pub fn tool_decls(&self) -> Vec<Value> {
        self.tools.lock().unwrap().clone()
    }

    pub fn log(&self) -> &EventLog {
        &self.log
    }

    /// The live assembly manifest (hot installs extend it) — a read-only view
    /// for hosts computing suggested wires or writing overlays.
    pub fn assembly(&self) -> &AssemblyManifest {
        &self.assembly
    }

    /// The live component registry (hot installs extend it), read-only.
    pub fn component_registry(&self) -> &HashMap<String, ComponentManifest> {
        &self.registry
    }

    /// Whether `instance` has witnessed `event_id` — received it on delivery or
    /// emitted it. A host that answers a request in an instance's name must
    /// check this: causing an answer by an event the instance never witnessed
    /// is a violation the kernel rejects (the completion would never land). The
    /// durable past (present at reopen) counts as witnessed by everyone, matching
    /// the dispatch-time rule.
    pub fn witnessed_by(&self, instance: &str, event_id: &str) -> std::io::Result<bool> {
        if self
            .witnessed
            .get(instance)
            .is_some_and(|w| w.contains(event_id))
        {
            return Ok(true);
        }
        Ok(self
            .log
            .header(event_id)?
            .is_some_and(|event| event.seq <= self.reopened_through))
    }

    /// Subscribe a host-side observer to freshly appended events (e.g. a
    /// printer). Runs on the dispatcher's thread, inside append. Deliberately
    /// narrow — handing out `&mut EventLog` would let the host bypass the
    /// kernel's three enforcement layers.
    pub fn subscribe_log(&mut self, handler: impl FnMut(&EventEnvelope) + Send + 'static) -> u64 {
        self.log.subscribe(handler)
    }

    /// Stop all component threads and hand back the log. Threads of
    /// components declared dead were already orphaned and are not joined —
    /// joining a stuck thread would hang shutdown forever.
    /// Wind every component down and hand back the ledger.
    ///
    /// Bounded. "Stop accepting new work, let what is in flight finish within
    /// a limit" is the stated protocol, and the limit was the part missing:
    /// this waited on every thread with no way out, so one component stuck
    /// inside its handler — and the ones that hold no watchman deadline can
    /// be, the main loop and both gates among them — meant the session could
    /// not be quit at all, only killed. A thread still running when the
    /// limit passes is left to the end of the process, which is where it was
    /// going anyway.
    pub fn shutdown(self) -> EventLog {
        self.shutdown_observed().0
    }

    /// Close the kernel and return neutral cleanup observations to its host.
    pub fn shutdown_observed(mut self) -> (EventLog, crate::shutdown::KernelShutdownCost) {
        let mut timer = crate::startup::PhaseTimer::start();
        if let Some(stop) = self.stopping.clone() {
            for started_type in [ce::MODEL_CALL_STARTED, ce::TOOL_EXEC_STARTED] {
                let Some(hanging) =
                    self.history_read(self.log.hanging(started_type), "settle stopped stream")
                else {
                    continue;
                };
                for id in hanging {
                    let result = self
                        .log
                        .append(
                            EventDraft {
                                event_type: ce::INTERRUPTED.into(),
                                causes: vec![id, stop.clone()],
                                origin: None,
                                payload: json!({"by": "host"}),
                                reason: Some(
                                    "stream stopped; whether in-flight work happened is unknown"
                                        .into(),
                                ),
                            },
                            KERNEL_SOURCE,
                        )
                        .map_err(std::io::Error::other);
                    if self
                        .history_read(result, "record stopped call settlement")
                        .is_none()
                    {
                        break;
                    }
                }
            }
        }
        timer.checkpoint("settle");
        self.mailboxes.clear(); // mailbox senders drop → component loops exit
        let limit = Instant::now() + self.shutdown_timeout;
        let mut lingering = Vec::new();
        for (instance, thread) in self.threads.drain() {
            while !thread.is_finished() {
                if Instant::now() >= limit {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                lingering.push(instance);
            }
        }
        timer.checkpoint("components_join");
        // What looking back cost this session, when asked for. Behind a
        // variable because it is a diagnostic, not news — but it is the only
        // way to turn "reading the ledger is quadratic in the conversation"
        // from a shape into a number worth acting on.
        if std::env::var_os("LATTICE_STATS").is_some() {
            eprintln!("{}", self.log.cost().summary());
        }
        if !lingering.is_empty() {
            // Said out loud rather than swallowed: a component that would not
            // wind down is a fact about this assembly worth knowing, and the
            // ledger is already closed by the time we know it.
            lingering.sort();
            eprintln!(
                "warning: these components did not wind down within {:?} and were left \
                 to the process exit: {}",
                self.shutdown_timeout,
                lingering.join(", ")
            );
        }
        // Anything still running in a subprocess goes now: it cannot be left
        // to the process exit, because it is not in this process.
        signal_registered_groups(&self.child_groups, None, kill_group);
        (
            self.log,
            crate::shutdown::KernelShutdownCost {
                timings: timer.finish("subprocess_cleanup"),
                lingering,
            },
        )
    }
}

/// Give one in-process instance its mailbox and the thread(s) behind it.
///
/// Extracted so that starting an instance and REPLACING one go through the
/// same code. A second copy of this would be a second place for the crash
/// path, the restore barrier and the per-worker mailbox discipline to drift.
#[allow(clippy::too_many_arguments)]
fn spawn_inproc(
    crew: Vec<Box<dyn Component>>,
    instance_name: String,
    central: mpsc::Sender<Message>,
    log_reader: LogReader,
    foreign: ForeignReaders,
    thread_prompts: PromptFragments,
    thread_tools: ToolDecls,
    thread_wake: mpsc::Sender<()>,
    restored: mpsc::Sender<(String, bool)>,
    ledger_path: Option<PathBuf>,
) -> (mpsc::Sender<Delivery>, JoinHandle<()>) {
    let (mail_tx, mail_rx) = mpsc::channel::<Delivery>();
    // A supervisor so that one instance is still one JoinHandle, whatever
    // its crew size: shutdown, uninstall and crash handling all address
    // an instance, and none of them should have to know how many threads
    // are behind it.
    let thread = std::thread::spawn(move || {
        let post = Arc::new(Mutex::new(mail_rx));
        let ready = Arc::new((Mutex::new(None::<bool>), std::sync::Condvar::new()));
        let failed = CancellationToken::new();
        let mut crew_threads = Vec::with_capacity(crew.len());
        for (seat, mut component) in crew.into_iter().enumerate() {
            let post = Arc::clone(&post);
            let central = central.clone();
            let instance_name = instance_name.clone();
            let log_reader = log_reader.clone();
            let ledger_path = ledger_path.clone();
            let foreign = foreign.clone();
            let thread_prompts = Arc::clone(&thread_prompts);
            let thread_tools = Arc::clone(&thread_tools);
            let thread_wake = thread_wake.clone();
            let restored = restored.clone();
            let ready = Arc::clone(&ready);
            let failed = failed.clone();
            crew_threads.push(std::thread::spawn(move || {
                let make_ctx = |token: CancellationToken| Ctx {
                    source: instance_name.clone(),
                    cancellation: token,
                    log: log_reader.clone(),
                    foreign: foreign.clone(),
                    prompts: Arc::clone(&thread_prompts),
                    tools: Arc::clone(&thread_tools),
                    central: central.clone(),
                    wake: thread_wake.clone(),
                    out: Vec::new(),
                    failure: None,
                    ledger_path: ledger_path.clone(),
                };
                // One-time restore before the mailbox opens: rebuild
                // in-memory state from the ledger and re-arm background
                // wake sources lost to the previous process's death. A
                // panic here is a crash, like a panic in handle — the
                // kernel is not taken down. Only the first seat does it:
                // restoring is about the INSTANCE, and doing it once per
                // worker would re-arm every timer as many times.
                if seat == 0 {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut ctx = make_ctx(CancellationToken::new());
                        component.restore(&mut ctx);
                        ctx
                    }));
                    let success = match outcome {
                        Ok(ctx) => match ctx.failure {
                            Some(failure) => {
                                failed.cancel();
                                let _ = central.send(Message::Failed {
                                    instance: instance_name.clone(),
                                    event_id: None,
                                    failure,
                                });
                                false
                            }
                            None => {
                                for (port, draft) in ctx.out {
                                    let _ = central.send(Message::Emission {
                                        source: instance_name.clone(),
                                        port,
                                        draft,
                                    });
                                }
                                true
                            }
                        },
                        Err(_) => {
                            failed.cancel();
                            let _ = central.send(Message::Crashed {
                                instance: instance_name.clone(),
                                event_id: String::new(),
                            });
                            false
                        }
                    };
                    // Every outcome acknowledges the barrier, even with other
                    // workers and instances still holding channel senders.
                    *ready.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(success);
                    ready.1.notify_all();
                    let _ = restored.send((instance_name.clone(), success));
                    if !success {
                        return;
                    }
                } else {
                    let mut state = ready.0.lock().unwrap_or_else(|e| e.into_inner());
                    while state.is_none() {
                        state = ready.1.wait(state).unwrap_or_else(|e| e.into_inner());
                    }
                    if *state != Some(true) {
                        return;
                    }
                }
                loop {
                    // Held only across the receive: two workers take
                    // different deliveries, and neither waits on the
                    // other while it works.
                    let next = { post.lock().unwrap_or_else(|e| e.into_inner()).recv() };
                    let Ok((port, event, token)) = next else {
                        break;
                    };
                    if failed.is_cancelled() {
                        token.cancel();
                        break;
                    }
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut ctx = make_ctx(token);
                        component.handle(&port, &event, &mut ctx);
                        ctx
                    }));
                    match outcome {
                        Ok(ctx) => {
                            if let Some(failure) = ctx.failure {
                                failed.cancel();
                                let _ = central.send(Message::Failed {
                                    instance: instance_name.clone(),
                                    event_id: Some(event.id.clone()),
                                    failure,
                                });
                                break;
                            }
                            if failed.is_cancelled() {
                                break;
                            }
                            // Per-sender FIFO guarantees the dispatcher
                            // sees these before the Processed marker
                            for (out_port, draft) in ctx.out {
                                let _ = central.send(Message::Emission {
                                    source: instance_name.clone(),
                                    port: out_port,
                                    draft,
                                });
                            }
                            let _ = central.send(Message::Processed {
                                instance: instance_name.clone(),
                                event: event.id.clone(),
                            });
                        }
                        Err(_) => {
                            failed.cancel();
                            let _ = central.send(Message::Crashed {
                                instance: instance_name.clone(),
                                event_id: event.id.clone(),
                            });
                            break;
                        }
                    }
                }
            }));
        }
        for worker in crew_threads {
            let _ = worker.join();
        }
    });
    (mail_tx, thread)
}
