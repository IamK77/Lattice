//! Wake sources survive a restart. Timers are pure data, so a repeating timer
//! re-arms on reopen and continues its fire count where the dead process
//! stopped; an unfired one-shot and an unscheduled timer do not come back. A
//! background job cannot be re-waited (its child is orphaned), so it is not
//! re-run — it is settled: a wake telling the agent it was cut off, recorded
//! once no matter how many times the ledger reopens.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use lattice::components::{shell_tools, timer_tools};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _p: &str, _e: &EventEnvelope, _c: &mut Ctx) {}
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

fn options(file: &Path) -> KernelOptions {
    KernelOptions {
        stream: Some("main".to_string()),
        log_file: Some(file.to_path_buf()),
        ..KernelOptions::default()
    }
}

/// driver → tool assembly, persisted to `file`. `tool_name`/`manifest`/factory
/// pick which tool provider (timer or shell) sits behind the driver.
fn open(file: &Path, tool: &str, manifest: ComponentManifest, factory: Factory) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (tool.to_string(), manifest),
    ]
    .into();
    let mut f: HashMap<String, Factory> = HashMap::new();
    f.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    f.insert(tool.to_string(), factory);
    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "driver".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "tool".to_string(),
                ComponentInstance {
                    component: tool.to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "tool.execute")],
    };
    Kernel::start(&assembly, &registry, &mut f, options(file)).unwrap()
}

fn open_timer(file: &Path) -> Kernel {
    open(
        file,
        timer_tools::NAME,
        timer_tools::manifest(),
        Box::new(|c| Box::new(timer_tools::TimerTools::from_config(c))),
    )
}

fn open_shell(file: &Path) -> Kernel {
    open(
        file,
        shell_tools::NAME,
        shell_tools::manifest(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
    )
}

fn drive(kernel: &Kernel, call: &str, tool: &str, args: Value) {
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": call, "tool": tool, "arguments": args}),
        ),
    );
}

/// The host push loop: run to quiescence, then wait for the next wake. Ends
/// when no wake arrives within `quiet` (the wake source has gone silent).
fn pump(kernel: &mut Kernel, wake_rx: &Receiver<()>, quiet: Duration) {
    loop {
        kernel.run_until_quiescent().unwrap();
        if wake_rx.recv_timeout(quiet).is_err() {
            break;
        }
    }
}

