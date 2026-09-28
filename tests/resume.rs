//! Durable resume: the ledger is the state, so a restart must cost nothing.
//!
//! Two halves, mirroring the recorded decision ("流水即状态"): reopening a
//! ledger SETTLES chains severed mid-flight (recovery speaks, never re-does),
//! and the loop REBUILDS its material pointers from the record — the
//! conversation's memory coming back without any saved component state.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire, KERNEL_SOURCE,
};

struct Noop;
impl Component for Noop {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}

fn options(file: &Path) -> KernelOptions {
    KernelOptions {
        stream: Some("main".to_string()),
        log_file: Some(file.to_path_buf()),
        ..KernelOptions::default()
    }
}

#[test]
fn core_resume_does_not_copy_historical_payloads_for_bookkeeping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let mut log =
        lattice::EventLog::open(ce::core_event_decls(), "main", Some(path.clone())).unwrap();
    for _ in 0..8 {
        log.append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text": "x".repeat(128 * 1024)}),
            ),
            "ui",
        )
        .unwrap();
    }
    drop(log);
    let kernel = Kernel::start(
        &AssemblyManifest {
            instances: Default::default(),
            wires: vec![],
        },
        &Default::default(),
        &mut Default::default(),
        options(&path),
    )
    .unwrap();
    assert_eq!(
        kernel
            .log()
            .cost()
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "recording resume, finding unfinished chains, and seeding witnesses need no payload copies"
    );
    assert!(kernel
        .log()
        .reader()
        .any(|event| { event.event_type == ce::STREAM_RESUMED && event.payload["fromSeq"] == 8 })
        .unwrap());
    kernel.shutdown();
}

#[test]
fn restoring_components_do_not_copy_unrelated_history() {
    use lattice::components::{fs_watch, shell_tools, subagent, timer_tools};
    let cases: Vec<(ComponentManifest, Factory)> = vec![
        (
            shell_tools::manifest(),
            Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
        ),
        (
            timer_tools::manifest(),
            Box::new(|c| Box::new(timer_tools::TimerTools::from_config(c))),
        ),
        (
            fs_watch::manifest(),
            Box::new(|c| Box::new(fs_watch::FsWatch::from_config(c))),
        ),
        (
            subagent::manifest(),
            Box::new(|c| Box::new(subagent::Subagent::from_config(c))),
        ),
    ];
    for (manifest, factory) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut log =
            lattice::EventLog::open(ce::core_event_decls(), "main", Some(path.clone())).unwrap();
        let large = "x".repeat(128 * 1024);
        log.append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": large})),
            "ui",
        )
        .unwrap();
        log.append(
            EventDraft::new(
                ce::WAKE,
                &[],
                json!({
                    "source": "other", "summary": "unrelated wake", "body": {"text": large}
                }),
            ),
            "other",
        )
        .unwrap();
        let request = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[],
                    json!({
                        "call": "other", "tool": "Other", "arguments": {"text": large}
                    }),
                ),
                "other",
            )
            .unwrap();
        log.append(
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[&request.id],
                json!({
                    "call": "other", "status": "ok", "result": {"text": large}
                }),
            ),
            "other",
        )
        .unwrap();
        drop(log);
        let name = manifest.name.clone();
        let (mut registry, mut factories, mut assembly) = mute_tool_setup();
        let port = manifest
            .inputs
            .iter()
            .find(|port| port.events.contains(&ce::TOOL_EXEC_STARTED.to_string()))
            .unwrap()
            .name
            .clone();
        registry.insert(name.clone(), manifest);
        factories.insert(name.clone(), factory);
        assembly.instances.insert(
            "restoring".into(),
            ComponentInstance {
                component: name.clone(),
                config: None,
                requires: vec![],
            },
        );
        assembly
            .wires
            .push(Wire::new("driver.out", &format!("restoring.{port}")));
        let kernel = Kernel::start(&assembly, &registry, &mut factories, options(&path)).unwrap();
        let copied = kernel
            .log()
            .cost()
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed);
        kernel.shutdown();
        assert_eq!(
            copied, 0,
            "{name} must not copy unrelated payloads during restore"
        );
    }
}

