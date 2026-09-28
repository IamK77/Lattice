//! The frontend↔core boundary, driven headlessly: prove the two message
//! types carry a whole conversation without any terminal. What renders in a
//! real TUI is exactly what this test drains.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, silent_ui};

mod common;
use common::calc_tools;
use lattice::core_events as ce;
use lattice::{
    render_line, AssemblyManifest, ComponentInstance, ComponentManifest, Factory, Kernel,
    KernelOptions, RenderEvent, Session, Wire,
};

#[test]
fn shutdown_signals_an_active_model_without_waiting_for_its_normal_completion() {
    struct Held {
        entered: std::sync::mpsc::Sender<()>,
        cancelled: std::sync::mpsc::Sender<()>,
    }
    impl lattice::Component for Held {
        fn handle(&mut self, port: &str, event: &lattice::EventEnvelope, ctx: &mut lattice::Ctx) {
            if port != "request" {
                return;
            }
            self.entered.send(()).unwrap();
            let token = ctx.cancellation();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            let result = runtime.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(10), token.cancelled()).await
            });
            assert!(
                result.is_ok(),
                "shutdown did not cancel the active delivery"
            );
            self.cancelled.send(()).unwrap();
            ctx.emit(
                "result",
                lattice::EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&event.id],
                    json!({"status":"cancelled"}),
                ),
            );
        }
    }
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (cancelled_tx, cancelled) = std::sync::mpsc::channel();
    let session = Session::spawn("ui", move |tx| {
        build_with_model(
            tx,
            Box::new(move |_| {
                Box::new(Held {
                    entered: entered_tx.clone(),
                    cancelled: cancelled_tx.clone(),
                })
            }),
        )
    })
    .unwrap();
    session.send_text("hold until cancelled");
    entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    session.request_shutdown();
    cancelled
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("cancellation must bypass the session command queue");
    session.shutdown();
}

#[test]
fn compaction_request_reaches_the_ledger_while_a_foreground_model_is_still_active() {
    struct Held(std::sync::mpsc::Sender<()>);
    impl lattice::Component for Held {
        fn handle(&mut self, port: &str, event: &lattice::EventEnvelope, ctx: &mut lattice::Ctx) {
            if port != "request" {
                return;
            }
            self.0.send(()).unwrap();
            let token = ctx.cancellation();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            let result = runtime.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(10), token.cancelled()).await
            });
            assert!(result.is_ok(), "fixture was not released by shutdown");
            ctx.emit(
                "result",
                lattice::EventDraft::new(
                    ce::MODEL_CALL_COMPLETED,
                    &[&event.id],
                    json!({"status":"cancelled"}),
                ),
            );
        }
    }
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (observed_tx, observed) = std::sync::mpsc::channel();
    let session = Session::spawn("ui", move |tx| {
        let mut kernel =
            build_with_model(tx, Box::new(move |_| Box::new(Held(entered_tx.clone()))))?;
        kernel.subscribe_log(move |event| {
            if event.event_type == ce::EXTERNAL_INPUT
                && event.payload["channel"] == lattice::components::context_gate::COMPACT_CHANNEL
            {
                let _ = observed_tx.send(event.clone());
            }
        });
        Ok(kernel)
    })
    .unwrap();
    session.send_text("hold the ordinary model request");
    entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    session.request_compaction();
    let request = observed.recv_timeout(std::time::Duration::from_secs(5));
    session.request_shutdown();
    session.shutdown();
    let request = request.expect("compaction must bypass the busy session command queue");
    assert_eq!(
        request.payload,
        json!({"channel":lattice::components::context_gate::COMPACT_CHANNEL})
    );
    assert!(request.causes.is_empty());
}

fn build_scripted_kernel(
    render_tx: std::sync::mpsc::Sender<RenderEvent>,
) -> Result<Kernel, lattice::KernelError> {
    build_with_model(
        render_tx,
        Box::new(|config| Box::new(scripted_model::ScriptedModel::from_config(config))),
    )
}

fn build_with_model(
    render_tx: std::sync::mpsc::Sender<RenderEvent>,
    model: Factory,
) -> Result<Kernel, lattice::KernelError> {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
    ]
    .into();
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&displayed)))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(scripted_model::NAME.to_string(), model);
    factories.insert(
        calc_tools::NAME.to_string(),
        Box::new(|c| Box::new(calc_tools::CalcTools::from_config(c))),
    );

    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "t1", "tool": "calc", "arguments": {"numbers": [4, 7]}}]},
        {"status": "ok", "text": "4 + 7 = 11"},
    ]});
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
                "tools".to_string(),
                ComponentInstance {
                    component: calc_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("ui.interrupt", "model.control"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )?;
    // Wire the render channel into the ledger and the notice bypass
    let for_log = render_tx.clone();
    kernel.subscribe_log(move |event| {
        let _ = for_log.send(RenderEvent::Appended(Box::new(event.clone())));
    });
    kernel.set_notice_handler(move |source, payload| {
        let _ = render_tx.send(RenderEvent::Notice {
            source: source.to_string(),
            payload: payload.clone(),
        });
    });
    Ok(kernel)
}

