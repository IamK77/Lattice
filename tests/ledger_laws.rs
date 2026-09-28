//! What must be true of the ledger, whatever happened in it.
//!
//! The rules checked here are each guarded somewhere by a hand-built scenario:
//! a call that hangs, a call the kernel settles at restart, an event whose
//! cause does not exist. What no hand-built scenario reaches is the ninth
//! COMBINATION — a restart in the middle of a parallel tool turn, a foreign
//! tool going unanswered while another finishes, the same shape twice in a
//! row. A generator reaches those, and the rules are cheap enough to check on
//! everything it produces.
//!
//! The assembly here has no gate on purpose. A gate re-sends a request, so a
//! request and its ending are no longer one to one, and "exactly one ending"
//! would have to be asked along the whole forwarding chain. That question has
//! its own tests; this file asks the simpler one, and asks it of everything.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, ComponentManifest, EventDraft, EventEnvelope, Factory,
    Kernel, KernelOptions, Wire, ENVELOPE_VERSION,
};

mod common;

/// xorshift64*, seeded and fixed: a failure names a seed, and that seed brings
/// it back. See tests/materialize_invariants.rs for the same generator — they
/// are separate binaries and a shared helper would have to live in the crate.
struct Seeded(u64);

impl Seeded {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn below(&mut self, n: u64) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
    }
}

fn options(file: &Path) -> KernelOptions {
    KernelOptions {
        stream: Some("main".to_string()),
        log_file: Some(file.to_path_buf()),
        ..KernelOptions::default()
    }
}

/// ui → loop → model, with a tool runner that stays silent on tools it does
/// not provide. Silence is the fan-out convention, and it is also how a
/// request ends up with no answer at all — the state a restart has to settle.
fn setup(
    script: Value,
) -> (
    HashMap<String, ComponentManifest>,
    HashMap<String, Factory>,
    AssemblyManifest,
) {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (
            common::calc_tools::NAME.to_string(),
            common::calc_tools::manifest(),
        ),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        common::calc_tools::NAME.to_string(),
        Box::new(|c| Box::new(common::calc_tools::CalcTools::from_config(c))),
    );

    let instance = |component: &str, config: Option<Value>| ComponentInstance {
        component: component.to_string(),
        requires: Vec::new(),
        config,
    };
    let assembly = AssemblyManifest {
        instances: [
            ("ui".to_string(), instance(silent_ui::NAME, None)),
            ("loop".to_string(), instance(minimal_loop::NAME, None)),
            (
                "model".to_string(),
                instance(scripted_model::NAME, Some(script)),
            ),
            (
                "tools".to_string(),
                instance(common::calc_tools::NAME, Some(json!({"exclusive": false}))),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    (registry, factories, assembly)
}

/// One scripted model turn. The awkward ones are deliberate: a tool nobody
/// provides is never answered, and a turn left in flight at shutdown is the
/// state restarting has to clean up.
fn scripted_turn(rng: &mut Seeded) -> Value {
    match rng.below(5) {
        0 => json!({"status": "ok", "text": "nothing to do"}),
        1 => json!({"status": "ok", "toolCalls": [
            {"id": "c1", "tool": "calc", "arguments": {"numbers": [1, 2]}}
        ]}),
        2 => json!({"status": "ok", "toolCalls": [
            {"id": "p1", "tool": "calc", "arguments": {"numbers": [1]}},
            {"id": "p2", "tool": "calc", "arguments": {"numbers": [2]}}
        ]}),
        // Nobody provides this one, and the silent runner says nothing about
        // it — the chain hangs until something settles it.
        3 => json!({"status": "ok", "toolCalls": [
            {"id": "h1", "tool": "nobody-has-this", "arguments": {}}
        ]}),
        // One that answers and one that never will, in the same turn.
        _ => json!({"status": "ok", "toolCalls": [
            {"id": "m1", "tool": "calc", "arguments": {"numbers": [3]}},
            {"id": "m2", "tool": "nobody-has-this", "arguments": {}}
        ]}),
    }
}

/// Run `turns` generated turns, shut down, and reopen — reopening is what
/// settles whatever was left hanging. The events returned are the whole
/// ledger as the second life sees it.
fn run_and_reopen(seed: u64, turns: usize) -> Vec<EventEnvelope> {
    let mut rng = Seeded::new(seed);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.jsonl");

    let script: Vec<Value> = (0..turns).map(|_| scripted_turn(&mut rng)).collect();
    let (registry, mut factories, assembly) = setup(json!({ "script": script }));
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    for n in 0..turns {
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({ "text": format!("turn {n}") }),
            ),
        );
        kernel.run_until_quiescent().unwrap();
    }
    kernel.shutdown();

    // Life two on the same ledger: opening settles every chain life one left
    // in flight, which is what makes "exactly one ending" answerable at all.
    let (registry, mut factories, assembly) = setup(json!({"script": []}));
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    events
}

/// Every ending of the request `started`, by either name. A settlement points
/// back at the request that hung; a result names the call it answers and is
/// caused by the request too.
fn endings<'a>(events: &'a [EventEnvelope], started: &EventEnvelope) -> Vec<&'a EventEnvelope> {
    events
        .iter()
        .filter(|e| {
            matches!(
                e.event_type.as_str(),
                ce::TOOL_EXEC_COMPLETED | ce::INTERRUPTED
            ) && e.causes.contains(&started.id)
        })
        .collect()
}

