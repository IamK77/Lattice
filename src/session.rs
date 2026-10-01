//! The frontend↔core boundary, as pure data.
//!
//! A `Session` owns the core (a `Kernel`) on a background thread, so a
//! multi-second model call never freezes the frontend. Frontend and core
//! talk over exactly two message types — both serializable. In-process they
//! flow through in-memory channels; the day a daemon is wanted, the SAME two
//! types flow over a socket (the bridge protocol) and the frontend code does
//! not change. That is the day-one bet ("composable into distributed") raised
//! to the frontend boundary.
//!
//! Slash commands are NOT here: they are a frontend-side parsing convenience
//! that the driver turns into a command, a host call, or an event. The core
//! knows nothing about slashes.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Injector, Kernel, KernelError};

/// Frontend → core. Sent between turns (things that start work).
/// Interrupt is deliberately NOT here: it must reach a RUNNING model call, so
/// the frontend holds an injector and fires it directly (see `Session::interrupt`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendCommand {
    /// The user typed a line
    SendText { text: String },
    /// The user answered an authorization request (the trust gate's event
    /// pair): `request` is the id of the trust.authorization_requested event
    Authorize { request: String, approve: bool },
    /// Internal: an injection fired a wake, so run a turn. NOT sent by the
    /// frontend — the session's own wake forwarder produces it, so any wake
    /// source (a background command, a timer) drives a turn the same way a
    /// user message does.
    Wake,
    /// Turn the thinking dial for the rest of this conversation, and record
    /// it as the user's standing choice. `value` is the neutral thinking
    /// value, not a dialect's word — each adapter translates it.
    SetEffort { value: Value },
    /// Change the model itself, by its short name in the catalog. Handled on
    /// the thread that owns the kernel, because it replaces components — the
    /// same reason an install is serviced there.
    SetModel { id: String },
    /// Take an installed component out of the running assembly. Handled on
    /// the thread that owns the kernel, because rewiring is the kernel's own
    /// business — the same reason an install is serviced there.
    Uninstall { instance: String },
    /// The user added or deleted a model in the catalog. The file was already
    /// written when this is sent — this records that it happened.
    NoteCatalogChange(CatalogNote),
    /// What the turn that just ended cost the frontend to draw.
    NoteRenderCost(RenderCostNote),
    /// Shut the session down
    Shutdown,
}

/// What a catalog edit is allowed to say on the ledger.
///
/// Spelled out field by field rather than carrying the entry as the file
/// writes it, and that is the whole point of the type: an entry may hold a key
/// in full (`apiKey`), and a ledger cannot take a secret back once it is on it.
/// Nothing here can hold one — `key_env` is a variable NAME, which is worth
/// nothing to whoever reads it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogNote {
    /// "added" or "removed"
    pub action: String,
    pub id: String,
    pub model: String,
    pub adapter: String,
    /// The endpoint host, as the panel shows it
    pub endpoint: String,
    /// Which environment variable holds the key, or empty when the entry
    /// carries the key itself
    pub key_env: String,
}

/// What one turn cost the frontend to draw.
///
/// The frontend is the one part of the runtime whose work never touched the
/// ledger, so "it got slow" was a thing only a person at the keyboard could
/// notice, and only while it was happening. One entry per turn makes it a
/// fact the agent can read back about itself — and the numbers that matter
/// are the ones that grow: frames against transcript size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderCostNote {
    /// Frames actually built
    pub frames: usize,
    /// Loop passes that needed no frame — the idle work not done
    pub skipped: usize,
    pub total_ms: f64,
    /// The slowest single frame, which is what a person feels as a stutter
    pub worst_ms: f64,
    /// How long the transcript was, since the cost scales with it
    pub entries: usize,
    #[serde(default)]
    pub memory: Option<crate::memory::Snapshot>,
    #[serde(default)]
    pub history: Value,
}

/// One successful TUI launch, from main entry through the first completed draw.
/// `kernel` is nested inside the frontend's `session_start` phase; do not add
/// its durations to the outer total. Timestamps are labels, not elapsed clocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupNote {
    pub started_at: String,
    pub first_frame_at: String,
    pub resumed: bool,
    pub history_events: usize,
    pub frontend: crate::startup::Timings,
    pub kernel: crate::startup::Timings,
    #[serde(default)]
    pub replay_memory: crate::memory::Breakdown,
    #[serde(default)]
    pub history: Value,
    #[serde(default)]
    pub ui_counts: Value,
}

/// A completed model swap: what to tell the person, and the event that tells
/// the context gate what it is now budgeting for.
struct Swapped {
    note: String,
    dial: EventDraft,
}

/// Read back which model is actually running, from the running assembly.
///
/// Not from a copy of the startup config: the assembly IS what is running, so
/// reading it there cannot drift from the truth — and after one swap a startup
/// config would be describing a model that left.
fn running_model(kernel: &Kernel) -> Result<(crate::models::Entry, Option<Value>), String> {
    let instance = kernel
        .assembly()
        .instances
        .get(crate::preset::MAIN_MODEL)
        .ok_or_else(|| "this session has no main model instance".to_string())?;
    let config = instance.config.clone().unwrap_or(Value::Null);
    let field = |key: &str| config.get(key).and_then(Value::as_str).unwrap_or_default();
    let model = field("model");
    if model.is_empty() {
        return Err("this session runs a scripted model, which has nowhere to \
                    swap to"
            .to_string());
    }
    Ok((
        crate::models::Entry {
            id: model.to_string(),
            adapter: crate::preset::adapter_of(&instance.component).to_string(),
            model: model.to_string(),
            base_url: field("baseUrl").to_string(),
            key_env: field("apiKeyEnv").to_string(),
            // Only the complete retained profile may override the catalog.
            profile: config
                .get("profile")
                .filter(|value| !value.is_null())
                .cloned(),
        },
        config.get("thinking").cloned(),
    ))
}