#[test]
fn a_whole_turn_flows_over_the_two_message_types() {
    let session = Session::spawn("ui", build_scripted_kernel).unwrap();
    session.send_text("add 4 and 7");

    // Drain render events until the turn is quiescent
    let mut appended = Vec::new();
    loop {
        match session.next_render() {
            Some(RenderEvent::Appended(e)) => appended.push(*e),
            Some(RenderEvent::Quiescent) => break,
            Some(_) => {}
            None => break,
        }
    }

    // The ledger flowed through, in order, as ordinary data
    let types: Vec<&str> = appended.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec![
            ce::USER_MESSAGE,
            ce::MODEL_CALL_STARTED,
            ce::MODEL_CALL_COMPLETED,
            ce::TOOL_EXEC_STARTED,
            ce::TOOL_EXEC_COMPLETED,
            ce::MODEL_CALL_STARTED,
            ce::MODEL_CALL_COMPLETED,
            ce::OUTPUT_REPLY,
            ce::TURN_COMPLETED,
        ]
    );

    // The frontend's render rule turns them into screen lines
    let lines: Vec<(&str, String)> = appended.iter().filter_map(|e| render_line(e)).collect();
    assert_eq!(lines[0], ("you", "add 4 and 7".to_string()));
    assert!(lines
        .iter()
        .any(|(who, text)| *who == "tool" && text.contains("calc")));
    assert_eq!(lines.last().unwrap(), &("agent", "4 + 7 = 11".to_string()));

    session.shutdown();
}

/// Deleting a model leaves a record, and the record is the ONLY thing that
/// still knows what was deleted: the catalog keeps no backup by decision, so
/// once the file is rewritten the entry is gone from the machine entirely.
///
/// The second half is why the note is spelled out field by field instead of
/// carrying the entry as the file writes it. An entry may hold a key in full,
/// and a ledger cannot take a secret back — so the payload is pinned here, and
/// anything new that appears in it has to be put there on purpose.
#[test]
fn a_deleted_model_leaves_a_record_and_the_record_carries_no_key() {
    let session = Session::spawn("ui", build_scripted_kernel).unwrap();
    session.note_catalog_change(lattice::CatalogNote {
        action: "removed".to_string(),
        id: "spare".to_string(),
        model: "some-model".to_string(),
        adapter: "openai".to_string(),
        endpoint: "api.example.com".to_string(),
        key_env: "SPARE_API_KEY".to_string(),
    });
    // A turn behind it, so that "the record never came" ends this test instead
    // of parking it forever: draining renders is a blocking read, and a note
    // that emits nothing wakes nobody. The turn always reaches quiescence, so
    // the missing event becomes a failed assertion rather than a hang.
    session.send_text("add 4 and 7");

    let mut appended = Vec::new();
    loop {
        match session.next_render() {
            Some(RenderEvent::Appended(e)) => appended.push(*e),
            Some(RenderEvent::Quiescent) => break,
            Some(_) => {}
            None => break,
        }
    }

    let recorded = appended
        .iter()
        .find(|e| e.event_type == silent_ui::CATALOG_CHANGED)
        .expect("the deletion is on the ledger");
    assert_eq!(recorded.source, "ui", "the frontend's own event");
    assert_eq!(recorded.payload["id"], json!("spare"));
    assert_eq!(recorded.payload["action"], json!("removed"));
    assert_eq!(recorded.payload["model"], json!("some-model"));
    assert_eq!(recorded.payload["endpoint"], json!("api.example.com"));
    assert_eq!(
        recorded.payload["keyEnv"],
        json!("SPARE_API_KEY"),
        "the variable's NAME, which is worth nothing to whoever reads it"
    );
    // A decision event, so the kernel refuses it without one
    let reason = recorded.reason.as_deref().expect("decisions say why");
    assert!(reason.contains("spare"), "the reason names it: {reason}");

    let carried: Vec<&String> = recorded
        .payload
        .as_object()
        .expect("an object")
        .keys()
        .collect();
    assert_eq!(
        carried,
        vec!["action", "adapter", "endpoint", "id", "keyEnv", "model"],
        "exactly these — nothing from the entry as the file spells it"
    );

    session.shutdown();
}

