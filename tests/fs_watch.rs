//! fs-watch: the event-driven wake source. A watched path wakes the loop the
//! moment it actually changes (OS notification, no polling), each fire caused
//! by the watch call that armed it; unwatch stops it; a live watch re-arms on
//! restart and continues its fire count. Waits are causal (block on the wake
//! signal), never bare sleeps.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use serde_json::{json, Value};

use lattice::components::fs_watch;
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

fn open(file: Option<&Path>) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (fs_watch::NAME.to_string(), fs_watch::manifest()),
    ]
    .into();
    let mut f: HashMap<String, Factory> = HashMap::new();
    f.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    f.insert(
        fs_watch::NAME.to_string(),
        Box::new(|c| Box::new(fs_watch::FsWatch::from_config(c))),
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
                "watch".to_string(),
                ComponentInstance {
                    component: fs_watch::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "watch.execute")],
    };
    let options = KernelOptions {
        stream: Some("main".to_string()),
        log_file: file.map(Path::to_path_buf),
        ..KernelOptions::default()
    };
    Kernel::start(&assembly, &registry, &mut f, options).unwrap()
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

/// Run to quiescence, then wait for the next wake; ends when no wake arrives
/// within `quiet` — the watch has gone silent.
fn pump(kernel: &mut Kernel, wake_rx: &Receiver<()>, quiet: Duration) {
    loop {
        kernel.run_until_quiescent().unwrap();
        if wake_rx.recv_timeout(quiet).is_err() {
            break;
        }
    }
}

/// Keep OS notification integration tests isolated from one another. This
/// serializes test load; it is not a cure for backend registration stalls.
/// The lock is taken, never poisoned away: one failure must not mask others.
static WATCHER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    WATCHER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run to quiescence until `done` holds, bounded by `within`.
///
/// One pass is not proof: `run_until_quiescent` measures an IDLE kernel, and a
/// just-injected event that no component has picked up yet leaves it idle. So
/// the test that asserted straight after a single pass was reading the ledger
/// before the call it drove had been handled — rare alone, common once the rest
/// of the file was busy enough to lose the race.
///
/// The condition is the real signal (the event is on the ledger); the short
/// wait between passes only keeps this from spinning a core.
fn settle_until(kernel: &mut Kernel, within: Duration, done: impl Fn(&Kernel) -> bool) {
    let deadline = std::time::Instant::now() + within;
    loop {
        kernel.run_until_quiescent().unwrap();
        if done(kernel) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the driven call was never handled"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Has the call this test drove been answered?
fn completed(kernel: &Kernel) -> Option<EventEnvelope> {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
}

fn fires(kernel: &Kernel, watch: u64) -> Vec<EventEnvelope> {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::WAKE && e.payload["body"]["watch"] == watch)
        .collect()
}

/// Keep changing `dir` until the watch has fired more than `beyond` times.
///
/// The wait here cannot be made causal the way the rest of the suite is: the
/// fire depends on an OS notification, which this process does not cause and
/// cannot observe until it lands. What it can stop doing is betting on one
/// delivery arriving inside one timeout — under the load of the whole suite it
/// did not, and this test went red while passing on its own. So the loop keeps
/// supplying the cause until the effect shows up, bounded by `within`.
///
/// It drains to quiet before returning, so the fire numbers on the ledger are
/// the whole run rather than a prefix cut short by the shutdown that follows.
fn fires_beyond(
    kernel: &mut Kernel,
    wake_rx: &Receiver<()>,
    dir: &Path,
    beyond: u64,
    within: Duration,
) -> Vec<EventEnvelope> {
    let deadline = std::time::Instant::now() + within;
    let mut round = 0u64;
    while (fires(kernel, 1).len() as u64) <= beyond {
        assert!(
            std::time::Instant::now() < deadline,
            "the watch never fired past {beyond}"
        );
        round += 1;
        std::fs::write(dir.join(format!("touch-{round}.txt")), format!("{round}")).unwrap();
        if wake_rx.recv_timeout(Duration::from_millis(400)).is_ok() {
            kernel.run_until_quiescent().unwrap();
        }
    }
    pump(kernel, wake_rx, Duration::from_millis(500));
    fires(kernel, 1)
}

#[cfg(target_os = "macos")]
#[test]
fn macos_registration_does_not_depend_on_the_fsevents_service() {
    use notify::Watcher;
    assert_eq!(
        notify::RecommendedWatcher::kind(),
        notify::WatcherKind::Kqueue
    );
}

#[cfg(target_os = "macos")]
#[test]
fn exhausted_watch_resources_return_an_error_instead_of_hanging() {
    const CHILD: &str = "LATTICE_WATCH_RESOURCE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exhausted_watch_resources_return_an_error_instead_of_hanging",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for n in 0..128 {
        std::fs::write(dir.path().join(n.to_string()), "test").unwrap();
    }
    let mut kernel = open(None);
    // Limit only this disposable child, never the test runner or product.
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        limit.rlim_cur = 64.min(limit.rlim_max);
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
    }
    drive(
        &kernel,
        "resource",
        "Watch",
        json!({"path": dir.path(), "recursive": true}),
    );
    settle_until(&mut kernel, Duration::from_secs(10), |k| {
        completed(k).is_some()
    });
    let answer = completed(&kernel).unwrap();
    assert_eq!(answer.payload["status"], "error");
    assert_eq!(answer.payload["error"]["code"], "watch.failed");
    kernel.shutdown();
}