/// Restore runs asynchronously in the tool's thread and (for a background
/// settle) fires no wake, so poll the ledger until `cond` holds.
fn pump_until(kernel: &mut Kernel, cond: impl Fn(&Kernel) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        kernel.run_until_quiescent().unwrap();
        if cond(kernel) {
            return;
        }
        assert!(Instant::now() < deadline, "restore did not settle in time");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Every timer fire number recorded, per timer id.
fn fires_by_timer(kernel: &Kernel) -> HashMap<u64, Vec<u64>> {
    let mut out: HashMap<u64, Vec<u64>> = HashMap::new();
    for e in kernel.log().replay(1).unwrap() {
        if e.event_type == ce::WAKE {
            if let (Some(t), Some(n)) = (
                e.payload["body"]["timer"].as_u64(),
                e.payload["body"]["fire"].as_u64(),
            ) {
                out.entry(t).or_default().push(n);
            }
        }
    }
    out
}

#[test]
fn a_repeating_timer_resumes_and_an_unfired_one_shot_does_not() {
    for name in ["main.jsonl", "main.ledger"] {
        repeating_timer_resumes_without_resurrecting_one_shot(name);
    }
}

fn repeating_timer_resumes_without_resurrecting_one_shot(name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(name);

    // Life one: a repeating timer (id 1, capped at 4) and a one-shot that
    // will not fire for a long time (id 2). Let the repeater fire at least
    // once, then die mid-course.
    let mut kernel = open_timer(&file);
    let wake_rx = kernel.take_wake_receiver().unwrap();
    drive(
        &kernel,
        "rep",
        "Schedule",
        json!({"delay_ms": 15, "interval_ms": 15, "max_fires": 4}),
    );
    drive(&kernel, "once", "Schedule", json!({"delay_ms": 100_000}));
    loop {
        kernel.run_until_quiescent().unwrap();
        let some = fires_by_timer(&kernel).get(&1).map(Vec::len).unwrap_or(0) >= 1;
        if some || wake_rx.recv_timeout(Duration::from_millis(300)).is_err() {
            break;
        }
    }
    assert!(
        fires_by_timer(&kernel).get(&1).map(Vec::len).unwrap_or(0) >= 1,
        "the repeating timer fired at least once before the restart"
    );
    assert!(
        kernel.log().replay(1).unwrap().iter().any(|event| {
            event.event_type == ce::TOOL_EXEC_COMPLETED
                && event.payload["call"] == "once"
                && event.payload["status"] == "ok"
                && event.payload["result"]["timer"] == 2
        }),
        "the one-shot must actually have been scheduled before testing its recovery"
    );
    kernel.shutdown();

    // Life two: a FRESH timer instance on the same ledger. Restore re-arms the
    // repeater; it runs out its remaining fires. The one-shot stays gone.
    let mut kernel = open_timer(&file);
    let wake_rx = kernel.take_wake_receiver().unwrap();
    pump(&mut kernel, &wake_rx, Duration::from_millis(200));

    let fires = fires_by_timer(&kernel);
    let mut repeater = fires.get(&1).cloned().unwrap_or_default();
    repeater.sort_unstable();
    assert_eq!(
        repeater,
        vec![1, 2, 3, 4],
        "the repeater's fires are contiguous across the restart — no repeat, no gap, no restart from 1"
    );
    assert!(
        !fires.contains_key(&2),
        "an unfired one-shot is not resurrected on restart"
    );
    kernel.shutdown();
}

#[test]
fn an_unscheduled_timer_stays_dead_across_a_restart() {
    for name in ["main.jsonl", "main.ledger"] {
        unscheduled_timer_stays_dead(name);
    }
}

fn unscheduled_timer_stays_dead(name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(name);

    // Life one: a repeating timer with plenty of headroom, then unschedule it.
    let mut kernel = open_timer(&file);
    let wake_rx = kernel.take_wake_receiver().unwrap();
    drive(
        &kernel,
        "rep",
        "Schedule",
        json!({"delay_ms": 15, "interval_ms": 15, "max_fires": 50}),
    );
    loop {
        kernel.run_until_quiescent().unwrap();
        let fired = fires_by_timer(&kernel).get(&1).map(Vec::len).unwrap_or(0) >= 1;
        if fired {
            drive(&kernel, "stop", "Unschedule", json!({"timer": 1}));
            kernel.run_until_quiescent().unwrap();
            break;
        }
        if wake_rx.recv_timeout(Duration::from_millis(300)).is_err() {
            break;
        }
    }
    let before = fires_by_timer(&kernel).get(&1).map(Vec::len).unwrap_or(0);
    kernel.shutdown();

    // Life two: restore must respect the cancellation and NOT re-arm it. Wait
    // long enough that a wrongly re-armed timer would have fired several times.
    let mut kernel = open_timer(&file);
    let wake_rx = kernel.take_wake_receiver().unwrap();
    pump(&mut kernel, &wake_rx, Duration::from_millis(200));
    let after = fires_by_timer(&kernel).get(&1).map(Vec::len).unwrap_or(0);
    assert_eq!(
        after, before,
        "an unscheduled timer must not fire again after a restart"
    );
    kernel.shutdown();
}

#[test]
fn a_waiting_receipt_is_settled_on_restart_without_running_its_command() {
    for name in ["main.jsonl", "main.ledger"] {
        waiting_receipt_is_settled_without_running_its_command(name);
    }
}

fn waiting_receipt_is_settled_without_running_its_command(name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(name);
    let marker = dir.path().join("must-not-exist");
    let mut log =
        lattice::EventLog::open(ce::core_event_decls(), "main", Some(file.clone())).unwrap();
    let started = log.append(EventDraft::new(ce::TOOL_EXEC_STARTED, &[],
        json!({"call":"waiting", "tool":"Run", "arguments":{"command":format!("touch '{}'", marker.display())}})), "driver").unwrap();
    log.append(EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&started.id],
        json!({"call":"waiting", "status":"ok", "continuation":"wait", "result":{"background":true,"job":"waiting"}})), "tool").unwrap();
    drop(log);
    for _ in 0..2 {
        let mut kernel = open_shell(&file);
        kernel.run_until_quiescent().unwrap();
        let events = kernel.log().replay(1).unwrap();
        let settled: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == ce::WAKE && e.payload["body"]["interrupted"] == "restart")
            .collect();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].causes, std::slice::from_ref(&started.id));
        assert!(settled[0].payload["body"].get("exit_code").is_none());
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED)
                .count(),
            1
        );
        assert!(!events
            .iter()
            .any(|e| e.event_type == ce::MODEL_CALL_STARTED));
        assert!(
            !marker.exists(),
            "reopening must not execute the recorded command"
        );
        kernel.shutdown();
    }
}

#[test]
fn a_cut_off_background_job_is_settled_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.jsonl");

    // Life one: launch a long background job; its ack lands but its completion
    // wake never will (we die long before it finishes).
    let mut kernel = open_shell(&file);
    drive(
        &kernel,
        "bg",
        "Run",
        json!({"command": "sleep 30", "background": true}),
    );
    kernel.run_until_quiescent().unwrap();
    let pid = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .find(|e| {
            e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["result"]["background"] == true
        })
        .and_then(|e| e.payload["result"]["pid"].as_u64())
        .expect("the background run acknowledged with a pid");
    kernel.shutdown();

    let is_settle = |e: &EventEnvelope| {
        e.event_type == ce::WAKE && e.payload["body"]["interrupted"] == "restart"
    };

    // Life two: reopen. Restore settles the cut-off job — a wake that says the
    // job was interrupted (outcome unknown), caused by the run that started it.
    let mut kernel = open_shell(&file);
    pump_until(&mut kernel, |k| {
        k.log().replay(1).unwrap().iter().any(is_settle)
    });
    let events = kernel.log().replay(1).unwrap();
    let settle = events.iter().find(|e| is_settle(e)).unwrap();
    let launch = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .unwrap();
    assert_eq!(
        settle.causes,
        vec![launch.id.clone()],
        "settle points at the launch"
    );
    assert_eq!(settle.payload["body"]["job"], "bg");
    assert!(settle.payload["source"]
        .as_str()
        .unwrap()
        .starts_with("background:"));
    kernel.shutdown();

    // Life three — the call-it-again probe: a job already settled stays
    // settled. The settle wake IS a wake caused by the launch, so restore
    // reads the job as resolved and adds nothing. Pump briefly so that a
    // WRONG second settle would actually be recorded (and caught) here.
    let mut kernel = open_shell(&file);
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        kernel.run_until_quiescent().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let settles = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .filter(|e| is_settle(e))
        .count();
    assert_eq!(
        settles, 1,
        "settling a background job is idempotent across reopenings"
    );
    kernel.shutdown();

    // Housekeeping: the orphaned sleep is in its own group; reap it.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}