/// The frontend's own cost lands on the ledger, once per turn.
///
/// Once per TURN and not once per frame: a frame is not a completed state in
/// the sense this log means, and twenty entries a second would bury the record
/// they exist to make readable. What is worth keeping is the pair that shows
/// whether drawing is getting worse — frames against how long the transcript
/// had grown.
#[test]
fn what_a_turn_cost_to_draw_lands_on_the_ledger() {
    let session = Session::spawn("ui", build_scripted_kernel).unwrap();
    session.note_render_cost(lattice::RenderCostNote {
        frames: 12,
        skipped: 340,
        total_ms: 8.4,
        worst_ms: 1.9,
        entries: 111,
        history: Default::default(),
        memory: Default::default(),
    });
    session.send_text("add 4 and 7");

    let mut appended = Vec::new();
    loop {
        match session.next_render() {
            Some(RenderEvent::Appended(e)) => appended.push(*e),
            Some(RenderEvent::Quiescent) => break,
            Some(_) => {}
            None => break,
        }
    }

    let recorded = appended
        .iter()
        .find(|e| e.event_type == silent_ui::RENDER_COST)
        .expect("the turn's drawing cost is on the ledger");
    assert_eq!(recorded.source, "ui");
    assert_eq!(recorded.payload["frames"], json!(12));
    assert_eq!(
        recorded.payload["skipped"],
        json!(340),
        "the frames NOT built are the point of the measurement"
    );
    assert_eq!(recorded.payload["worstMs"], json!(1.9));
    assert_eq!(
        recorded.payload["entries"],
        json!(111),
        "without it there is nothing to read the cost against"
    );
    // An observation, not a decision — nothing was chosen, so nothing to justify
    assert!(recorded.reason.is_none());

    session.shutdown();
}

#[test]
fn startup_observation_reaches_the_ledger_without_requesting_a_model_turn() {
    let session = Session::spawn("ui", build_scripted_kernel).unwrap();
    let kernel_cost = session.startup_cost().clone();
    for phase in [
        "prepare",
        "log_open",
        "settle",
        "witness_seed",
        "components_start",
        "finalize",
    ] {
        assert!(kernel_cost.phases_ms.contains_key(phase), "missing {phase}");
    }
    let sum: f64 = kernel_cost.phases_ms.values().sum();
    assert!((sum - kernel_cost.total_ms).abs() < 1e-6);
    let note = lattice::session::StartupNote {
        started_at: "2026-09-11T00:00:00.000Z".into(),
        first_frame_at: "2026-09-11T00:00:00.015Z".into(),
        resumed: true,
        history_events: 123,
        frontend: lattice::startup::Timings {
            total_ms: 15.0,
            phases_ms: [("first_draw".to_string(), 15.0)].into(),
            memory: Default::default(),
        },
        kernel: kernel_cost,
        history: Default::default(),
        replay_memory: Default::default(),
        ui_counts: Default::default(),
    };
    let mut expected = serde_json::to_value(&note).unwrap();
    let history_stats = serde_json::to_value(session.log_reader().memory_stats().unwrap()).unwrap();
    let round = serde_json::from_value(expected.clone()).unwrap();
    session.note_startup(round);
    let mut appended = Vec::new();
    while let Some(event) = session.next_render() {
        match event {
            RenderEvent::Appended(e) => appended.push(*e),
            RenderEvent::Quiescent => break,
            _ => {}
        }
    }
    session.shutdown();
    let recorded: Vec<_> = appended
        .iter()
        .filter(|e| e.event_type == silent_ui::STARTUP_COST)
        .collect();
    assert_eq!(recorded.len(), 1, "one completed startup observation");
    assert_eq!(recorded[0].source, "ui");
    let history = &recorded[0].payload["history"];
    assert_eq!(history["stats"], history_stats);
    chrono::DateTime::parse_from_rfc3339(history["at"].as_str().unwrap()).unwrap();
    expected["history"] = history.clone();
    assert_eq!(recorded[0].payload, expected);
    assert!(recorded[0].reason.is_none());
    assert!(!appended
        .iter()
        .any(|e| e.event_type == ce::MODEL_CALL_STARTED));
    assert!(!appended.iter().any(|e| e.event_type == ce::ERROR));
}

#[test]
fn the_boundary_messages_are_pure_data() {
    // If they serialize, they can flow over a socket unchanged — the daemon
    // is a transport swap, not a rewrite
    let cmd = lattice::FrontendCommand::SendText { text: "hi".into() };
    let round: lattice::FrontendCommand =
        serde_json::from_str(&serde_json::to_string(&cmd).unwrap()).unwrap();
    assert!(matches!(round, lattice::FrontendCommand::SendText { .. }));

    let ev = RenderEvent::Notice {
        source: "model".into(),
        payload: json!({"chunk": "hi"}),
    };
    let round: RenderEvent = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
    assert!(matches!(round, RenderEvent::Notice { .. }));
}
