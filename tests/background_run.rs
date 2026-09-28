//! Background `run`: a long command returns immediately with a job id, keeps
//! running on its own, and WAKES the loop with its result when it finishes —
//! the first real wake source. The turn is never held for the command; the
//! wake arrives as a fresh input, caused by the run that started it.
#![cfg(unix)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, shell_tools, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

/// ui + loop + scripted model + shell, with shell's wake wired back to the
/// loop's input — the assembly shape that makes a background command a wake
/// source. Returns the kernel and the displayed replies buffer.
fn start(script: serde_json::Value) -> (Kernel, Arc<Mutex<Vec<String>>>) {
    start_with_config(script, None)
}

fn start_with_config(
    script: serde_json::Value,
    shell_config: Option<serde_json::Value>,
) -> (Kernel, Arc<Mutex<Vec<String>>>) {
    let registry: HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (shell_tools::NAME.to_string(), shell_tools::manifest()),
    ]
    .into();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let ui_buf = Arc::clone(&displayed);
    let mut f: HashMap<String, Factory> = HashMap::new();
    f.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&ui_buf)))),
    );
    f.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    f.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    f.insert(
        shell_tools::NAME.to_string(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "loop".to_string(),
                ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(script),
                },
            ),
            (
                "shell".to_string(),
                ComponentInstance {
                    component: shell_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: shell_config,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "shell.execute"),
            Wire::new("shell.outcome", "loop.tools"),
            // The wake wire: a finished background command re-enters as input
            Wire::new("shell.wake", "loop.input"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let kernel = Kernel::start(&assembly, &registry, &mut f, KernelOptions::default()).unwrap();
    (kernel, displayed)
}

// A FIFO, not elapsed time, holds the command until the test releases it.
struct CommandGate {
    _dir: tempfile::TempDir,
    file: std::fs::File,
    command: String,
}
impl CommandGate {
    fn new(code: i32) -> Self {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release");
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let command = format!(
            "read -r line < '{}'; printf 'finished\\n'; exit {code}",
            path.display()
        );
        Self {
            _dir: dir,
            file,
            command,
        }
    }
    fn release(&mut self) {
        use std::io::Write;
        self.file.write_all(b"go\n").unwrap();
    }
}
impl Drop for CommandGate {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = self.file.write_all(b"go\n");
    }
}
fn model_calls(kernel: &Kernel) -> usize {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .count()
}
fn start_waiting(gate: &CommandGate) -> (Kernel, Arc<Mutex<Vec<String>>>) {
    start_with_config(
        json!({"script": [
            {"status":"ok", "toolCalls":[{"id":"waiting", "tool":"Run", "arguments":{"command":gate.command}}]},
            {"status":"ok", "text":"resumed"},
            {"status":"ok", "text":"finished"}
        ]}),
        Some(json!({"yieldAfterMs":0})),
    )
}

#[test]
fn waiting_receipt_does_not_call_the_model_until_completion_even_on_failure() {
    for code in [0, 7] {
        let mut gate = CommandGate::new(code);
        let (mut kernel, _) = start_waiting(&gate);
        let receiver = kernel.take_wake_receiver().unwrap();
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"test"})),
        );
        kernel.run_until_quiescent().unwrap();
        for _ in 0..3 {
            kernel.run_until_quiescent().unwrap();
            assert_eq!(
                model_calls(&kernel),
                1,
                "a running process is not new information"
            );
        }
        let events = kernel.log().replay(1).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == minimal_loop::WAITING)
                .count(),
            1
        );
        assert!(!events.iter().any(|e| e.event_type == ce::TURN_COMPLETED));
        let ack = events
            .iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
            .unwrap();
        assert_eq!(ack.payload["continuation"], "wait");
        gate.release();
        let deadline = Instant::now() + Duration::from_secs(10);
        while model_calls(&kernel) < 2 {
            receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            kernel.run_until_quiescent().unwrap();
        }
        let events = kernel.log().replay(1).unwrap();
        let wakes: Vec<_> = events.iter().filter(|e| e.event_type == ce::WAKE).collect();
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].payload["body"]["exit_code"], code);
        assert!(wakes[0].payload["summary"]
            .as_str()
            .unwrap()
            .contains("read -r line"));
        assert_eq!(model_calls(&kernel), 2);
        kernel.shutdown();
    }
}