/// driver → mute tool: records a start, never a completion — the recipe for
/// a hanging chain.
fn mute_tool_setup() -> (
    HashMap<String, ComponentManifest>,
    HashMap<String, Factory>,
    AssemblyManifest,
) {
    let driver = ComponentManifest {
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
    let mute = ComponentManifest {
        name: "mute-tool".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:mute-tool".to_string(),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
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
        ("driver".to_string(), driver),
        ("mute-tool".to_string(), mute),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    factories.insert("mute-tool".to_string(), Box::new(|_| Box::new(Noop)));
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
                "tool".to_string(),
                ComponentInstance {
                    component: "mute-tool".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "tool.execute")],
    };
    (registry, factories, assembly)
}

/// driver → gate → tool. The gate FORWARDS by appending its own copy of the
/// request, which is what every gate in the product assembly does, and the
/// tool answers the copy.
fn gated_setup(
    answer: bool,
) -> (
    HashMap<String, ComponentManifest>,
    HashMap<String, Factory>,
    AssemblyManifest,
) {
    fn manifest(name: &str, input: Option<&str>, output: (&str, &str)) -> ComponentManifest {
        ComponentManifest {
            name: name.to_string(),
            version: "0".to_string(),
            runtime: RuntimeKind::Inproc,
            entry: format!("builtin:{name}"),
            inputs: input
                .map(|port| vec![PortDecl::new(port, &[ce::TOOL_EXEC_STARTED])])
                .unwrap_or_default(),
            outputs: vec![PortDecl::new(output.0, &[output.1])],
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
    struct Relay;
    impl Component for Relay {
        fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
            ctx.emit(
                "forward",
                EventDraft::new(ce::TOOL_EXEC_STARTED, &[&event.id], event.payload.clone()),
            );
        }
    }
    struct Answering;
    impl Component for Answering {
        fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
            ctx.emit(
                "outcome",
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[&event.id],
                    json!({"call": event.payload["call"], "status": "ok", "result": "done"}),
                ),
            );
        }
    }

    let registry: HashMap<String, ComponentManifest> = [
        (
            "driver".to_string(),
            manifest("driver", None, ("out", ce::TOOL_EXEC_STARTED)),
        ),
        (
            "gate".to_string(),
            manifest("gate", Some("in"), ("forward", ce::TOOL_EXEC_STARTED)),
        ),
        (
            "mute-tool".to_string(),
            manifest(
                "mute-tool",
                Some("execute"),
                ("outcome", ce::TOOL_EXEC_COMPLETED),
            ),
        ),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Noop)));
    factories.insert("gate".to_string(), Box::new(|_| Box::new(Relay)));
    factories.insert(
        "mute-tool".to_string(),
        Box::new(move |_| {
            if answer {
                Box::new(Answering) as Box<dyn Component>
            } else {
                Box::new(Noop) as Box<dyn Component>
            }
        }),
    );
    let instance = |component: &str| ComponentInstance {
        component: component.to_string(),
        requires: Vec::new(),
        config: None,
    };
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), instance("driver")),
            ("gate".to_string(), instance("gate")),
            ("tool".to_string(), instance("mute-tool")),
        ]
        .into(),
        wires: vec![
            Wire::new("driver.out", "gate.in"),
            Wire::new("gate.forward", "tool.execute"),
        ],
    };
    (registry, factories, assembly)
}