/// Every reason a swap is refused, as a rule rather than as code buried in the
/// middle of one — so it can be checked without a running kernel, and so the
/// wording a person reads can be checked at all.
///
/// `key_present` is passed in rather than read here: whether a variable is set
/// is a fact about the process, and a rule that consulted the environment
/// could only be tested by changing it.
fn refuse_swap(
    running: &crate::models::Entry,
    entry: &crate::models::Entry,
    key_present: bool,
) -> Option<String> {
    if entry.same_target(running) {
        return Some(format!("already running {} — nothing changed", entry.model));
    }
    if !key_present {
        // The one that matters most. Swapping to a model that cannot be
        // reached leaves a conversation whose every next turn fails, and the
        // only honest moment to say "you do not have that key" is before the
        // swap, not on the ledger afterwards.
        return Some(format!(
            "{} needs {} in the environment, and it is not set — not swapping, \
             because every turn after it would fail",
            entry.model, entry.key_env
        ));
    }
    None
}

/// Swap the model the conversation is running on, main and condenser together.
/// Everything that can be refused (see [`refuse_swap`]) is refused before
/// anything is torn down.
fn swap_model(kernel: &mut Kernel, id: &str) -> Result<Swapped, String> {
    let (running, thinking) = running_model(kernel)?;
    let entry = crate::models::find(id, &running)
        .ok_or_else(|| format!("no model called \"{id}\" is configured"))?;
    if let Some(problem) = refuse_swap(&running, &entry, entry.key_present()) {
        return Err(problem);
    }

    let component = crate::preset::brain_name(&entry.adapter);
    let reason = format!(
        "the user chose the model \"{}\" ({} at {})",
        entry.id, entry.model, entry.base_url
    );
    kernel.replace(
        crate::preset::MAIN_MODEL,
        component,
        Some(crate::preset::main_model_config(&entry, thinking.as_ref())),
        &reason,
        &[],
    )?;
    // The condenser follows the main model. A failure here is reported but
    // does not undo the swap: an assembly with no condenser is a working
    // assembly (the gate simply stops condensing), while a half-swapped one —
    // main model on one provider, condenser on another — is not.
    let mut note = format!("model is now {} ({})", entry.model, entry.base_url);
    if kernel
        .assembly()
        .instances
        .contains_key(crate::preset::CONDENSE_MODEL)
    {
        let mut config = crate::preset::condenser_config(&entry);
        if thinking.is_some() {
            config["thinking"] = json!(false);
        }
        if let Err(problem) = kernel.replace(
            crate::preset::CONDENSE_MODEL,
            component,
            Some(config),
            &reason,
            &[],
        ) {
            note.push_str(&format!(" — but the condenser did not follow: {problem}"));
        }
    }

    // What the gate needs to know about the new model: what to call it in the
    // prompt, and what to budget with. Sent as an event rather than by
    // rebuilding the gate, because rebuilding it would discard the effort a
    // person turned five minutes ago, which lives only in its memory.
    let mut dialed = json!({
        "channel": crate::components::context_gate::MODEL_CHANNEL,
        "model": entry.model,
        "usageFields": entry.usage_fields(),
        "nativeCompaction": entry.adapter == "responses",
        "nativeTarget": {"model": entry.model, "baseUrl": entry.base_url},
    });
    match entry.context_window() {
        Some(window) => dialed["contextWindow"] = json!(window),
        // No profile: say nothing about the window rather than a number nobody
        // measured. The gate keeps what it had, and the note says so, because
        // a budget quietly belonging to the previous model is exactly the kind
        // of thing that is discovered from a bill.
        None => note.push_str(
            " — no profile ships for it, so the context budget is still the \
             previous model's",
        ),
    }
    Ok(Swapped {
        note,
        dial: EventDraft::new(ce::EXTERNAL_INPUT, &[], dialed),
    })
}