#[test]
fn a_person_can_resume_a_waiting_conversation_without_stopping_the_process() {
    let mut gate = CommandGate::new(0);
    let (mut kernel, displayed) = start_waiting(&gate);
    let receiver = kernel.take_wake_receiver().unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"start"})),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(model_calls(&kernel), 1);
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"do not commit"})),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(model_calls(&kernel), 2);
    assert_eq!(*displayed.lock().unwrap(), ["resumed"]);
    assert!(!kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|e| e.event_type == ce::WAKE));
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(10);
    while model_calls(&kernel) < 3 {
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        kernel.run_until_quiescent().unwrap();
    }
    assert_eq!(*displayed.lock().unwrap(), ["resumed", "finished"]);
    kernel.shutdown();
}

#[test]
fn a_background_command_returns_at_once_then_wakes_with_its_result() {
    // Turn 1: model asks to run a slow command in the background, then says a
    // word acknowledging it started. Turn 2 (woken by the finish): model
    // reads the result and replies.
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "j1", "tool": "Run",
            "arguments": {"command": "sleep 1; echo done-work", "background": true}}]},
        {"status": "ok", "text": "started it in the background"},
        {"status": "ok", "text": "the job finished"},
    ]});
    let (mut kernel, displayed) = start(script);
    let wake_rx = kernel.take_wake_receiver().unwrap();

    let started = Instant::now();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "run the slow job"})),
    );

    // The host push loop
    loop {
        kernel.run_until_quiescent().unwrap();
        if wake_rx.recv_timeout(Duration::from_millis(2500)).is_err() {
            break;
        }
    }

    // The first turn did NOT wait for the command: it acknowledged fast
    // (well under the 1s the command sleeps) — proven by the ack being
    // recorded and the first reply present early. We assert timing loosely:
    // the whole thing (both turns) took at least the 1s the command runs.
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the command really ran"
    );

    let events = kernel.log().replay(1).unwrap();
    // The background ack: the tool call completed immediately with a job id
    let ack = events
        .iter()
        .find(|e| {
            e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["result"]["background"] == true
        })
        .expect("the background run acknowledged at once with a job id");
    assert_eq!(ack.payload["result"]["job"], "j1");
    // The ack also carries a real pid the agent can kill
    assert!(
        ack.payload["result"]["pid"].as_u64().is_some(),
        "the ack carries a pid"
    );

    // The wake: caused by the run that started it, carrying the result
    let wake = events
        .iter()
        .find(|e| e.event_type == ce::WAKE)
        .expect("the finished command woke the loop");
    assert_eq!(wake.source, "shell");
    assert_eq!(wake.payload["body"]["exit_code"], 0);
    let final_log = &wake.payload["body"]["logs"]["stdout"];
    assert_eq!(
        ack.payload["result"]["logs"]["stdout"]["path"],
        final_log["path"]
    );
    assert_eq!(ack.payload["result"]["logs"]["stdout"]["sealed"], false);
    assert_eq!(final_log["sealed"], true);
    assert_eq!(final_log["complete"], true);
    assert_eq!(
        std::fs::read_to_string(final_log["path"].as_str().unwrap()).unwrap(),
        "done-work\n"
    );
    assert!(wake.payload["body"]["cwd"].as_str().is_some());
    assert!(wake.payload["body"]["stdout"]
        .as_str()
        .unwrap()
        .contains("done-work"));
    // Its cause is the tool_exec_started that launched the background command
    let launch = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.payload["call"] == "j1")
        .unwrap();
    assert_eq!(wake.causes, vec![launch.id.clone()]);

    // And the wake drove a second model turn whose reply reached the user
    assert_eq!(
        *displayed.lock().unwrap(),
        vec![
            "started it in the background".to_string(),
            "the job finished".to_string()
        ]
    );

    kernel.shutdown();
}