/// A call that a gate forwarded and the tool ANSWERED is finished, and
/// reopening must say nothing about it.
///
/// The outcome answers the gate's copy of the request, not the original, so
/// asking whether anything ends the original reads every relayed call as
/// hanging. In the product assembly a gate sits on both the model wire and
/// the tool wire, so that was every call: resuming a real 316-event
/// conversation appended 97 interruptions, all false, and another set on
/// every later resume. The ledger is the audit record — it may not invent
/// interruptions that did not happen.
/// Every stream says what it is, as its own first event.
///
/// A file header would have been the obvious place and is wrong here: an
/// event id carries its own line number (`ev_42_…` is line 42), which is the
/// address the agent is given for reaching one event, so a header line would
/// put every event one line off its own id.
#[test]
fn a_stream_opens_by_saying_what_it_is_and_resuming_says_where_it_left_off() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.jsonl");

    let (registry, mut factories, assembly) = mute_tool_setup();
    let mut first_life = options(&file);
    first_life.stream_note = Some(json!({"host": "test-host", "model": "scripted"}));
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, first_life).unwrap();
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "mute", "arguments": {}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let opened = kernel.log().replay(1).unwrap()[0].clone();
    kernel.shutdown();

    assert_eq!(opened.seq, 1, "it is the FIRST line, so line == seq holds");
    assert_eq!(opened.event_type, ce::STREAM_OPENED);
    assert_eq!(opened.source, KERNEL_SOURCE);
    assert!(opened.causes.is_empty());
    assert!(
        !opened.payload["lattice"].as_str().unwrap().is_empty(),
        "the runtime that opened it"
    );
    assert_eq!(
        opened.payload["instances"]["tool"], "mute-tool",
        "the assembly it opened under: {}",
        opened.payload["instances"]
    );
    // What only the host knows, merged in — the kernel has no notion of hosts
    assert_eq!(opened.payload["host"], "test-host");
    assert_eq!(opened.payload["model"], "scripted");

    // Life two: the same ledger, picked up again
    let (registry, mut factories, assembly) = mute_tool_setup();
    let kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == ce::STREAM_OPENED)
            .count(),
        1,
        "a stream opens once, however many processes carry it"
    );
    let resumed = events
        .iter()
        .find(|e| e.event_type == ce::STREAM_RESUMED)
        .expect("the second life said so");
    assert_eq!(
        resumed.payload["fromSeq"], 2,
        "where the previous life stopped — the opening plus the one hanging          request it left: {resumed:?}"
    );
    // Before the settling, so the ledger reads in the order things happened:
    // this process arrived, THEN it cleared what the last one left in flight
    let settle = events
        .iter()
        .find(|e| e.event_type == ce::INTERRUPTED)
        .expect("the hanging chain was settled");
    assert!(
        resumed.seq < settle.seq,
        "arrival is recorded before the clearing it caused"
    );
}

/// The driver's own request — the head of the chain, found by source rather
/// than by position, because every ledger begins with `core.stream.opened`.
fn request_head(kernel: &Kernel) -> String {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "driver")
        .expect("the driver's request is on the ledger")
        .id
}

#[test]
fn reopening_says_nothing_about_a_call_a_gate_forwarded_and_the_tool_answered() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.jsonl");

    let (registry, mut factories, assembly) = gated_setup(true);
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "mute", "arguments": {}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let before = kernel.log().replay(1).unwrap();
    assert_eq!(
        before
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED)
            .count(),
        2,
        "the request is on the ledger twice: the driver's and the gate's copy"
    );
    kernel.shutdown();

    let (registry, mut factories, assembly) = gated_setup(true);
    let kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    let settles: Vec<EventEnvelope> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::INTERRUPTED)
        .collect();
    assert!(
        settles.is_empty(),
        "nothing hung — the tool answered the gate's copy: {settles:?}"
    );
    kernel.shutdown();
}

/// The other half: a relayed call that really DID hang is settled, once for
/// the chain rather than once per copy — two interruptions would be two
/// answers to one call, which no wire format accepts.
#[test]
fn a_relayed_chain_that_really_hung_is_settled_once_for_the_whole_chain() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.jsonl");

    let (registry, mut factories, assembly) = gated_setup(false);
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "mute", "arguments": {}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let head = request_head(&kernel);
    kernel.shutdown();

    let (registry, mut factories, assembly) = gated_setup(false);
    let kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    let settles: Vec<EventEnvelope> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::INTERRUPTED)
        .collect();
    assert_eq!(settles.len(), 1, "one chain, one settle: {settles:?}");
    assert_eq!(
        settles[0].causes,
        vec![head],
        "settled at the head of the chain"
    );
    kernel.shutdown();
}