const SEEDS: [u64; 12] = [
    1, 7, 19, 42, 97, 128, 333, 1009, 4096, 12345, 65537, 999_983,
];

#[test]
fn every_event_is_numbered_in_order_with_no_gaps() {
    for seed in SEEDS {
        let events = run_and_reopen(seed, 4);
        for (at, event) in events.iter().enumerate() {
            assert_eq!(
                event.seq,
                at as u64 + 1,
                "seed {seed}: numbering jumps at {}",
                event.id
            );
            assert_eq!(event.v, ENVELOPE_VERSION, "seed {seed}: {}", event.id);
            assert_eq!(event.stream, "main", "seed {seed}: {}", event.id);
        }
    }
}

/// Tracing back is a walk up the ancestor graph. It terminates only because
/// every cause is older than the event naming it — a cause pointing forward,
/// or at itself, would be a cycle nobody notices until a trace hangs.
#[test]
fn every_cause_is_older_than_the_event_that_names_it() {
    for seed in SEEDS {
        let events = run_and_reopen(seed, 4);
        let seq_of: HashMap<&str, u64> = events.iter().map(|e| (e.id.as_str(), e.seq)).collect();
        for event in &events {
            for cause in &event.causes {
                let at = seq_of.get(cause.as_str()).unwrap_or_else(|| {
                    panic!("seed {seed}: {} names a cause that is not here", event.id)
                });
                assert!(
                    *at < event.seq,
                    "seed {seed}: {} (#{}) names #{at} as a cause",
                    event.id,
                    event.seq
                );
            }
        }
    }
}

/// The rule the contract states twice over: a call always ends, and ends once.
/// Asked after a reopen, because that is when the kernel settles whatever was
/// still in flight — before it, "not yet" is a legitimate answer.
#[test]
fn every_request_ends_exactly_once() {
    for seed in SEEDS {
        let events = run_and_reopen(seed, 4);
        let started: Vec<&EventEnvelope> = events
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED)
            .collect();
        assert!(
            !started.is_empty(),
            "seed {seed}: no tool was ever asked for — the generator is not \
             generating anything and this whole file proves nothing"
        );
        for request in started {
            let ends = endings(&events, request);
            assert_eq!(
                ends.len(),
                1,
                "seed {seed}: {} ({}) has {} endings: {:?}",
                request.id,
                request.payload["tool"],
                ends.len(),
                ends.iter().map(|e| &e.event_type).collect::<Vec<_>>()
            );
        }
    }
}

/// A decision without a reason is a decision nobody can audit later. The log
/// enforces this at its entry point; this asks whether anything got in anyway.
#[test]
fn every_decision_says_why() {
    let decisions: Vec<String> = lattice::core_events::core_event_decls()
        .into_iter()
        .filter(|d| d.decision)
        .map(|d| d.event_type)
        .collect();
    for seed in SEEDS {
        for event in run_and_reopen(seed, 4) {
            if decisions.contains(&event.event_type) {
                let reason = event.reason.as_deref().unwrap_or("");
                assert!(
                    !reason.trim().is_empty(),
                    "seed {seed}: {} is a decision with no reason",
                    event.id
                );
            }
        }
    }
}

/// Reopening a settled ledger must add nothing. The first reopen settles what
/// hung; a second one is being asked the same question again, and answering it
/// twice would write a second ending for calls that already have one.
#[test]
fn reopening_an_already_settled_ledger_adds_nothing() {
    for seed in SEEDS {
        let mut rng = Seeded::new(seed);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.jsonl");
        let script: Vec<Value> = (0..4).map(|_| scripted_turn(&mut rng)).collect();

        let (registry, mut factories, assembly) = setup(json!({ "script": script }));
        let mut kernel =
            Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
        for n in 0..4 {
            kernel.injector("ui").emit(
                "user",
                EventDraft::new(
                    ce::USER_MESSAGE,
                    &[],
                    json!({ "text": format!("turn {n}") }),
                ),
            );
            kernel.run_until_quiescent().unwrap();
        }
        kernel.shutdown();

        // Each open legitimately records that the stream resumed, so the
        // ledger is one event longer every time. What must NOT grow is the
        // number of endings: settling a chain is a one-time repair, and doing
        // it again would write a second ending for a call that has one.
        let mut settled = Vec::new();
        let mut added = Vec::new();
        for _ in 0..3 {
            let (registry, mut factories, assembly) = setup(json!({"script": []}));
            let mut kernel =
                Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
            kernel.run_until_quiescent().unwrap();
            let events = kernel.log().replay(1).unwrap();
            settled.push(
                events
                    .iter()
                    .filter(|e| e.event_type == ce::INTERRUPTED)
                    .count(),
            );
            added.push(
                events
                    .iter()
                    .filter(|e| e.event_type != ce::STREAM_RESUMED)
                    .count(),
            );
            kernel.shutdown();
        }
        assert_eq!(
            settled[0], settled[2],
            "seed {seed}: reopening keeps settling chains that are already \
             settled — a second ending for a call that has one: {settled:?}"
        );
        assert_eq!(
            added[0], added[2],
            "seed {seed}: reopening writes more than the note that it reopened: \
             {added:?}"
        );
    }
}