/// A wake arriving MID-ROUND must not start a second model call.
///
/// The round's tool results have not all come back yet; asking again now puts
/// a second assistant turn on the record before the first one's calls are
/// answered, and both wire formats reject that conversation outright — every
/// later message fails, not just this one. The wake is already in the
/// material, so it rides along when the round closes.
///
/// Seen in the wild: two background wakes during a round the trust gate was
/// holding, and the session could never speak again.
#[test]
fn a_wake_during_an_open_round_does_not_start_a_second_model_call() {
    // A tool provider that never answers — standing in for a gate waiting on
    // a human, or any call still in flight
    struct Deaf;
    impl Component for Deaf {
        fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
    }
    let deaf = ComponentManifest {
        name: "deaf".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:deaf".to_string(),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![
            PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED]),
            // A wake source, wired to the loop's input exactly as the shell's
            // background jobs are — this is the real shape being tested
            PortDecl::new("wake", &[ce::WAKE]),
        ],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        ("deaf".to_string(), deaf),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::default()))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert("deaf".to_string(), Box::new(|_| Box::new(Deaf)));

    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "c1", "tool": "hang", "arguments": {}}]},
        {"status": "ok", "text": "second call — must not happen"},
    ]});
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance::new(silent_ui::NAME, None),
            ),
            (
                "loop".to_string(),
                ComponentInstance::new(minimal_loop::NAME, None),
            ),
            (
                "model".to_string(),
                ComponentInstance::new(scripted_model::NAME, Some(script)),
            ),
            ("tools".to_string(), ComponentInstance::new("deaf", None)),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("tools.wake", "loop.input"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();

    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "do it"})),
    );
    kernel.run_until_quiescent().unwrap();
    // The round is open: one ask went out, the tool never answered
    let asks = |k: &Kernel| {
        k.log()
            .replay(1)
            .unwrap()
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
            .count()
    };
    assert_eq!(asks(&kernel), 1, "the round asked once");

    // A background wake lands while the call is still out
    kernel.injector("tools").emit(
        "wake",
        EventDraft::new(ce::WAKE, &[], json!({"source": "timer", "summary": "tick"})),
    );
    kernel.run_until_quiescent().unwrap();
    // It really landed — a wake the kernel refused would prove nothing
    assert!(
        kernel
            .log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == ce::WAKE),
        "the wake must actually reach the ledger"
    );
    assert_eq!(
        asks(&kernel),
        1,
        "the wake must NOT open a second call on top of an unanswered one"
    );
    let started = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .unwrap();
    kernel.injector("tools").emit(
        "outcome",
        EventDraft::new(
            ce::TOOL_EXEC_COMPLETED,
            &[&started.id],
            json!({"status":"ok", "call":"c1", "continuation":"wait", "result":{}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    assert_eq!(
        asks(&kernel),
        2,
        "a waiting receipt must not hide the earlier wake"
    );
    assert!(!kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|e| e.event_type == minimal_loop::WAITING));
    kernel.shutdown();
}

#[test]
fn a_background_job_can_be_killed_by_its_pid() {
    use lattice::{Component, ComponentManifest, Ctx, EventEnvelope, PortDecl, RuntimeKind};
    use std::collections::HashMap as Map;

    // A driver that just injects tool requests; handles nothing.
    struct Driver;
    impl Component for Driver {
        fn handle(&mut self, _p: &str, _e: &EventEnvelope, _c: &mut Ctx) {}
    }
    let driver_manifest = ComponentManifest {
        name: "driver".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:driver".to_string(),
        inputs: vec![],
        outputs: vec![PortDecl::new("out", &[ce::TOOL_EXEC_STARTED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let registry: Map<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest),
        (shell_tools::NAME.to_string(), shell_tools::manifest()),
    ]
    .into();
    let mut f: Map<String, Factory> = Map::new();
    f.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    f.insert(
        shell_tools::NAME.to_string(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "driver".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "shell".to_string(),
                ComponentInstance {
                    component: shell_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "shell.execute")],
    };
    let mut kernel = Kernel::start(&assembly, &registry, &mut f, KernelOptions::default()).unwrap();

    // Launch a long-lived background job (writes a marker file we can watch)
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("alive");
    let cmd = format!("touch {m}; sleep 30; rm {m}", m = marker.display());
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "bg", "tool": "Run", "arguments": {"command": cmd, "background": true}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    // Read the pid from the ack, wait for the job to be alive
    let pid = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .find(|e| {
            e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["result"]["background"] == true
        })
        .and_then(|e| e.payload["result"]["pid"].as_u64())
        .expect("a pid") as i32;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "the background job never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Kill it the way an agent would: `run("kill -TERM -<pid>")` — a normal
    // foreground command. (Driving the tool directly stands in for the model.)
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "kill", "tool": "Run",
                   "arguments": {"command": format!("kill -TERM -{pid}")}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    // The job dies before its 30s sleep — the marker never gets removed by
    // the job itself, but the process is gone; confirm the group is dead
    let killed_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if !alive {
            break;
        }
        assert!(
            Instant::now() < killed_deadline,
            "the background job was not killed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    kernel.shutdown();
}
