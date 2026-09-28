//! The daemon: a StreamHost that keeps running when every terminal is closed,
//! plus a Unix-socket server that speaks the wire protocol to many clients at
//! once. Broadcasting a stream's events to every attached client is what lets
//! a terminal and a phone watch the same conversation.
//!
//! Threading: one core thread instantiates streams (component factories are
//! not Send, so construction stays here), then hands each stream's kernel to
//! its own DRIVER thread — that stream's subscriptions, replays and turns all
//! queue behind each other in arrival order, but never behind another
//! stream's (streams are independent; one long turn must not freeze the
//! rest). Each client connection gets a reader thread (socket → core) and a
//! writer thread (outbox → socket). Interrupts bypass every queue — a reader
//! fires the stream's injector directly, so it reaches a running turn
//! immediately (the same trick as the in-process Session).

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::json;

use super::history::{self, Projection};
use crate::contracts::core_events as ce;
use crate::contracts::event::EventDraft;
use crate::daemon::protocol::{AttachmentHistory, ClientMessage, HistoryCursor, ServerMessage};
use crate::kernel::host::{Injector, Kernel};
use crate::kernel::log::LogReader;
use crate::kernel::stream_host::StreamHost;

/// The default template used when `Attach` names no template.
pub const DEFAULT_TEMPLATE: &str = "chat";

type Outbox = mpsc::Sender<ServerMessage>;

/// The clients watching one stream, tagged with connection ids so a repeated
/// attach replaces a subscription instead of doubling it
type Watchers = Arc<Mutex<Vec<(u64, Outbox)>>>;

/// What the core loop (and the wake forwarder) send a stream's driver thread.
enum DriverCmd {
    /// Wind this stream down: leave the loop and shut the kernel.
    ///
    /// Said explicitly rather than by closing the channel, because the
    /// channel does not close — the wake-forwarding thread holds a sender of
    /// its own and sits blocked on its own receiver, so dropping the core's
    /// handle leaves the driver waiting for a command that can still arrive.
    Stop,
    Subscribe {
        client_id: u64,
        outbox: Outbox,
        /// The client's self-declared capabilities from the handshake;
        /// mismatches against what the assembly expects come back as
        /// warnings on `Attached`.
        capabilities: Vec<String>,
    },
    Unsubscribe {
        client_id: u64,
    },
    History {
        client_id: u64,
        cursor: HistoryCursor,
        outbox: Outbox,
    },
    /// Inject a user message. Does NOT run the turn itself — the injection
    /// fires a wake, which arrives as `Wake` and runs it. One path for every
    /// injection (user text, interrupts, and future background/timer sources).
    UserText {
        text: String,
    },
    /// Inject the user's answer to an authorization request (external input
    /// on the trust gate's channel). Same wake path as user text.
    Authorize {
        request: String,
        approve: bool,
    },
    /// Run the stream to quiescence and broadcast it. Sent by the wake
    /// forwarder whenever anything is injected into this stream — this is the
    /// push loop: any injection ⇒ a turn.
    Wake,
}

/// The core loop's handle on one stream: its driver's queue, plus a ledger
/// read handle kept for deriving sidechannels after the kernel has moved
/// onto its driver thread.
struct StreamDriver {
    cmd: mpsc::Sender<DriverCmd>,
    reader: LogReader,
    /// Kept so a stopping daemon can WAIT for this stream to wind down.
    /// Without it the process simply ended and every stream's kernel was
    /// still holding its in-flight work — no stop protocol, no sealing.
    thread: Option<JoinHandle<()>>,
}

/// A command that reached the core thread, tagged with the client's identity
/// and outbox so the core can register it for broadcasts (at most once per
/// client per stream) and reply to it directly.
struct CoreCommand {
    client_id: u64,
    message: ClientMessage,
    outbox: Outbox,
}

/// What reaches the core thread: a client's command, or the daemon itself
/// saying to wind down. Both travel the same channel so the core can be
/// stopped while it is blocked waiting for the next command.
enum CoreMessage {
    Client(Box<CoreCommand>),
    Shutdown,
}

