//! When a model asks for three things at once, it means them to happen at
//! once.
//!
//! A mailbox with one worker drains in order, so the second call waits for the
//! first to come back. A real session paid ninety seconds for three commands
//! of thirty — and the agent, asked whether they had run together, said yes
//! until it read the source. `concurrency` on a manifest turns that mailbox
//! into a work queue with several workers, each holding its own instance of
//! the component, and only components that keep nothing between deliveries may
//! ask for it.
//!
//! No clock decides anything here. Each command waits for the others on a
//! named pipe — a wait the kernel ends when a writer arrives, not when enough
//! time has passed — so under one worker none of them can finish and under
//! several they all do. Parallelism is the only way through.
//!
//! It was not always so. The first version polled a marker file on a counted
//! `sleep` loop, and the count was both the patience and the proof: on a
//! loaded machine it ran out before the others arrived, and a correct runtime
//! failed. A blocking rendezvous cannot fail that way — a slow machine makes
//! everyone wait longer and changes nothing about who gets through.
#![cfg(unix)]

use std::collections::HashMap;

use serde_json::json;

use lattice::components::shell_tools;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

fn driver_manifest() -> ComponentManifest {
    ComponentManifest {
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
    }
}

/// Every case here names its own deadline, because every case waits on a pipe
/// and a pipe never gives up: the deadline is what turns "this runtime is
/// broken" from a hang into a failure. None of them let it decide the verdict.
fn kernel_with(workers: Option<usize>, cwd: &std::path::Path, deadline_ms: u64) -> Kernel {
    let mut shell = shell_tools::manifest();
    shell.concurrency = workers;
    shell.handle_timeout_ms = Some(deadline_ms);
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (shell_tools::NAME.to_string(), shell),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    let cwd = cwd.to_string_lossy().into_owned();
    factories.insert(
        shell_tools::NAME.to_string(),
        Box::new(move |_| {
            Box::new(shell_tools::ShellTools::from_config(Some(&json!({
                "cwd": cwd,
                // Test mailbox concurrency, not automatic process handoff.
                // Keep ownership until each test's watchdog ends the delivery.
                "yieldAfterMs": 30_000,
            }))))
        }),
    );
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), ComponentInstance::new("driver", None)),
            (
                "shell".to_string(),
                ComponentInstance::new(shell_tools::NAME, None),
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "shell.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("valid assembly")
}

const CREW: [&str; 3] = ["one", "two", "three"];

/// One named pipe per ORDERED pair, made before anyone runs.
///
/// A pipe per member would be fewer files and would not work: a reader sees
/// end-of-file only once every writer has closed, so one `cat` can swallow
/// both greetings at once and leave the second waiting for a writer that has
/// already been and gone. One pipe per direction gives every read exactly one
/// writer, and nothing depends on the order they arrive in.
fn make_pipes(dir: &std::path::Path) {
    for from in CREW {
        for to in CREW.iter().filter(|to| **to != from) {
            let status = std::process::Command::new("mkfifo")
                .arg(dir.join(format!("{from}-to-{to}")))
                .status()
                .expect("mkfifo runs");
            assert!(status.success(), "could not make the pipe {from}-to-{to}");
        }
    }
}

/// Each command greets the other two and waits to be greeted by both.
///
/// The wait blocks in the KERNEL on a named pipe — opening one for reading
/// returns when a writer opens it, and not before. Nothing here consults a
/// clock, which is the point: an earlier version polled a marker file on a
/// counted `sleep` loop, and under a loaded machine the count ran out before
/// the others arrived, so a correct runtime failed the test. A blocking
/// rendezvous cannot do that — a slow machine only makes everyone wait
/// longer, and the verdict is the same either way.
///
/// It also means a command that is alone waits FOREVER rather than giving up,
/// which is exactly what the serialised case below wants to demonstrate; the
/// watchman is what ends it there.
fn rendezvous(dir: &std::path::Path, me: &str) -> String {
    let peers = || CREW.iter().filter(move |who| **who != me);
    let pipe = |from: &str, to: &str| dir.join(format!("{from}-to-{to}")).display().to_string();
    // Greetings go out in the background, because opening a pipe to write
    // waits for its reader; sending both first and only then listening would
    // be a crew of three each waiting to be heard before hearing anyone.
    let greet: Vec<String> = peers()
        .map(|who| format!("echo . > {} &", pipe(me, who)))
        .collect();
    let listen: Vec<String> = peers()
        .map(|who| format!("cat {} > /dev/null;", pipe(who, me)))
        .collect();
    format!("{} {} echo {me}-through", greet.join(" "), listen.join(" "))
}