fn assembled(kernel: &Kernel) -> Result<Vec<crate::Assembled>, String> {
    let installed = crate::workshop::live_installed(kernel)?;
    let mut rows: Vec<_> = kernel
        .assembly()
        .instances
        .iter()
        .map(|(name, spec)| {
            let manifest = kernel.component_registry().get(&spec.component);
            let runtime = match manifest.map(|m| &m.runtime) {
                Some(crate::RuntimeKind::Inproc) => "in-process",
                Some(_) => "subprocess",
                None => "?",
            };
            let tools = manifest
                .map(|m| {
                    m.tools
                        .iter()
                        .filter_map(|tool| tool["name"].as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let wires = kernel
                .assembly()
                .wires
                .iter()
                .filter(|wire| {
                    wire.from.starts_with(&format!("{name}."))
                        || wire.to.starts_with(&format!("{name}."))
                })
                .map(|wire| format!("{} → {}", wire.from, wire.to))
                .collect();
            (
                name.clone(),
                spec.component.clone(),
                runtime,
                tools,
                installed.contains(name),
                wires,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}

/// Core → frontend. What the screen renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RenderEvent {
    /// A completed event was appended to the ledger (render it). Boxed
    /// because an envelope dwarfs the other variants.
    Appended(Box<EventEnvelope>),
    /// TUI-local subscription timestamp. Clock values are meaningful only
    /// within the identified process; the boundary still carries pure data.
    InputObserved {
        event: String,
        pid: u32,
        clock_ns: u64,
    },
    /// A transient streaming fragment (live typing); never recorded
    Notice { source: String, payload: Value },
    /// A turn finished; the frontend may re-enable input
    Quiescent,
    /// The session could not start
    StartupFailed(String),
}

/// How the frontend drives one running conversation.
pub struct Session {
    commands: mpsc::Sender<FrontendCommand>,
    renders: mpsc::Receiver<RenderEvent>,
    /// Held so interrupts reach a running turn immediately, bypassing the
    /// command queue (which a blocked `run_until_quiescent` would not read)
    interrupter: Injector,
    interface_id: Option<String>,
    operation_authorization: bool,
    authorization_sources: crate::components::operation_policy::AuthorizationSources,
    stop: crate::kernel::host::StopHandle,
    startup_cost: crate::startup::Timings,
    reader: crate::kernel::log::LogReader,
    stopping: Arc<AtomicBool>,
    shutdown_started: Arc<Mutex<Option<Instant>>>,
    initial_parts: Vec<crate::Assembled>,
    handle: Option<JoinHandle<Option<crate::shutdown::SessionShutdown>>>,
}

impl Session {
    /// Spawn the core on a background thread. `ui_instance` is the frontend
    /// socket's instance name; user text and interrupts are injected through
    /// it. `build` constructs the kernel ON the thread (so nothing un-Send
    /// crosses the boundary), wiring the given render sender into the log
    /// subscriber and notice handler before returning.
    pub fn spawn(
        ui_instance: &str,
        build: impl FnOnce(mpsc::Sender<RenderEvent>) -> Result<Kernel, KernelError> + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_with(ui_instance, None, build)
    }

    /// As [`Session::spawn`], plus the workshop locations this host installs
    /// into. Without one the session still runs — it simply cannot install,
    /// which is right for a test or a throwaway session.
    pub fn spawn_with(
        ui_instance: &str,
        workshop: Option<crate::workshop::Workshop>,
        build: impl FnOnce(mpsc::Sender<RenderEvent>) -> Result<Kernel, KernelError> + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_with_subagents(ui_instance, workshop, None, build)
    }

    /// As [`Session::spawn_with`], plus the streams a subagent runs in.
    ///
    /// The conversation stays where it is — this session owns and drives its
    /// own kernel, and gaining subagents must not change that. `experts` is a
    /// separate host holding ONLY the streams a subagent runs in, built on
    /// this thread for the same reason the kernel is.
    pub fn spawn_with_subagents(
        ui_instance: &str,
        workshop: Option<crate::workshop::Workshop>,
        experts: Option<(
            String,
            Box<dyn FnOnce() -> crate::kernel::stream_host::StreamHost + Send>,
        )>,
        build: impl FnOnce(mpsc::Sender<RenderEvent>) -> Result<Kernel, KernelError> + Send + 'static,
    ) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<FrontendCommand>();
        let (render_tx, render_rx) = mpsc::channel::<RenderEvent>();
        let (ready_tx, ready_rx) = mpsc::channel::<
            Result<
                (
                    Injector,
                    crate::kernel::host::StopHandle,
                    crate::startup::Timings,
                    crate::kernel::log::LogReader,
                    Vec<crate::Assembled>,
                    Option<String>,
                    bool,
                    crate::components::operation_policy::AuthorizationSources,
                ),
                String,
            >,
        >();
        let ui_instance = ui_instance.to_string();
        let wake_cmd_tx = cmd_tx.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = stopping.clone();
        let shutdown_started = Arc::new(Mutex::new(None));
        let worker_started = shutdown_started.clone();

        let handle = std::thread::spawn(move || {
            let mut kernel = match build(render_tx.clone()) {
                Ok(kernel) => kernel,
                Err(err) => {
                    let _ = render_tx.send(RenderEvent::StartupFailed(err.to_string()));
                    let _ = ready_tx.send(Err(err.to_string()));
                    return None;
                }
            };
            let injector = kernel.injector(&ui_instance);
            let answer_service = |state: &str| {
                kernel.assembly().wires.iter().find_map(|wire| {
                    if wire.from != format!("{ui_instance}.answer") {
                        return None;
                    }
                    let (instance, _) = wire.to.rsplit_once('.')?;
                    let spec = kernel.assembly().instances.get(instance)?;
                    let manifest = kernel.component_registry().get(&spec.component)?;
                    manifest
                        .outputs
                        .iter()
                        .any(|port| port.events.iter().any(|kind| kind == state))
                        .then(|| instance.to_owned())
                })
            };
            let operations = answer_service(crate::components::operation_policy::STATE);
            let interfaces = answer_service(crate::components::interface_permissions::STATE);
            let operation_authorization = operations.is_some();
            let interface_id = interfaces.as_ref().map(|_| {
                crate::components::interface_permissions::new_instance_id(&kernel.log().reader())
            });
            let mut authorization_sources =
                crate::components::operation_policy::AuthorizationSources::default();
            if let Some(source) = operations {
                authorization_sources.operations = source;
            }
            if let Some(source) = interfaces {
                authorization_sources.interfaces = source;
            }
            // Built here, on the thread, for the same reason the kernel is.
            let mut subagents = experts.map(|(_main_id, build)| {
                // Only the kernel knows the stream it actually opened. A new
                // ledger does not exist when the frontend prepares this call.
                let main_id = kernel.log().stream().to_string();
                (crate::subagent_host::SubagentHost::new(), build(), main_id)
            });

            // The push loop: every injection into the core fires a wake;
            // forward each batch as one `Wake` command so a turn runs. This
            // is what lets background commands and timers drive turns in-
            // process, exactly as they do in the daemon.
            if let Some(wake_rx) = kernel.take_wake_receiver() {
                std::thread::spawn(move || {
                    while wake_rx.recv().is_ok() {
                        while wake_rx.try_recv().is_ok() {}
                        if wake_cmd_tx.send(FrontendCommand::Wake).is_err() {
                            break;
                        }
                    }
                });
            }

            let mut parts = match assembled(&kernel) {
                Ok(parts) => parts,
                Err(error) => {
                    let _ = render_tx.send(RenderEvent::StartupFailed(error.clone()));
                    let _ = ready_tx.send(Err(error));
                    kernel.shutdown();
                    return None;
                }
            };
            // A gate-less embedding keeps its original startup/quiescence
            // behavior; do not inject an unhandled lifecycle wake there.
            if let Some(id) = &interface_id {
                injector.emit(
                    "answer",
                    EventDraft::new(
                        ce::EXTERNAL_INPUT,
                        &[],
                        json!({
                            "channel":crate::components::interface_permissions::CHANNEL,
                            "interface":id,"action":"open",
                        }),
                    ),
                );
            }
            if ready_tx
                .send(Ok((
                    injector.clone(),
                    kernel.stop_handle(),
                    kernel.startup_cost().clone(),
                    kernel.log().reader(),
                    parts.clone(),
                    interface_id.clone(),
                    operation_authorization,
                    authorization_sources,
                )))
                .is_err()
            {
                return None;
            }

            while let Ok(command) = cmd_rx.recv() {
                if worker_stopping.load(Ordering::Acquire) {
                    break;
                }
                match command {
                    FrontendCommand::SendText { text } => {
                        // Only inject; the wake it fires arrives as `Wake`
                        injector.emit(
                            "user",
                            EventDraft::new(
                                ce::USER_MESSAGE,
                                &[],
                                json!({ "text": text, "interface":interface_id }),
                            ),
                        );
                    }
                    FrontendCommand::Authorize { request, approve } => {
                        injector.emit(
                            "answer",
                            EventDraft::new(
                                ce::EXTERNAL_INPUT,
                                &[],
                                json!({
                                    "channel": crate::components::trust_policy::AUTH_CHANNEL,
                                    "request": request,
                                    "approve": approve,
                                    "interface": interface_id,
                                }),
                            ),
                        );
                    }
                    FrontendCommand::SetEffort { value } => {
                        // Two effects, deliberately: the injected event turns
                        // it for THIS conversation (and puts the act on the
                        // ledger, where every later call shows what it ran
                        // at), and the preference makes it the standing
                        // choice, which the ledger alone could never do —
                        // the next conversation has no ledger yet.
                        injector.emit(
                            "answer",
                            EventDraft::new(
                                ce::EXTERNAL_INPUT,
                                &[],
                                json!({
                                    "channel": crate::components::context_gate::EFFORT_CHANNEL,
                                    "value": value,
                                }),
                            ),
                        );
                        // The cost this command carries, said at the moment
                        // it is paid rather than discovered on a bill.
                        // Anthropic documents that effort shapes the rendered
                        // prompt, so changing it mid-conversation does not
                        // preserve the cached prefix; whether DeepSeek's cache
                        // behaves the same is untested, which is itself a
                        // reason to warn rather than stay quiet.
                        let cost = "the next call starts from a cold prompt cache";
                        let note = match crate::preferences::set("thinking", value.clone()) {
                            Ok(_) => {
                                format!("thinking set to {value}, saved as your default — {cost}")
                            }
                            Err(problem) => format!(
                                "thinking set to {value} for this session only \
                                 ({problem}) — {cost}"
                            ),
                        };
                        let _ = render_tx.send(RenderEvent::Notice {
                            source: "preferences".to_string(),
                            payload: json!({"note": note}),
                        });
                    }
                    FrontendCommand::SetModel { id } => {
                        let note = match swap_model(&mut kernel, &id) {
                            Ok(done) => {
                                // Same two effects as the effort dial: the
                                // injected event turns it for THIS conversation
                                // and puts the act on the ledger; the preference
                                // makes it the standing choice, which no ledger
                                // could do for a conversation that does not
                                // exist yet.
                                injector.emit("answer", done.dial);
                                match crate::preferences::set("model", json!(id)) {
                                    Ok(_) => format!("{}, saved as your default", done.note),
                                    Err(problem) => {
                                        format!("{} for this session only ({problem})", done.note)
                                    }
                                }
                            }
                            Err(problem) => problem,
                        };
                        let _ = render_tx.send(RenderEvent::Notice {
                            source: "model".to_string(),
                            payload: json!({ "note": note }),
                        });
                        let _ = render_tx.send(RenderEvent::Quiescent);
                    }
                    FrontendCommand::Wake => {
                        // Not a bare run: the agent may have asked to install a
                        // tool, and installing rewires the kernel itself — only
                        // the host that owns it can do that. Servicing here (and
                        // in the daemon, at its own quiet point) is what makes
                        // "the agent can install a tool" true of the product and
                        // not just of an example.
                        let _ = match &workshop {
                            Some(shop) => shop.run(&mut kernel),
                            None => kernel.run_until_quiescent(),
                        };
                        if !worker_stopping.load(Ordering::Acquire) && !kernel.is_stopping() {
                            if let Err(error) = run_experts(&mut kernel, &mut subagents) {
                                let _ = render_tx.send(RenderEvent::Notice {
                                    source: "session".into(),
                                    payload: json!({"note": format!("subagent dispatch failed: {error}")}),
                                });
                            }
                        }
                        let _ = render_tx.send(RenderEvent::Quiescent);
                    }
                    FrontendCommand::Uninstall { instance } => {
                        let outcome = match &workshop {
                            Some(shop) => shop.remove(&mut kernel, &instance),
                            None => Err("this session cannot install or remove".to_string()),
                        };
                        if let Err(problem) = outcome {
                            let _ = render_tx.send(RenderEvent::Notice {
                                source: "workshop".to_string(),
                                payload: json!({"note": problem}),
                            });
                        }
                        let _ = render_tx.send(RenderEvent::Quiescent);
                    }
                    FrontendCommand::NoteCatalogChange(note) => {
                        // Recorded after the fact, which is the rule the ledger
                        // keeps everywhere: an event is a completed state. The
                        // file is already written by the time this arrives.
                        injector.emit(
                            "catalog",
                            EventDraft::new(
                                crate::components::silent_ui::CATALOG_CHANGED,
                                &[],
                                json!({
                                    "action": note.action,
                                    "id": note.id,
                                    "model": note.model,
                                    "adapter": note.adapter,
                                    "endpoint": note.endpoint,
                                    "keyEnv": note.key_env,
                                }),
                            )
                            .with_reason(&format!(
                                "the user {} the model \"{}\" in the catalog",
                                note.action, note.id
                            )),
                        );
                    }
                    FrontendCommand::NoteRenderCost(note) => {
                        injector.emit(
                            "stats",
                            EventDraft::new(
                                crate::components::silent_ui::RENDER_COST,
                                &[],
                                json!({
                                    "frames": note.frames,
                                    "skipped": note.skipped,
                                    "totalMs": note.total_ms,
                                    "worstMs": note.worst_ms,
                                    "entries": note.entries,
                                    "memory": note.memory,
                                    "history": note.history,
                                }),
                            ),
                        );
                    }
                    FrontendCommand::Shutdown => break,
                }
                let current = match assembled(&kernel) {
                    Ok(current) => current,
                    Err(error) => {
                        // Keep the last display, but do not advertise unverifiable removal rights.
                        for row in &mut parts {
                            row.4 = false;
                        }
                        let _ = render_tx.send(RenderEvent::Notice {
                            source: "assembly".into(),
                            payload: json!({"parts": parts, "note": format!("component provenance unavailable; removal disabled: {error}")}),
                        });
                        continue;
                    }
                };
                if current != parts {
                    let _ = render_tx.send(RenderEvent::Notice {
                        source: "assembly".into(),
                        payload: json!({"parts":current}),
                    });
                    parts = current;
                }
            }
            let began = worker_started
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or_else(Instant::now);
            let mut timer = crate::startup::PhaseTimer::since(began);
            if let Some((host, _, _)) = subagents.as_ref() {
                host.request_shutdown();
            }
            // Also consume the stop on an idle session, where no Wake ran it.
            kernel
                .stop_handle()
                .request("session host is shutting down".into(), None);
            let _ = kernel.run_until_quiescent();
            timer.checkpoint("active_drain");
            // Children report through the parent's injector: join them while
            // that inbox and ledger are still alive, then take a finite cut.
            drop(subagents);
            timer.checkpoint("experts_shutdown");
            kernel.drain_stopped_inputs();
            timer.checkpoint("final_messages");
            let (log, kernel_cost) = kernel.shutdown_observed();
            Some(crate::shutdown::SessionShutdown {
                log,
                timings: timer.finish("kernel_shutdown"),
                kernel: kernel_cost,
            })
        });

        match ready_rx.recv() {
            Ok(Ok((
                interrupter,
                stop,
                startup_cost,
                reader,
                initial_parts,
                interface_id,
                operation_authorization,
                authorization_sources,
            ))) => Ok(Self {
                interface_id,
                operation_authorization,
                authorization_sources,
                stop,
                initial_parts,
                startup_cost,
                reader,
                stopping,
                shutdown_started,
                commands: cmd_tx,
                renders: render_rx,
                interrupter,
                handle: Some(handle),
            }),
            Ok(Err(err)) => Err(err),
            Err(_) => Err("session thread died during startup".to_string()),
        }
    }

    /// Ask for an installed component to be taken out.
    pub fn uninstall(&self, instance: impl Into<String>) {
        let _ = self.commands.send(FrontendCommand::Uninstall {
            instance: instance.into(),
        });
    }

    /// Turn the thinking dial. Takes effect on the next call and is saved as
    /// the standing choice.
    pub fn set_effort(&self, value: Value) {
        let _ = self.commands.send(FrontendCommand::SetEffort { value });
    }

    /// Request one context-compaction attempt, even while the main loop is busy.
    /// This is an external control input, not a user message or model change.
    pub fn request_compaction(&self) {
        self.interrupter.emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({
                    "channel": crate::components::context_gate::COMPACT_CHANNEL,
                }),
            ),
        );
    }

    /// Run one expert-management operation through the ordinary provider and
    /// authorization route, even while a model turn is busy. The frontend's
    /// correlation ID is returned by experts.ui.result; this is not chat input.
    pub fn manage_experts(&self, request: &str, operation: &str, arguments: Value) {
        self.interrupter.emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({
                    "channel":crate::components::expert_ui::CHANNEL,
                    "request":request,"operation":operation,"arguments":arguments,
                    "interface":self.interface_id,"workInput":true,
                }),
            ),
        );
    }

    /// Put a catalog edit on the ledger. The file is written by the caller —
    /// it is the frontend's own file and the person is owed an answer at the
    /// keystroke, not after a round trip — so this reports rather than asks.
    pub fn note_catalog_change(&self, note: CatalogNote) {
        let _ = self.commands.send(FrontendCommand::NoteCatalogChange(note));
    }

    /// Put what the last turn cost to draw on the ledger. Dropped silently if
    /// the session is gone: a measurement is never worth failing a turn over.
    pub fn note_render_cost(&self, mut note: RenderCostNote) {
        note.history = self.memory_history_note();
        let _ = self.commands.send(FrontendCommand::NoteRenderCost(note));
    }

    /// First successful frame after absorbing a submitted message; never a local echo.
    pub fn note_input_cost(&self, note: crate::input_latency::Note) {
        self.interrupter.emit(
            "stats",
            EventDraft::new(
                crate::components::silent_ui::INPUT_COST,
                &[&note.event],
                serde_json::to_value(&note).expect("input timing serializes"),
            ),
        );
    }

    fn memory_history_note(&self) -> Value {
        match self.log_reader().memory_stats() {
            Ok(stats) => json!({"at": chrono::Utc::now().to_rfc3339(), "stats": stats}),
            Err(error) => json!({"error": error.to_string()}),
        }
    }

    pub fn startup_cost(&self) -> &crate::startup::Timings {
        &self.startup_cost
    }

    /// Inject directly so an already running turn cannot hold the observation
    /// in the command queue. The event's timestamp may still follow the draw.
    pub fn note_startup(&self, mut note: StartupNote) {
        note.history = self.memory_history_note();
        self.interrupter.emit(
            "stats",
            EventDraft::new(
                crate::components::silent_ui::STARTUP_COST,
                &[],
                serde_json::to_value(note).expect("startup measurements serialize"),
            ),
        );
    }

    /// Change the model, by its short name in the catalog. Takes effect on the
    /// next call and is saved as the standing choice; the conversation carries
    /// on, because the material is a list of ledger pointers and the new model
    /// reads the same history the old one did.
    pub fn set_model(&self, id: impl Into<String>) {
        let _ = self
            .commands
            .send(FrontendCommand::SetModel { id: id.into() });
    }

    /// Send a typed line (starts a turn).
    /// Say something — reaching the kernel immediately, mid-turn, for the
    /// same reason an interrupt does: the command queue is not read while
    /// `run_until_quiescent` is running, and a turn with a tool in it runs for
    /// as long as the tool takes.
    ///
    /// Going through the queue meant a line typed during a turn sat in the
    /// channel until the turn ENDED — measured at 24 seconds behind a
    /// `sleep 25`, arriving 4ms after the turn closed. Everything downstream
    /// followed from that: it was not on the ledger, so nothing could show it
    /// as waiting, and the loop's own mid-turn insertion (which carries a
    /// waiting line on the NEXT question it asks, not at the end of the turn)
    /// never had anything to carry.
    ///
    /// Injecting does not make it a second question: the loop sees a line
    /// arriving mid-turn and holds it in its material until the next question,
    /// which is the earliest moment it could be heard.
    pub fn send_text(&self, text: impl Into<String>) {
        self.send_with_images(text, Vec::new());
    }

    /// Say something with pictures attached.
    ///
    /// `images` are REFERENCES into the documents directory beside this
    /// stream's ledger — already stored by whoever took them from the clipboard
    /// or off the dragged path. The bytes never travel through an event: the
    /// ledger is one JSON object per line, and an inlined picture would put
    /// megabytes of base64 on a line every reader has to walk past.
    pub fn send_with_images(&self, text: impl Into<String>, images: Vec<Value>) {
        self.send_with_origin(text, images, None);
    }

    /// Inject a root input with optional cross-stream provenance.
    pub fn send_with_origin(
        &self,
        text: impl Into<String>,
        images: Vec<Value>,
        origin: Option<crate::contracts::event::StreamRef>,
    ) {
        let mut payload = json!({ "text": text.into(), "interface":self.interface_id });
        if !images.is_empty() {
            payload["images"] = Value::Array(images);
        }
        let mut draft = EventDraft::new(ce::USER_MESSAGE, &[], payload);
        draft.origin = origin;
        self.interrupter.emit("user", draft);
    }

    /// This frontend's live binding, not the UI component instance or stream id.
    pub fn interface_id(&self) -> Option<&str> {
        self.interface_id.as_deref()
    }

    /// Change live permission without waiting behind a running model/tool.
    /// The authoritative state event confirms when the change takes effect.
    pub fn set_permission(&self, enabled: bool) -> Result<(), String> {
        let id = self
            .interface_id
            .as_deref()
            .ok_or("This assembly does not support interface permission")?;
        if self.stopping.load(Ordering::Acquire) {
            return Err("This interface is closing".into());
        }
        self.interrupter.emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({
                    "channel":crate::components::interface_permissions::CHANNEL,
                    "interface":id,"action":"set","enabled":enabled,
                }),
            ),
        );
        Ok(())
    }

    pub fn supports_operation_authorization(&self) -> bool {
        self.operation_authorization
    }

    /// Read the authority's acknowledgement, not a locally predicted toggle.
    pub fn permission_enabled(&self) -> std::io::Result<Option<bool>> {
        let Some(id) = self.interface_id.as_deref() else {
            return Ok(None);
        };
        crate::components::interface_permissions::read_state(
            &self.reader,
            &self.authorization_sources.interfaces,
        )
        .map(|state| state.map(|state| state.permits(id)))
    }

    pub fn flow_grants(&self) -> std::io::Result<crate::components::operation_policy::GrantState> {
        if !self.operation_authorization {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "This assembly does not support operation authorization",
            ));
        }
        crate::components::operation_policy::read_grants(
            &self.reader,
            &self.authorization_sources.operations,
        )
    }

    fn require_operation_authorization(&self) -> Result<(), String> {
        if self.stopping.load(Ordering::Acquire) {
            return Err("This interface is closing".into());
        }
        if !self.operation_authorization {
            return Err("This assembly does not support operation authorization".into());
        }
        Ok(())
    }

    /// Save the service-proposed operation matchers, not a frontend-invented rule.
    pub fn authorize_flow(&self, request: impl Into<String>) -> Result<(), String> {
        self.authorize_operation(request.into(), "flow")
    }

    /// Explicitly approve once without using the legacy permanent-trust choice.
    pub fn authorize_once(&self, request: impl Into<String>) -> Result<(), String> {
        self.authorize_operation(request.into(), "once")
    }

    fn authorize_operation(&self, request: String, scope: &str) -> Result<(), String> {
        self.require_operation_authorization()?;
        self.interrupter.emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({
                    "channel":crate::components::operation_policy::ANSWER_CHANNEL,
                    "interface":self.interface_id,"request":request,"approve":true,"scope":scope,
                }),
            ),
        );
        Ok(())
    }

    pub fn revoke_grant(&self, grant: impl Into<String>) -> Result<(), String> {
        self.require_operation_authorization()?;
        self.interrupter.emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({
                    "channel":crate::components::operation_policy::CHANNEL,
                    "interface":self.interface_id,"action":"revoke","grant":grant.into(),
                }),
            ),
        );
        Ok(())
    }

    /// Answer an authorization request (y/n on the trust gate's card).
    pub fn authorize(&self, request: impl Into<String>, approve: bool) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        self.interrupter.emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({
                    "channel":crate::components::trust_policy::AUTH_CHANNEL,
                    "request":request.into(),"approve":approve,"interface":self.interface_id,
                }),
            ),
        );
    }

    /// Interrupt a running turn — reaches the kernel immediately, mid model
    /// call, because it goes through the injector, not the command queue.
    pub fn interrupt(&self) {
        self.interrupter.emit(
            "interrupt",
            EventDraft::new(ce::INTERRUPTED, &[], json!({ "by": "user" })),
        );
    }

    /// For the embedding host to wire read-only observation, as the daemon
    /// does. This is not a frontend protocol capability or a writable handle.
    pub fn log_reader(&self) -> crate::kernel::log::LogReader {
        self.reader.clone()
    }

    /// Snapshot of the assembly that actually started; later changes arrive
    /// as an `assembly` notice on the render channel.
    pub fn initial_parts(&self) -> &[crate::Assembled] {
        &self.initial_parts
    }

    /// Drain the next render event, if any (non-blocking).
    pub fn poll_render(&self) -> Option<RenderEvent> {
        self.renders.try_recv().ok()
    }

    /// Block for the next render event (used by headless drivers/tests).
    pub fn next_render(&self) -> Option<RenderEvent> {
        self.renders.recv().ok()
    }

    /// Block for a render event with a deadline, without polling or sleeping.
    pub fn next_render_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<RenderEvent, mpsc::RecvTimeoutError> {
        self.renders.recv_timeout(timeout)
    }

    /// Signal every session before joining any of them. Cancellation bypasses
    /// the command queue so an active model/tool cannot postpone the request.
    pub fn request_shutdown(&self) {
        let mut started = self
            .shutdown_started
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !self.stopping.swap(true, Ordering::AcqRel) {
            *started = Some(Instant::now());
            if let Some(id) = &self.interface_id {
                self.interrupter.emit(
                    "answer",
                    EventDraft::new(
                        ce::EXTERNAL_INPUT,
                        &[],
                        json!({
                            "channel":crate::components::interface_permissions::CHANNEL,
                            "interface":id,"action":"close",
                        }),
                    ),
                );
            }
            self.stop.request("frontend is shutting down".into(), None);
            let _ = self.commands.send(FrontendCommand::Shutdown);
        }
    }

    /// Ask the session to stop and wait for the core to seal its ledger.
    pub fn shutdown(self) {
        let _ = self.finish_shutdown();
    }

    /// Transfer the closed kernel's sole ledger writer back to the host, so
    /// final frontend cleanup can be observed without reopening a live stream.
    pub fn finish_shutdown(mut self) -> Result<crate::shutdown::SessionShutdown, String> {
        self.request_shutdown();
        self.handle
            .take()
            .ok_or("session already joined")?
            .join()
            .map_err(|_| "session thread panicked during shutdown".to_string())?
            .ok_or_else(|| "session did not finish startup".to_string())
    }
}