/// A running daemon. Drop or `stop()` to shut it down.
pub struct Daemon {
    socket_path: PathBuf,
    /// Identity of the socket inode this instance bound. Shutdown only unlinks
    /// that inode; a later daemon may already own the pathname.
    socket_identity: (u64, u64),
    shutdown: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    core: Option<JoinHandle<()>>,
    /// The daemon's own way to reach the core thread, so stopping does not
    /// depend on every client having disconnected first.
    to_core: mpsc::Sender<CoreMessage>,
}

impl Daemon {
    /// Bind the socket and start serving. `build` constructs the StreamHost ON
    /// the core thread (so its non-Send factories never cross a boundary).
    pub fn serve(
        socket_path: impl AsRef<Path>,
        build: impl FnOnce() -> StreamHost + Send + 'static,
    ) -> std::io::Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();
        prepare_socket_path(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        let metadata = std::fs::symlink_metadata(&socket_path)?;
        let socket_identity = (metadata.dev(), metadata.ino());
        listener.set_nonblocking(true)?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let interrupters: Arc<Mutex<HashMap<String, Injector>>> = Arc::default();
        let (cmd_tx, cmd_rx) = mpsc::channel::<CoreMessage>();
        let to_core = cmd_tx.clone();

        // The core thread owns the StreamHost and processes commands serially.
        let core_interrupters = Arc::clone(&interrupters);
        let core = std::thread::spawn(move || core_loop(build(), cmd_rx, core_interrupters));

        // The accept thread hands each connection its own reader + writer.
        let accept_shutdown = Arc::clone(&shutdown);
        let accept = std::thread::spawn(move || {
            let next_client_id = AtomicU64::new(1);
            for stream in listener.incoming() {
                if accept_shutdown.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let client_id = next_client_id.fetch_add(1, Ordering::Relaxed);
                        serve_client(client_id, stream, cmd_tx.clone(), Arc::clone(&interrupters));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            socket_path,
            socket_identity,
            shutdown,
            accept: Some(accept),
            core: Some(core),
            to_core,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Stop accepting, wind every stream down, remove the socket file.
    ///
    /// The winding down is the point. This used to detach and let the process
    /// exit take the streams with it: no stop protocol, no in-flight work
    /// allowed to finish, nothing sealed — the ledgers survived only because
    /// every event is fsynced as it is written. Telling the core directly
    /// (rather than waiting for every client to disconnect) is what makes the
    /// stop reachable at all while somebody is still attached.
    pub fn stop(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let _ = self.to_core.send(CoreMessage::Shutdown);
        if let Some(core) = self.core.take() {
            let _ = core.join();
        }
        remove_owned_socket(&self.socket_path, self.socket_identity);
    }
}

/// Refuse anything except a stale socket. Only connection refusal is evidence
/// of a stale listener; permission and resource errors must preserve the path.
fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("refusing to replace non-socket path {}", path.display()),
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("socket {} already has a listener", path.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            let current = std::fs::symlink_metadata(path)?;
            if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "socket path changed during startup",
                ));
            }
            std::fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

fn remove_owned_socket(path: &Path, identity: (u64, u64)) {
    let owns_path = std::fs::symlink_metadata(path)
        .map(|metadata| {
            metadata.file_type().is_socket() && (metadata.dev(), metadata.ino()) == identity
        })
        .unwrap_or(false);
    if owns_path {
        let _ = std::fs::remove_file(path);
    }
}

/// Per-connection plumbing: a writer thread drains the outbox to the socket,
/// a reader thread parses the socket into core commands (interrupts shortcut
/// straight to the injector).
fn serve_client(
    client_id: u64,
    stream: UnixStream,
    cmd_tx: mpsc::Sender<CoreMessage>,
    interrupters: Arc<Mutex<HashMap<String, Injector>>>,
) {
    // The accepted socket may inherit the listener's non-blocking mode; the
    // per-connection reader/writer want blocking I/O
    let _ = stream.set_nonblocking(false);
    let (out_tx, out_rx) = mpsc::channel::<ServerMessage>();

    let mut write_half = match stream.try_clone() {
        Ok(half) => half,
        Err(_) => return,
    };
    std::thread::spawn(move || {
        while let Ok(message) = out_rx.recv() {
            let Ok(line) = serde_json::to_string(&message) else {
                continue;
            };
            if writeln!(write_half, "{line}").is_err() {
                break;
            }
        }
    });

    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(message) = serde_json::from_str::<ClientMessage>(&line) else {
                let _ = out_tx.send(ServerMessage::Error {
                    stream: None,
                    message: "unparseable client message".to_string(),
                });
                continue;
            };
            // Interrupt must reach a running turn now, not queue behind it:
            // fire the injector here (it cancels in-flight work immediately),
            // then STILL forward the command — the driver schedules a drain
            // run so the interrupted event is recorded even on an idle
            // stream (an injected event must never wait for the next turn
            // to enter the ledger)
            if let ClientMessage::Interrupt { stream } = &message {
                if let Some(injector) = interrupters.lock().unwrap().get(stream) {
                    injector.emit(
                        "interrupt",
                        EventDraft::new(ce::INTERRUPTED, &[], json!({"by": "user"})),
                    );
                }
            }
            if cmd_tx
                .send(CoreMessage::Client(Box::new(CoreCommand {
                    client_id,
                    message,
                    outbox: out_tx.clone(),
                })))
                .is_err()
            {
                break;
            }
        }
    });
}

