//! Process-form hosting and the child side of the v1 line protocol.

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{Component, Ctx, Delivery, Message};
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::log::EventLog;

#[cfg(all(test, unix))]
#[path = "host_bridge_tests.rs"]
mod tests;

/// Kill a child's whole process GROUP, so whatever it spawned goes with
/// it — a bridge child is given its own group at birth for exactly this.
#[cfg(unix)]
pub(super) fn kill_group(pid: i32) {
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub(super) fn kill_group(_pid: i32) {}

/// Finish signals while holding the registration lock. A bridge takes this
/// same lock before reaping, so a captured process id never escapes the lock
/// and becomes a delayed signal to a process whose id might have been reused.
pub(super) fn signal_registered_groups(
    groups: &Mutex<HashMap<String, i32>>,
    instance: Option<&str>,
    mut signal: impl FnMut(i32),
) {
    let mut registered = groups.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(instance) = instance {
        if let Some(pid) = registered.remove(instance) {
            signal(pid);
        }
    } else {
        for (_, pid) in registered.drain() {
            signal(pid);
        }
    }
}

/// The cross-process bridge — the process-hosted half of duty #4.
///
/// The child speaks the v1 wire contract (docs/contracts/06): one JSON per
/// line on stdin/stdout. Kernel → child: `hello` (instance, stream, config,
/// capabilities), `deliver` (port + full envelope), `cancel`, `stop`.
/// Everything a process-form component needs to be started: who it is, what
/// to run, and the two facts about how this host runs children.
#[cfg(unix)]
pub(super) struct BridgeSeat {
    pub(super) instance: String,
    pub(super) entry: String,
    pub(super) config: Option<Value>,
    pub(super) stream: String,
    /// Start the process on the first delivery rather than at build
    pub(super) lazy: bool,
    /// Environment variables this child must not inherit
    pub(super) env_deny: Vec<String>,
}

/// Child → kernel: `emit` (a draft), `notice`, `processed` (one per
/// delivery). The bridge thread plays the same role as an in-process
/// component thread; the stdout reader forwards spontaneous emissions any
/// time, so edge adapters can speak unprompted. A vanished child becomes a
/// recorded crash; a stopped one gets the stop line, then the process group
/// gets SIGTERM and, after a beat, SIGKILL.
pub(super) fn spawn_process_bridge(
    seat: BridgeSeat,
    central: mpsc::Sender<Message>,
    wake: mpsc::Sender<()>,
    groups: Arc<Mutex<HashMap<String, i32>>>,
) -> std::io::Result<(mpsc::Sender<Delivery>, JoinHandle<()>)> {
    use std::io::Write;
    let BridgeSeat {
        instance,
        entry,
        config,
        stream,
        lazy,
        env_deny,
    } = seat;
    let (mail_tx, mail_rx) = mpsc::channel::<Delivery>();

    // The child is either started now, or on the first delivery. Deferring it
    // separates a component's DECLARATION from its IMPLEMENTATION: the tools,
    // the ports and the prompt fragment come from the manifest and are on the
    // books from the first turn, while the process — and whatever it loads
    // when it starts — waits until something actually asks for it. An assembly
    // can then carry a library it rarely uses without paying for it at every
    // start. Nothing the model sees changes either way, which is the point:
    // the tool list stays byte-stable, so the prompt cache is untouched.
    let mut live = if lazy {
        None
    } else {
        // Eager: a child that cannot start should fail the build, loudly, here
        let started = materialise(
            &instance, &entry, &config, &stream, &central, &wake, &env_deny,
        )?;
        groups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(instance.clone(), started.pid);
        Some(started)
    };

    let bridge = std::thread::spawn(move || {
        while let Ok((port, event, token)) = mail_rx.recv() {
            if live.is_none() {
                match materialise(
                    &instance, &entry, &config, &stream, &central, &wake, &env_deny,
                ) {
                    Ok(started) => {
                        groups
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(instance.clone(), started.pid);
                        live = Some(started);
                    }
                    Err(_) => {
                        // A lazy child that will not start is a crash at the
                        // moment of asking, which is where the call that asked
                        // gets settled — never a fabricated failure result.
                        let _ = central.send(Message::Crashed {
                            instance: instance.clone(),
                            event_id: event.id.clone(),
                        });
                        return;
                    }
                }
            }
            let child = live.as_mut().expect("materialised above");
            let deliver = json!({"deliver": {"port": port, "event": event}});
            if writeln!(child.stdin, "{deliver}").is_err() {
                let _ = central.send(Message::Crashed {
                    instance: instance.clone(),
                    event_id: event.id.clone(),
                });
                if let Some(child) = live {
                    child.reap(&instance, &groups);
                }
                return;
            }
            let mut cancel_sent = false;
            loop {
                if token.is_cancelled() && !cancel_sent {
                    let _ = writeln!(child.stdin, "{}", json!({"cancel": {}}));
                    cancel_sent = true;
                }
                // The kernel kills a child it has declared unresponsive, and
                // killing closes the pipe — which is what ends this wait. It
                // had no other exit: a child that ignored `cancel` was
                // forgotten by the kernel while this thread polled for an
                // acknowledgement that was never coming, and the process and
                // its whole group ran on.
                match child.processed.recv_timeout(Duration::from_millis(25)) {
                    Ok(()) => {
                        let _ = central.send(Message::Processed {
                            instance: instance.clone(),
                            event: event.id.clone(),
                        });
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = central.send(Message::Crashed {
                            instance: instance.clone(),
                            event_id: event.id.clone(),
                        });
                        if let Some(child) = live {
                            child.reap(&instance, &groups);
                        }
                        return;
                    }
                }
            }
        }
        // Mailbox closed: protocol first, signals second. A lazy child that was
        // never asked for anything has nothing to stop.
        if let Some(child) = live {
            child.stop(&instance, &groups);
        }
    });

    Ok((mail_tx, bridge))
}

/// A running bridge child: the pipe in, the `processed` acknowledgements out,
/// and what is needed to end it.
#[cfg(unix)]
struct BridgeChild {
    stdin: std::process::ChildStdin,
    processed: mpsc::Receiver<()>,
    reader: JoinHandle<()>,
    child: std::process::Child,
    pid: i32,
}

#[cfg(unix)]
impl BridgeChild {
    /// A closed pipe is not proof of process exit. Terminate our still-owned
    /// group, retire its registration, then reap and finish the reader.
    fn reap(mut self, instance: &str, groups: &Mutex<HashMap<String, i32>>) {
        {
            let mut registered = groups.lock().unwrap_or_else(|e| e.into_inner());
            kill_group(self.pid);
            // A replacement can already own this name. Never remove its seat.
            if registered.get(instance) == Some(&self.pid) {
                registered.remove(instance);
            }
        }
        // No kernel signal can now be waiting outside the registration lock
        // with our pid. Waiting must not hold the shared lock.
        let _ = self.child.wait();
        let _ = self.reader.join();
    }

    /// Stop protocol first, signals second, with the existing grace periods.
    fn stop(mut self, instance: &str, groups: &Mutex<HashMap<String, i32>>) {
        use std::io::Write;
        let _ = writeln!(self.stdin, "{}", json!({"stop": {}}));
        std::thread::sleep(Duration::from_millis(300));
        unsafe {
            libc::kill(-self.pid, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(300));
        self.reap(instance, groups);
    }
}

/// A failed handshake has no registered bridge to clean it up later.
#[cfg(unix)]
fn send_bridge_hello(
    child: &mut std::process::Child,
    stdin: &mut std::process::ChildStdin,
    hello: &Value,
) -> std::io::Result<()> {
    use std::io::Write;
    if let Err(error) = writeln!(stdin, "{hello}") {
        kill_group(child.id() as i32);
        let _ = child.wait();
        return Err(error);
    }
    Ok(())
}

/// Start the child and its reader, and say hello. Shared by both the eager
/// path (at build) and the lazy one (at first delivery).
#[cfg(unix)]
fn materialise(
    instance: &str,
    entry: &str,
    config: &Option<Value>,
    stream: &str,
    central: &mpsc::Sender<Message>,
    wake: &mpsc::Sender<()>,
    env_deny: &[String],
) -> std::io::Result<BridgeChild> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut parts = entry.split_whitespace();
    let program = parts.next().unwrap_or_default().to_string();
    let mut command = Command::new(program);
    command
        .args(parts)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .process_group(0); // its own group: grandchildren die with it
    for name in env_deny {
        command.env_remove(name);
    }
    let mut child = command.spawn()?;
    let pid = child.id() as i32;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");

    let hello = json!({"hello": {
        "v": 1,
        "instance": instance,
        "stream": stream,
        "config": config,
    }});
    send_bridge_hello(&mut child, &mut stdin, &hello)?;

    // The reader forwards child lines as they come: spontaneous emissions
    // flow any time (edge adapters), `processed` markers go to the bridge
    let (processed_tx, processed) = mpsc::channel::<()>();
    let reader_instance = instance.to_string();
    let reader_central = central.clone();
    let reader_wake = wake.clone();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue; // unknown lines are tolerated for forward compat
            };
            if msg.get("processed").is_some() {
                let _ = processed_tx.send(());
            } else if let Some(emit) = msg.get("emit") {
                let draft = EventDraft {
                    event_type: emit["type"].as_str().unwrap_or_default().to_string(),
                    causes: emit["causes"]
                        .as_array()
                        .map(|causes| {
                            causes
                                .iter()
                                .filter_map(|c| c.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    origin: serde_json::from_value(emit["origin"].clone()).ok(),
                    payload: emit["payload"].clone(),
                    reason: emit["reason"].as_str().map(str::to_string),
                };
                let _ = reader_central.send(Message::Emission {
                    source: reader_instance.clone(),
                    port: emit["port"].as_str().unwrap_or_default().to_string(),
                    draft,
                });
                // Like Injector::emit: enqueue before waking a host that may
                // be waiting outside run_until_quiescent. Speech is not
                // restricted to an outstanding delivery in process form.
                let _ = reader_wake.send(());
            } else if let Some(payload) = msg.get("notice") {
                let _ = reader_central.send(Message::Notice {
                    source: reader_instance.clone(),
                    payload: payload.clone(),
                });
            }
        }
        // stdout closed, not necessarily process exit. The disconnected
        // `processed` channel makes the bridge terminate and reap the child
        // when a delivery is awaiting its acknowledgement.
    });

    Ok(BridgeChild {
        stdin,
        processed,
        reader,
        child,
        pid,
    })
}

