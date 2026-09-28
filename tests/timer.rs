//! Timer tools: the time-driven wake source. A scheduled timer wakes the loop
//! after a delay (once) or on an interval (repeating, bounded), and unschedule
//! stops it. loop and poll-monitor are the same repeating timer, so this
//! proves all three time/condition wake shapes at once.
#![cfg(unix)]

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;

use lattice::components::timer_tools;
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

fn timer_kernel() -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (timer_tools::NAME.to_string(), timer_tools::manifest()),
    ]
    .into();
    let mut f: HashMap<String, Factory> = HashMap::new();
    f.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    f.insert(
        timer_tools::NAME.to_string(),
        Box::new(|c| Box::new(timer_tools::TimerTools::from_config(c))),
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
                "timer".to_string(),
                ComponentInstance {
                    component: timer_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "timer.execute")],
    };
    Kernel::start(&assembly, &registry, &mut f, KernelOptions::default()).unwrap()
}

fn schedule(kernel: &Kernel, call: &str, args: serde_json::Value) {
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": call, "tool": "Schedule", "arguments": args}),
        ),
    );
}

fn count_fires(kernel: &Kernel) -> usize {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .filter(|e| e.event_type == ce::WAKE && e.source == "timer")
        .count()
}

#[test]
fn a_one_shot_timer_fires_once() {
    let mut kernel = timer_kernel();
    let wake_rx = kernel.take_wake_receiver().unwrap();
    schedule(
        &kernel,
        "t1",
        json!({"delay_ms": 40, "note": "check the oven"}),
    );

    loop {
        kernel.run_until_quiescent().unwrap();
        if wake_rx.recv_timeout(Duration::from_millis(300)).is_err() {
            break;
        }
    }

    assert_eq!(
        count_fires(&kernel),
        1,
        "a one-shot timer fires exactly once"
    );
    let fire = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::WAKE)
        .unwrap();
    assert_eq!(fire.payload["source"], "timer:1");
    assert_eq!(fire.payload["body"]["note"], "check the oven");
    // Caused by the schedule call that set it — audit answers "why this turn"
    assert!(!fire.causes.is_empty());
    kernel.shutdown();
}

#[test]
fn a_repeating_timer_fires_up_to_max_then_stops() {
    let mut kernel = timer_kernel();
    let wake_rx = kernel.take_wake_receiver().unwrap();
    schedule(
        &kernel,
        "t2",
        json!({"delay_ms": 15, "interval_ms": 15, "max_fires": 3, "note": "poll"}),
    );

    loop {
        kernel.run_until_quiescent().unwrap();
        if wake_rx.recv_timeout(Duration::from_millis(200)).is_err() {
            break;
        }
    }

    assert_eq!(
        count_fires(&kernel),
        3,
        "a repeating timer is bounded by max_fires"
    );
    kernel.shutdown();
}

#[test]
fn unschedule_stops_a_repeating_timer() {
    let mut kernel = timer_kernel();
    let wake_rx = kernel.take_wake_receiver().unwrap();
    // Many fires allowed, but we cancel after the first
    schedule(
        &kernel,
        "t3",
        json!({"delay_ms": 20, "interval_ms": 20, "max_fires": 100}),
    );

    let mut cancelled = false;
    loop {
        kernel.run_until_quiescent().unwrap();
        if count_fires(&kernel) >= 1 && !cancelled {
            kernel.injector("driver").emit(
                "out",
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[],
                    json!({"call": "cancel", "tool": "Unschedule", "arguments": {"timer": 1}}),
                ),
            );
            cancelled = true;
        }
        if wake_rx.recv_timeout(Duration::from_millis(150)).is_err() {
            break;
        }
    }

    let fires = count_fires(&kernel);
    assert!(
        (1..=3).contains(&fires),
        "unschedule must stop the timer well short of max (saw {fires})"
    );
    kernel.shutdown();
}