/// The core loop: instantiates streams on demand and routes each command to
/// the right stream's driver. It never runs a turn itself, so no stream can
/// block another stream's attach, text or replay.
fn core_loop(
    mut host: StreamHost,
    cmd_rx: mpsc::Receiver<CoreMessage>,
    interrupters: Arc<Mutex<HashMap<String, Injector>>>,
) {
    let mut drivers: HashMap<String, StreamDriver> = HashMap::new();

    while let Ok(incoming) = cmd_rx.recv() {
        let CoreCommand {
            client_id,
            message,
            outbox,
        } = match incoming {
            CoreMessage::Client(command) => *command,
            // Wind every stream down before the process goes: drop the
            // command channels so each driver leaves its loop and calls
            // `Kernel::shutdown`, then WAIT for them. Stopping used to be a
            // detach — the ledger survived only because every event is
            // fsynced as it is written, and nothing else about the stop
            // protocol happened at all.
            CoreMessage::Shutdown => break,
        };
        match message {
            ClientMessage::Attach {
                stream,
                template,
                derive_from,
                capabilities,
            } => {
                let template = template.unwrap_or_else(|| DEFAULT_TEMPLATE.to_string());
                if !drivers.contains_key(&stream) {
                    match open_driver(
                        &mut host,
                        &stream,
                        &template,
                        derive_from.as_deref(),
                        &drivers,
                        &interrupters,
                    ) {
                        Ok(driver) => {
                            drivers.insert(stream.clone(), driver);
                        }
                        Err(message) => {
                            let _ = outbox.send(ServerMessage::Error {
                                stream: Some(stream),
                                message,
                            });
                            continue;
                        }
                    }
                }
                let _ = drivers[&stream].cmd.send(DriverCmd::Subscribe {
                    client_id,
                    outbox,
                    capabilities,
                });
            }
            ClientMessage::History { stream, cursor } => match drivers.get(&stream) {
                Some(driver) => {
                    let _ = driver.cmd.send(DriverCmd::History {
                        client_id,
                        cursor,
                        outbox,
                    });
                }
                None => {
                    let _ = outbox.send(ServerMessage::HistoryError {
                        stream,
                        cursor,
                        message: "unknown stream - attach first".into(),
                    });
                }
            },
            ClientMessage::SendText { stream, text } => match drivers.get(&stream) {
                Some(driver) => {
                    let _ = driver.cmd.send(DriverCmd::UserText { text });
                }
                None => {
                    let _ = outbox.send(ServerMessage::Error {
                        stream: Some(stream),
                        message: "unknown stream - attach first".to_string(),
                    });
                }
            },
            ClientMessage::Authorize {
                stream,
                request,
                approve,
            } => match drivers.get(&stream) {
                Some(driver) => {
                    let _ = driver.cmd.send(DriverCmd::Authorize { request, approve });
                }
                None => {
                    let _ = outbox.send(ServerMessage::Error {
                        stream: Some(stream),
                        message: "unknown stream - attach first".to_string(),
                    });
                }
            },
            ClientMessage::Detach { stream } => {
                if let Some(driver) = drivers.get(&stream) {
                    let _ = driver.cmd.send(DriverCmd::Unsubscribe { client_id });
                }
            }
            ClientMessage::Interrupt { .. } => {
                // Nothing to do here: the reader thread already fired the
                // interrupt through the injector, which cancels a running
                // turn immediately AND fires a wake — so the interrupted
                // event gets recorded and broadcast on the next turn even
                // when the stream was idle. One injection path, no drain.
            }
        }
    }
    // Dropping a driver's command sender is what tells its thread to leave
    // its loop and shut its kernel down; joining is what makes "stopped"
    // mean it has finished rather than that it was told to.
    for (_, mut driver) in drivers.drain() {
        let thread = driver.thread.take();
        let _ = driver.cmd.send(DriverCmd::Stop);
        drop(driver);
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

/// Open a stream and put its kernel on a fresh driver thread. Returns the
/// core loop's handle on it, or a message saying why it could not open.
fn open_driver(
    host: &mut StreamHost,
    stream: &str,
    template: &str,
    derive_from: Option<&str>,
    drivers: &HashMap<String, StreamDriver>,
    interrupters: &Arc<Mutex<HashMap<String, Injector>>>,
) -> Result<StreamDriver, String> {
    // Wire this stream's ledger + notice bypass into its watcher list
    let watchers: Watchers = Arc::default();
    let subs_log = Arc::clone(&watchers);
    let log_stream = stream.to_string();
    let subs_notice = Arc::clone(&watchers);
    let notice_stream = stream.to_string();
    let configure = move |kernel: &mut Kernel| {
        kernel.subscribe_log(move |event| {
            broadcast(
                &subs_log,
                ServerMessage::Appended {
                    stream: log_stream.clone(),
                    event: Box::new(event.clone()),
                },
            );
        });
        kernel.set_notice_handler(move |source, payload| {
            broadcast(
                &subs_notice,
                ServerMessage::Notice {
                    stream: notice_stream.clone(),
                    source: source.to_string(),
                    payload: payload.clone(),
                },
            );
        });
    };

    // A sidechannel (/btw) observes its parent read-only; the parent kernel
    // lives on its driver thread, so its retained reader is the handle
    // Where in the parent this sidechannel came from, remembered before the
    // stream opens so its very first event can point back at it.
    let mut provenance: Option<(String, String)> = None;
    let opened = match derive_from {
        Some(parent) => {
            let parent_reader = drivers
                .get(parent)
                .map(|d| d.reader.clone())
                .ok_or_else(|| format!("derives from unknown parent stream: {parent}"))?;
            provenance = parent_reader.latest_id().map(|id| (parent.to_string(), id));
            host.open_observing(
                stream,
                template,
                [(parent.to_string(), parent_reader)].into(),
                configure,
            )
        }
        None => host.open_with(stream, template, configure),
    };
    opened.map_err(|e| e.to_string())?;

    let mut kernel = host
        .take(stream)
        .ok_or_else(|| "stream vanished after open".to_string())?;
    let projection_reader = kernel.log().reader();
    let projection = Arc::new(Mutex::new(
        Projection::recover(&projection_reader).map_err(|error| error.to_string())?,
    ));
    let observed = Arc::clone(&projection);
    kernel.subscribe_log(move |event| {
        let mut state = observed.lock().unwrap();
        if let Err(error) = state.observe(event) {
            eprintln!("cannot advance daemon current state: {error}");
        } else if state.through.is_multiple_of(256) {
            state.save(&projection_reader);
        }
    });
    interrupters
        .lock()
        .unwrap()
        .insert(stream.to_string(), kernel.injector("ui"));
    let injector = kernel.injector("ui");
    let reader = kernel.log().reader();
    let (cmd_tx, cmd_rx) = mpsc::channel::<DriverCmd>();

    // The push loop: every injection into this stream fires a wake; forward
    // each batch as one `Wake` so the driver runs a turn. Coalescing keeps a
    // burst of injections from queueing a run apiece (they all drain in one).
    let wake_rx = kernel
        .take_wake_receiver()
        .expect("a fresh kernel offers its wake receiver");
    let wake_forward = cmd_tx.clone();
    std::thread::spawn(move || {
        while wake_rx.recv().is_ok() {
            while wake_rx.try_recv().is_ok() {}
            if wake_forward.send(DriverCmd::Wake).is_err() {
                break;
            }
        }
    });

    let stream_id = stream.to_string();
    let thread = std::thread::spawn(move || {
        drive_stream(
            stream_id, kernel, injector, watchers, cmd_rx, provenance, projection,
        )
    });
    Ok(StreamDriver {
        cmd: cmd_tx,
        reader,
        thread: Some(thread),
    })
}

/// One stream's driver: owns the kernel; serves subscriptions and replays,
/// runs turns. Everything for this stream happens here in arrival order (so
/// a replay can never race a broadcast), while other streams\' drivers run in
/// parallel. Exits — shutting the kernel down — when the daemon drops the
/// command sender.
fn drive_stream(
    stream: String,
    mut kernel: Kernel,
    injector: Injector,
    watchers: Watchers,
    commands: mpsc::Receiver<DriverCmd>,
    // Which event in which parent stream this sidechannel came out of, for
    // the ONE event that can carry it: the first thing said here.
    mut provenance: Option<(String, String)>,
    projection: Arc<Mutex<Projection>>,
) {
    // The daemon is a host: it owns a kernel, so it is the one that can rewire
    // one. Same locations the terminal UI installs into, so a tool installed
    // from either is there for both.
    let workshop = crate::workshop::Workshop::standard();
    let mut generation = 0u64;
    let mut cursors: HashMap<u64, HistoryCursor> = HashMap::new();
    while let Ok(command) = commands.recv() {
        match command {
            DriverCmd::Subscribe {
                client_id,
                outbox,
                capabilities,
            } => {
                let reader = kernel.log().reader();
                let through = reader.snapshot_end();
                let prepared = (|| -> std::io::Result<_> {
                    let before = through
                        .checked_add(1)
                        .ok_or_else(|| std::io::Error::other("history boundary overflow"))?;
                    let page =
                        reader.page_before(before, history::PAGE_EVENTS, history::PAGE_BYTES)?;
                    let state = projection.lock().unwrap();
                    if state.through != through {
                        return Err(std::io::Error::other(
                            "daemon current state is not at the attachment boundary",
                        ));
                    }
                    state.save(&reader);
                    Ok((page, state.state.clone()))
                })();
                let (page, state) = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let _ = outbox.send(ServerMessage::Error {
                            stream: Some(stream.clone()),
                            message: format!("cannot read attach history: {error}"),
                        });
                        continue;
                    }
                };
                let Some(next_generation) = generation.checked_add(1) else {
                    let _ = outbox.send(ServerMessage::Error {
                        stream: Some(stream.clone()),
                        message: "attachment generation exhausted".into(),
                    });
                    continue;
                };
                generation = next_generation;
                let older = page
                    .first()
                    .filter(|event| event.seq > 1)
                    .map(|event| HistoryCursor {
                        stream: stream.clone(),
                        generation,
                        through,
                        before: event.seq,
                    });
                let paged = capabilities
                    .iter()
                    .any(|capability| capability == history::CAPABILITY);
                if older.is_some() && !paged {
                    let _ = outbox.send(ServerMessage::Error {
                        stream: Some(stream.clone()),
                        message:
                            "history exceeds one page; update the client to support history-pages"
                                .into(),
                    });
                    continue;
                }
                cursors.remove(&client_id);
                if let Some(cursor) = &older {
                    cursors.insert(client_id, cursor.clone());
                }
                let _ = outbox.send(ServerMessage::Attached {
                    stream: stream.clone(),
                    replay: page.iter().map(|event| event.as_ref().clone()).collect(),
                    warnings: capability_warnings(&kernel, &capabilities),
                    history: paged.then_some(AttachmentHistory {
                        through,
                        older,
                        state,
                    }),
                });
                let mut w = watchers.lock().unwrap();
                w.retain(|(id, _)| *id != client_id);
                w.push((client_id, outbox.clone()));
            }
            DriverCmd::Unsubscribe { client_id } => {
                watchers.lock().unwrap().retain(|(id, _)| *id != client_id);
                cursors.remove(&client_id);
            }
            DriverCmd::History {
                client_id,
                cursor,
                outbox,
            } => {
                if cursors.get(&client_id) != Some(&cursor) {
                    let _ = outbox.send(ServerMessage::HistoryError {
                        stream: stream.clone(),
                        cursor,
                        message: "stale or unattached history cursor".into(),
                    });
                    continue;
                }
                match kernel.log().reader().page_before(
                    cursor.before,
                    history::PAGE_EVENTS,
                    history::PAGE_BYTES,
                ) {
                    Ok(page) => {
                        let older =
                            page.first()
                                .filter(|event| event.seq > 1)
                                .map(|event| HistoryCursor {
                                    before: event.seq,
                                    ..cursor.clone()
                                });
                        cursors.remove(&client_id);
                        if let Some(next) = &older {
                            cursors.insert(client_id, next.clone());
                        }
                        let _ = outbox.send(ServerMessage::HistoryPage {
                            stream: stream.clone(),
                            cursor,
                            replay: page.iter().map(|event| event.as_ref().clone()).collect(),
                            older,
                        });
                    }
                    Err(error) => {
                        let _ = outbox.send(ServerMessage::HistoryError {
                            stream: stream.clone(),
                            cursor,
                            message: format!("cannot read history page: {error}"),
                        });
                    }
                }
            }
            DriverCmd::UserText { text } => {
                // Only inject; the injection fires a wake that arrives as
                // `Wake` and runs the turn — same path as every other source.
                //
                // The FIRST thing said in a derived stream carries `origin`:
                // it is the root event, and origin is meaningful on root
                // events. That link is the only thing tying a sidechannel to
                // the conversation it grew out of — the kernel deliberately
                // does not check it, so where nobody writes it there is
                // simply no link, which is what this path had. A `/btw`
                // stream recorded nothing about which conversation it was an
                // aside to.
                let draft = match provenance.take() {
                    Some((parent, event)) => StreamHost::derived_root(
                        ce::USER_MESSAGE,
                        &parent,
                        &event,
                        json!({"text": text}),
                    ),
                    None => EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": text})),
                };
                injector.emit("user", draft);
            }
            DriverCmd::Authorize { request, approve } => {
                injector.emit(
                    "answer",
                    EventDraft::new(
                        ce::EXTERNAL_INPUT,
                        &[],
                        json!({
                            "channel": crate::components::trust_policy::AUTH_CHANNEL,
                            "request": request,
                            "approve": approve,
                        }),
                    ),
                );
            }
            DriverCmd::Stop => break,
            DriverCmd::Wake => {
                // Same moment, same reason as the session behind the terminal
                // UI: an install rewires the kernel, so only its host can do it
                if let Err(err) = workshop.run(&mut kernel) {
                    broadcast(
                        &watchers,
                        ServerMessage::Error {
                            stream: Some(stream.clone()),
                            message: err.to_string(),
                        },
                    );
                }
                broadcast(
                    &watchers,
                    ServerMessage::Quiescent {
                        stream: stream.clone(),
                    },
                );
            }
        }
    }
    let _ = kernel.shutdown();
}

/// The handshake's capability check: what this assembly may expect of a
/// client, against what the client declared. A mismatch never refuses the
/// attach (observers are legitimate) — it is SAID, so nobody discovers a
/// hanging authorization card by silence. The stance is read off the
/// assembly manifest; the daemon serves the preset, so knowing the trust
/// component's name here is the assembler's knowledge, not the kernel's.
fn capability_warnings(kernel: &Kernel, capabilities: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();
    let asks = kernel.assembly().instances.values().any(|inst| {
        inst.component == crate::components::trust_policy::NAME
            && inst.config.as_ref().and_then(|c| c["stance"].as_str()) == Some("ask")
    });
    if asks && !capabilities.iter().any(|c| c == "authorize") {
        warnings.push(
            "this stream can ask for authorization (trust stance \"ask\"), but this client \
             declared no \"authorize\" capability — approvals will wait until a client that \
             can answer attaches"
                .to_string(),
        );
    }
    warnings
}

/// Fan one message out to every watcher, pruning any whose socket has closed.
fn broadcast(watchers: &Watchers, message: ServerMessage) {
    watchers
        .lock()
        .unwrap()
        .retain(|(_, client)| client.send(message.clone()).is_ok());
}