#[cfg(not(unix))]
pub(super) fn spawn_process_bridge(
    _instance: String,
    _entry: String,
    _config: Option<Value>,
    _stream: String,
    _central: mpsc::Sender<Message>,
    _lazy: bool,
) -> std::io::Result<(mpsc::Sender<Delivery>, JoinHandle<()>)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process components require unix in v1",
    ))
}

/// Run a Component as a BRIDGE CHILD — the child's half of duty #4, in Rust.
///
/// This is what lets a BUILTIN component run as a separate process: the
/// product binary invokes it (`lattice component <name>`), it speaks the v1
/// wire contract on stdin/stdout (hello → deliver/cancel/stop in; emit/
/// notice/processed out), and the component code is exactly the in-process
/// one — same contract, different form, as promised.
///
/// Honest degradations of the child form, by construction: the ledger view
/// is EMPTY (`ctx.log()` sees nothing — read-from-history conveniences like
/// a skill library's duplicate-load note quietly do less), prompt-fragment
/// updates via `ctx.set_prompt` are discarded (the installer carries the
/// listing as instance config instead), and foreign streams are absent.
/// Background wake sources work: `ctx.injector()` emissions become
/// spontaneous `emit` lines — the child may speak any time.
pub fn run_bridge_child(
    factory: impl FnOnce(Option<&Value>) -> Box<dyn Component>,
) -> std::io::Result<()> {
    use std::io::{BufRead, Write};

    // One shared mouth: delivery answers and spontaneous speech interleave
    // line-atomically
    let stdout: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
    let say = |value: Value| {
        let mut out = stdout.lock().unwrap();
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    };
    let emit_line = |port: &str, draft: EventDraft| {
        json!({"emit": {
            "port": port,
            "type": draft.event_type,
            "causes": draft.causes,
            "origin": draft.origin,
            "payload": draft.payload,
            "reason": draft.reason,
        }})
    };

    // The token of whatever delivery is being handled right now, shared with
    // the stdin thread — which is the whole reason stdin has a thread.
    let current: Arc<Mutex<CancellationToken>> = Arc::new(Mutex::new(CancellationToken::new()));

    // stdin on its own thread, and `cancel` is ACTED ON THERE. Merely
    // forwarding it left the cancellation queued behind the delivery it was
    // meant to interrupt: the main loop is the only consumer and it was busy
    // inside `handle`, so the line was read when the work it should have
    // stopped had already finished, and by then the token it cancelled
    // belonged to nothing. The bridge's watchman deadline, and a person's
    // interrupt, both went nowhere.
    let (msg_tx, msg_rx) = mpsc::channel::<Value>();
    let cancelling = Arc::clone(&current);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue; // tolerate unknown lines (forward compat)
            };
            if msg.get("cancel").is_some() {
                cancelling
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .cancel();
                continue;
            }
            if msg_tx.send(msg).is_err() {
                break;
            }
        }
    });

    // Await the handshake; unknown lines before it are tolerated
    let hello = loop {
        match msg_rx.recv() {
            Ok(msg) if msg.get("hello").is_some() => break msg["hello"].clone(),
            Ok(_) => continue,
            Err(_) => return Ok(()), // stdin closed before hello: nothing to do
        }
    };
    let instance = hello["instance"].as_str().unwrap_or("child").to_string();
    let stream = hello["stream"].as_str().unwrap_or("main").to_string();
    let config = (!hello["config"].is_null()).then(|| hello["config"].clone());
    let mut component = factory(config.as_ref());

    // Spontaneous speech: injector/notify traffic from the component's own
    // background threads drains into emit/notice lines
    let (central_tx, central_rx) = mpsc::channel::<Message>();
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || for _ in wake_rx {});
    {
        let stdout = Arc::clone(&stdout);
        std::thread::spawn(move || {
            for msg in central_rx {
                let line = match msg {
                    Message::Emission { port, draft, .. } => json!({"emit": {
                        "port": port,
                        "type": draft.event_type,
                        "causes": draft.causes,
                        "origin": draft.origin,
                        "payload": draft.payload,
                        "reason": draft.reason,
                    }}),
                    Message::Notice { payload, .. } => json!({"notice": payload}),
                    _ => continue,
                };
                let mut out = stdout.lock().unwrap();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
            }
        });
    }

    // The empty ledger view (degradation #1) and the discarded fragment sink
    let empty_log = EventLog::in_memory(crate::contracts::core_events::core_event_decls(), stream);
    let reader = empty_log.reader();
    let child_ctx = |token: CancellationToken| Ctx {
        source: instance.clone(),
        cancellation: token,
        log: reader.clone(),
        foreign: Arc::new(HashMap::new()),
        prompts: Arc::default(),
        tools: Arc::default(),
        central: central_tx.clone(),
        wake: wake_tx.clone(),
        out: Vec::new(),
        failure: None,
        // A child process keeps its own ledger view empty (degradation #1),
        // and naming a file it cannot read would be worse than silence.
        ledger_path: None,
    };

    // One-time restore, same as an in-process thread start: re-arm background
    // wake sources (they will speak through the spontaneous channel)
    {
        let mut ctx = child_ctx(CancellationToken::new());
        component.restore(&mut ctx);
        if let Some(failure) = ctx.failure {
            return Err(std::io::Error::other(format!(
                "{}: {}",
                failure.operation, failure.message
            )));
        }
        for (port, draft) in ctx.out {
            say(emit_line(&port, draft));
        }
    }

    for msg in msg_rx {
        if msg.get("stop").is_some() {
            break;
        }
        // `cancel` never reaches here — the stdin thread acts on it directly,
        // which is the only way it can arrive while `handle` is running.
        let Some(deliver) = msg.get("deliver") else {
            continue; // tolerate unknown lines (forward compat)
        };
        let Ok(event) = serde_json::from_value::<EventEnvelope>(deliver["event"].clone()) else {
            continue;
        };
        let port = deliver["port"].as_str().unwrap_or_default().to_string();
        let token = CancellationToken::new();
        *current.lock().unwrap_or_else(|e| e.into_inner()) = token.clone();
        let mut ctx = child_ctx(token);
        component.handle(&port, &event, &mut ctx);
        if let Some(failure) = ctx.failure {
            return Err(std::io::Error::other(format!(
                "{}: {}",
                failure.operation, failure.message
            )));
        }
        for (out_port, draft) in ctx.out {
            say(emit_line(&out_port, draft));
        }
        say(json!({"processed": true}));
    }
    Ok(())
}