/// A render event carries a user-facing line, if it has one — the frontend's
/// rendering rule in one place (what shows on screen).
pub fn render_line(event: &EventEnvelope) -> Option<(&'static str, String)> {
    match event.event_type.as_str() {
        // A CAUSED user message is a re-emission (the expansion station's
        // forward); only the causeless typed original is shown
        ce::USER_MESSAGE if !event.causes.is_empty() => None,
        ce::USER_MESSAGE => Some(("you", event.payload["text"].as_str()?.to_string())),
        ce::OUTPUT_REPLY => {
            if let Some(text) = event.payload["text"].as_str() {
                Some(("agent", text.to_string()))
            } else if event.payload["cancelled"] == true {
                Some(("agent", "[interrupted]".to_string()))
            } else if let Some(msg) = event.payload["error"]["message"].as_str() {
                Some(("error", msg.to_string()))
            } else {
                None
            }
        }
        ce::TOOL_EXEC_STARTED => Some((
            "tool",
            format!(
                "⚙ {} {}",
                event.payload["tool"].as_str().unwrap_or("?"),
                event.payload["arguments"]
            ),
        )),
        t if t == crate::components::operation_policy::AUTH_REQUESTED
            || t == crate::components::trust_policy::AUTH_REQUESTED
            || t == crate::components::browser_tools::AUTH_REQUESTED
            || t == crate::components::expert_definitions::AUTH_REQUESTED =>
        {
            Some((
                "notice",
                format!(
                    "⚠ authorization required: {} — allow? (y/n)",
                    event.payload["summary"].as_str().unwrap_or("an admission")
                ),
            ))
        }
        t if t == crate::components::browser_tools::DECISION => Some((
            "notice",
            format!("browser: {}", event.reason.as_deref().unwrap_or("")),
        )),
        t if t == crate::components::trust_policy::DECISION => Some((
            "notice",
            format!(
                "{} trust: {}",
                if event.payload["verdict"] == "granted" {
                    "✓"
                } else {
                    "✗"
                },
                event.reason.as_deref().unwrap_or("")
            ),
        )),
        _ => None,
    }
}