#[test]
fn three_commands_asked_for_together_run_together() {
    let dir = tempfile::tempdir().unwrap();
    make_pipes(dir.path());
    // Working, the rendezvous takes about a tenth of a second, so this is a
    // hundredfold of headroom and a loaded machine never meets it. It is here
    // only to bound the FAILING case: waiting on a pipe never gives up, and
    // on the default deadline a broken runtime would take six minutes to say
    // so instead of half a one.
    let mut kernel = kernel_with(Some(4), dir.path(), 10_000);

    for who in CREW {
        kernel.injector("driver").emit(
            "out",
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({
                    "call": who,
                    "tool": "Run",
                    "arguments": {"command": rendezvous(dir.path(), who)},
                }),
            ),
        );
    }
    kernel.run_until_quiescent().unwrap();

    let events = kernel.log().replay(1).unwrap();
    for who in CREW {
        let done = events
            .iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == who)
            .unwrap_or_else(|| panic!("{who} never came back"))
            .payload
            .clone();
        assert_eq!(done["result"]["exit_code"], 0, "{who}: {done}");
        assert!(
            done["result"]["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains(&format!("{who}-through")),
            "{who} did not get through the rendezvous, so it ran alone: {done}"
        );
    }
    kernel.shutdown();
}

/// The mirror, so the test above is known to be testing something. Serialised,
/// no command can ever meet another: each holds the only worker while waiting
/// for company that cannot start until it lets go.
///
/// Waiting on a pipe never gives up, so the watchman is what ends it — a short
/// deadline, set here only so the test finishes. The deadline cannot change
/// the VERDICT: if these ran together they would greet each other and print
/// long before it, and if they run one at a time no deadline however long
/// would let them. A loaded machine makes this slower, never wrong.
#[test]
fn with_one_worker_the_same_three_cannot() {
    let dir = tempfile::tempdir().unwrap();
    make_pipes(dir.path());
    let mut kernel = kernel_with(Some(1), dir.path(), 1_500);

    for who in CREW {
        kernel.injector("driver").emit(
            "out",
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({
                    "call": who,
                    "tool": "Run",
                    "arguments": {"command": rendezvous(dir.path(), who)},
                }),
            ),
        );
    }
    kernel.run_until_quiescent().unwrap();

    let events = kernel.log().replay(1).unwrap();
    for who in CREW {
        let done = events
            .iter()
            .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == who)
            .unwrap_or_else(|| panic!("{who} never came back"))
            .payload
            .clone();
        assert!(
            !done["result"]["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("-through"),
            "serialised, {who} cannot meet anyone — if it did, the test above \
             proves nothing: {done}"
        );
    }
    kernel.shutdown();
}

/// Results come back in whatever order they finish, not the order they were
/// asked for. That is the trade `concurrency` makes, and it is stated here so
/// nothing downstream may quietly assume otherwise.
#[test]
fn results_arrive_in_finishing_order_not_asking_order() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    assert!(std::process::Command::new("mkfifo")
        .arg(&gate)
        .status()
        .expect("mkfifo runs")
        .success());
    // Bounded for the same reason as the test above: working, this is a tenth
    // of a second, and a runtime that serialised these would otherwise take
    // the full default deadline to admit it.
    let mut kernel = kernel_with(Some(4), dir.path(), 10_000);

    // What releases the slow one is the QUICK ONE'S RESULT LANDING ON THE
    // LEDGER — not a file it touched on its way out, and not a wait long
    // enough to be sure. So the order asserted below is the order that had to
    // happen, rather than the order that usually does.
    //
    // The watcher runs on its own thread because opening the pipe to write
    // blocks until the slow command reads it, and the ledger's subscriber
    // must not be the thing that blocks.
    let (seen, released) = std::sync::mpsc::channel::<()>();
    kernel.subscribe_log(move |event| {
        if event.event_type == ce::TOOL_EXEC_COMPLETED && event.payload["call"] == "quick" {
            let _ = seen.send(());
        }
    });
    let opener = gate.clone();
    std::thread::spawn(move || {
        if released.recv().is_ok() {
            let _ = std::fs::write(&opener, b"go\n");
        }
    });

    // Asked slowest first; the slow one cannot end until the quick one has.
    for (call, command) in [
        ("slow", format!("cat {} > /dev/null", gate.display())),
        ("quick", "true".to_string()),
    ] {
        kernel.injector("driver").emit(
            "out",
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[],
                json!({"call": call, "tool": "Run", "arguments": {"command": command}}),
            ),
        );
    }
    kernel.run_until_quiescent().unwrap();

    let order: Vec<String> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .filter_map(|e| e.payload["call"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        order,
        vec!["quick", "slow"],
        "the one that finished first came back first"
    );
    kernel.shutdown();
}