#[test]
fn a_change_on_a_watched_directory_wakes_with_causality() {
    let _serial = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = open(None);
    let wake_rx = kernel.take_wake_receiver().unwrap();

    drive(
        &kernel,
        "w1",
        "Watch",
        json!({"path": dir.path().display().to_string(), "debounce_ms": 50}),
    );
    settle_until(&mut kernel, Duration::from_secs(10), |k| {
        completed(k).is_some()
    });
    let events = kernel.log().replay(1).unwrap();
    let armed = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .expect("the watch call completed");
    assert_eq!(armed.payload["status"], "ok");
    assert_eq!(armed.payload["result"]["watch"], 1);
    let started_id = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .unwrap()
        .id
        .clone();

    // The actual change — the only thing that may wake anyone
    let fired = fires_beyond(
        &mut kernel,
        &wake_rx,
        dir.path(),
        0,
        Duration::from_secs(20),
    );
    assert!(
        !fired.is_empty(),
        "a change on the watched directory must fire a wake"
    );
    assert!(
        fired[0].causes.contains(&started_id),
        "the fire is caused by the watch call that armed it"
    );
    assert!(
        fired[0].payload["body"]["changed"]
            .as_array()
            .is_some_and(|c| !c.is_empty()),
        "the wake names the changed paths"
    );
    kernel.shutdown();
}

#[test]
fn unwatch_stops_the_fires() {
    let _serial = one_at_a_time();
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = open(None);
    let wake_rx = kernel.take_wake_receiver().unwrap();

    drive(
        &kernel,
        "w1",
        "Watch",
        json!({"path": dir.path().display().to_string(), "debounce_ms": 50}),
    );
    kernel.run_until_quiescent().unwrap();
    let before = fires_beyond(
        &mut kernel,
        &wake_rx,
        dir.path(),
        0,
        Duration::from_secs(20),
    )
    .len();
    assert!(before >= 1, "the watch fired while live");

    drive(&kernel, "w2", "Unwatch", json!({"watch": 1}));
    kernel.run_until_quiescent().unwrap();
    std::fs::write(dir.path().join("two.txt"), "2").unwrap();
    pump(&mut kernel, &wake_rx, Duration::from_secs(1));

    assert_eq!(
        fires(&kernel, 1).len(),
        before,
        "an unwatched path fires no further wakes"
    );
    kernel.shutdown();
}

#[test]
fn a_missing_path_is_refused_as_data() {
    let _serial = one_at_a_time();
    let mut kernel = open(None);
    drive(
        &kernel,
        "w1",
        "Watch",
        json!({"path": "/no/such/path/anywhere"}),
    );
    settle_until(&mut kernel, Duration::from_secs(10), |k| {
        completed(k).is_some()
    });
    let completion = completed(&kernel).unwrap();
    assert_eq!(completion.payload["status"], "error");
    assert_eq!(completion.payload["error"]["code"], "watch.no_such_path");
    kernel.shutdown();
}

#[test]
fn a_live_watch_survives_a_restart_and_continues_its_count() {
    let _serial = one_at_a_time();
    for name in ["main.jsonl", "main.ledger"] {
        live_watch_continues_its_count(name);
    }
}

fn live_watch_continues_its_count(name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let ledger = tempfile::tempdir().unwrap();
    let file = ledger.path().join(name);

    // Life one: arm, see at least one fire
    let mut kernel = open(Some(&file));
    let wake_rx = kernel.take_wake_receiver().unwrap();
    drive(
        &kernel,
        "w1",
        "Watch",
        json!({"path": dir.path().display().to_string(), "debounce_ms": 50}),
    );
    kernel.run_until_quiescent().unwrap();
    let life_one = fires_beyond(
        &mut kernel,
        &wake_rx,
        dir.path(),
        0,
        Duration::from_secs(20),
    )
    .len() as u64;
    assert!(life_one >= 1, "the watch fired before the restart");
    kernel.shutdown();

    // Life two: a FRESH instance on the same ledger. Restore re-arms the
    // watch asynchronously (and a change during the downtime is missed by
    // design — a watch resumes watching, it does not diff), so keep changing
    // the directory until a fire proves the watch is live again.
    let mut kernel = open(Some(&file));
    let wake_rx = kernel.take_wake_receiver().unwrap();
    kernel.run_until_quiescent().unwrap();
    let all = fires_beyond(
        &mut kernel,
        &wake_rx,
        dir.path(),
        life_one,
        Duration::from_secs(20),
    );
    assert!(
        all.len() as u64 > life_one,
        "the restored watch fired after the restart"
    );
    let numbers: Vec<u64> = all
        .iter()
        .filter_map(|e| e.payload["body"]["fire"].as_u64())
        .collect();
    let expected: Vec<u64> = (1..=numbers.len() as u64).collect();
    assert_eq!(
        numbers, expected,
        "fire numbers are contiguous across the restart"
    );
    kernel.shutdown();

    // Life three uses the saved live checkpoint plus the second life's tail.
    // Downtime changes are not replayed as new notifications.
    std::fs::write(dir.path().join("downtime"), b"offline").unwrap();
    let mut kernel = open(Some(&file));
    let wake_rx = kernel.take_wake_receiver().unwrap();
    pump(&mut kernel, &wake_rx, Duration::from_millis(200));
    assert_eq!(fires(&kernel, 1).len(), numbers.len());
    let resumed = fires_beyond(
        &mut kernel,
        &wake_rx,
        dir.path(),
        numbers.len() as u64,
        Duration::from_secs(20),
    );
    let resumed_numbers: Vec<_> = resumed
        .iter()
        .filter_map(|e| e.payload["body"]["fire"].as_u64())
        .collect();
    assert_eq!(
        resumed_numbers,
        (1..=resumed.len() as u64).collect::<Vec<_>>()
    );
    assert!(resumed.len() > numbers.len());
    kernel.shutdown();
}