/// Hand out any work the turn just asked for, and return.
///
/// One pass, no waiting, no loop: each expert gets a thread of its own and
/// reports by injecting back into this conversation, which fires a wake and
/// drives a turn exactly the way a finished background command does. So this
/// returns at once and the session thread goes back to reading commands —
/// which is what lets a person keep typing, and lets several experts run at
/// the same time.
fn run_experts(
    kernel: &mut Kernel,
    subagents: &mut Option<(
        crate::subagent_host::SubagentHost,
        crate::kernel::stream_host::StreamHost,
        String,
    )>,
) -> Result<(), String> {
    let Some((host_side, experts, main_id)) = subagents.as_mut() else {
        return Ok(());
    };
    let mut view = crate::subagent_host::MainAndChildren {
        main_id,
        main: kernel,
        children: experts,
    };
    host_side.poll(&mut view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, model: &str, url: &str, key: &str) -> crate::models::Entry {
        crate::models::Entry {
            id: id.to_string(),
            adapter: "openai".to_string(),
            model: model.to_string(),
            base_url: url.to_string(),
            key_env: key.to_string(),
            profile: None,
        }
    }

    /// The refusals, in the words a person reads. The key one is the reason
    /// this check exists at all: a swap to a model with no key would leave a
    /// conversation whose every next turn fails, and the failure would arrive
    /// as a rejected model call rather than as an answer to what was asked.
    #[test]
    fn a_swap_is_refused_before_it_can_break_the_conversation() {
        let running = entry("a", "model-a", "https://a.example", "A_KEY");
        let other = entry("b", "model-b", "https://b.example", "B_KEY");

        assert!(
            refuse_swap(&running, &other, true).is_none(),
            "the ordinary case"
        );

        let same = refuse_swap(&running, &running.clone(), true).expect("already there");
        assert!(same.contains("already running"), "{same}");

        let keyless = refuse_swap(&running, &other, false).expect("no key, no swap");
        assert!(
            keyless.contains("B_KEY") && keyless.contains("not swapping"),
            "it names the variable that is missing and says nothing was done: {keyless}"
        );

        // The same endpoint under a different short name is the same model,
        // not a swap — otherwise renaming a catalog entry would look like a
        // change of model
        let renamed = entry("nickname", "model-a", "https://a.example", "A_KEY");
        assert!(
            refuse_swap(&running, &renamed, true).is_some(),
            "two names for one endpoint are one model"
        );
    }
}