#[test]
fn reopening_settles_hanging_chains_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.jsonl");

    // Life one: a tool call starts and never completes
    let (registry, mut factories, assembly) = mute_tool_setup();
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "mute", "arguments": {}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let started_id = request_head(&kernel);
    kernel.shutdown();

    // Life two: reopening settles the severed chain — an interrupt from the
    // kernel, cause pointing at the hanging start, with a reason
    let (registry, mut factories, assembly) = mute_tool_setup();
    let kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    let settles: Vec<EventEnvelope> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::INTERRUPTED)
        .collect();
    assert_eq!(settles.len(), 1);
    assert_eq!(settles[0].source, KERNEL_SOURCE);
    assert_eq!(settles[0].payload["by"], "restart");
    assert_eq!(settles[0].causes, vec![started_id]);
    assert!(
        settles[0].reason.is_some(),
        "a settle is a decision — it says why"
    );
    kernel.shutdown();

    // Life three — the call-it-again probe: a settled chain stays settled
    let (registry, mut factories, assembly) = mute_tool_setup();
    let kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    let settles = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .filter(|e| e.event_type == ce::INTERRUPTED)
        .count();
    assert_eq!(settles, 1, "settling must be idempotent across reopenings");
    kernel.shutdown();
}

/// ui + loop + scripted model, persisted to `file`, with the given script.
fn chat_setup(
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
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    (registry, factories, assembly)
}

#[test]
fn first_input_after_long_resume_crosses_the_gate_without_reading_obsolete_requests() {
    long_resume_first_input(false);
}

#[test]
fn first_input_reports_an_unreadable_required_summary_measurement_request() {
    long_resume_first_input(true);
}

fn long_resume_first_input(break_required_request: bool) {
    use lattice::components::context_gate;
    use std::io::Write;
    use std::os::unix::fs::FileExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.jsonl");
    let mut declarations = ce::core_event_decls();
    declarations.extend(context_gate::manifest().events);
    let mut source = lattice::EventLog::in_memory(declarations, "main");
    let mut disk = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let mut offset = 0u64;
    let mut requests = Vec::new();
    let mut covers = Vec::new();
    let mut needed_request = 0;
    let mut side_replies = Vec::new();
    for n in 0..1152 {
        let side = n >= 1024;
        let mut payload = json!({"model":"scripted", "input":{"parts":[], "fingerprint":"sha256:test"}, "tools":[]});
        if side {
            payload["purpose"] = json!("fixture.side");
        }
        let request = source
            .append(
                EventDraft::new(ce::MODEL_CALL_STARTED, &[], payload),
                "loop",
            )
            .unwrap();
        if n == 1023 {
            needed_request = offset;
        }
        requests.push(offset);
        let reply = source
            .append(
                EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&request.id],
                    json!({"status":"ok", "text":"old reply", "usage":{"input_tokens":1}}),
                ),
                "model",
            )
            .unwrap();
        if !side {
            covers.push(reply.id.clone());
        }
        for event in [request, reply] {
            if side && event.event_type == ce::MODEL_CALL_COMPLETED {
                side_replies.push(offset);
            }
            let line = serde_json::to_vec(&event).unwrap();
            disk.write_all(&line).unwrap();
            disk.write_all(b"\n").unwrap();
            offset += line.len() as u64 + 1;
        }
    }
    let summary = source
        .append(
            EventDraft::new(
                context_gate::SUMMARY,
                &[],
                json!({"covers":covers, "text":"retained summary"}),
            )
            .with_reason("fixture summary"),
            "ctx",
        )
        .unwrap();
    serde_json::to_writer(&mut disk, &summary).unwrap();
    disk.write_all(b"\n").unwrap();
    disk.flush().unwrap();
    drop(disk);
    drop(source);

    let (mut registry, mut factories, mut assembly) =
        chat_setup(json!({"script":[{"status":"ok", "text":"resumed reply"}]}));
    registry.insert(context_gate::NAME.into(), context_gate::manifest());
    factories.insert(
        context_gate::NAME.into(),
        Box::new(|config| Box::new(context_gate::ContextGate::from_config(config))),
    );
    assembly.instances.insert(
        "ctx".into(),
        ComponentInstance {
            component: context_gate::NAME.into(),
            requires: vec![],
            config: Some(json!({"contextWindow":128})),
        },
    );
    assembly.wires = vec![
        Wire::new("ui.user", "loop.input"),
        Wire::new("loop.ask", "ctx.ask"),
        Wire::new("ctx.forward", "model.request"),
        Wire::new("model.result", "loop.model"),
        Wire::new("loop.out", "ui.display"),
    ];
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&path)).unwrap();
    let disk = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    // Only the latest MAIN request is needed to check summary measurement.
    // Later side-channel completions and their requests must also stay unread.
    for offset in requests
        .into_iter()
        .chain(side_replies)
        .filter(|offset| break_required_request || *offset != needed_request)
    {
        disk.write_all_at(b"!", offset).unwrap();
    }
    let through = kernel.log().reader().snapshot_end();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"first input"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(through + 1).unwrap();
    if break_required_request {
        assert!(events
            .iter()
            .any(|e| e.payload["code"] == "core.component_failed"
                && e.payload["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("committed ledger bytes changed"))));
        assert!(!events
            .iter()
            .any(|e| e.source == "ctx" && e.event_type == ce::MODEL_CALL_STARTED));
        assert!(!events
            .iter()
            .any(|e| e.event_type == ce::MODEL_CALL_COMPLETED));
        // The loop may report the interruption to the UI, but must not
        // disguise it as a normal model reply.
        let replies: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == ce::OUTPUT_REPLY)
            .collect();
        assert!(
            replies
                .iter()
                .all(|e| e.payload["cancelled"] == true && e.payload["text"].is_null()),
            "failed preparation may only publish cancellation: {replies:?}"
        );
        kernel.shutdown();
        return;
    }
    assert!(!events.iter().any(|e| e.event_type == ce::ERROR));
    let forwarded = events
        .iter()
        .find(|e| e.source == "ctx" && e.event_type == ce::MODEL_CALL_STARTED)
        .unwrap();
    let parts = forwarded.payload["input"]["parts"].as_array().unwrap();
    assert_eq!(
        parts.len(),
        2,
        "one summary and the current input, not the old body list"
    );
    assert!(parts.iter().any(|p| p["digest"]["of"] == summary.id));
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "resumed reply"));
    assert!(events.iter().any(|e| e.event_type == ce::TURN_COMPLETED));
    kernel.shutdown();
}

#[test]
fn the_conversations_memory_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.jsonl");

    // Life one: a whole turn, properly finished, then shutdown
    let (registry, mut factories, assembly) =
        chat_setup(json!({"script": [{"status": "ok", "text": "reply one"}]}));
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "first thing"})),
    );
    kernel.run_until_quiescent().unwrap();
    let life_one: Vec<EventEnvelope> = kernel.log().replay(1).unwrap();
    let user_one = life_one
        .iter()
        .find(|e| e.event_type == ce::USER_MESSAGE)
        .unwrap()
        .id
        .clone();
    let completed_one = life_one
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
        .unwrap()
        .id
        .clone();
    kernel.shutdown();

    // Life two: a FRESH loop instance (empty memory) on the same ledger
    let (registry, mut factories, assembly) =
        chat_setup(json!({"script": [{"status": "ok", "text": "reply two"}]}));
    let mut kernel = Kernel::start(&assembly, &registry, &mut factories, options(&file)).unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "second thing"})),
    );
    kernel.run_until_quiescent().unwrap();

    // The new turn's model call must carry life one's material — the memory
    // came back from the ledger, not from any saved component state
    let events = kernel.log().replay(1).unwrap();
    let second_ask = events
        .iter()
        .rev()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .unwrap();
    let parts: Vec<&str> = second_ask.payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["event"].as_str())
        .collect();
    assert!(
        parts.contains(&user_one.as_str()),
        "life one's user message must be in the material: {parts:?}"
    );
    assert!(
        parts.contains(&completed_one.as_str()),
        "life one's reply must be in the material: {parts:?}"
    );
    // And the turn actually completed with the new script
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "reply two"));
    kernel.shutdown();
}
