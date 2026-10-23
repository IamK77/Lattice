use super::*;

#[path = "handoff_edges.rs"]
mod edges;

struct SourceTools;

impl lattice::Component for SourceTools {
    fn handle(&mut self, _: &str, event: &lattice::EventEnvelope, ctx: &mut lattice::Ctx) {
        ctx.emit("outcome", EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], json!({
            "call": event.payload["call"], "status": "ok",
            "result": if event.payload["call"] == "source" { "source material\n".repeat(1024) } else { "small result".into() }
        })));
    }
}

fn fixture(log: Option<&std::path::Path>) -> Kernel {
    fixture_with_delivery(log, true)
}

fn fixture_with_delivery(log: Option<&std::path::Path>, deliver_summary: bool) -> Kernel {
    fixture_with_compactor(log, deliver_summary, false)
}

struct Compactor {
    crash: bool,
}
impl lattice::Component for Compactor {
    fn handle(&mut self, port: &str, _: &lattice::EventEnvelope, _: &mut lattice::Ctx) {
        assert!(
            !(self.crash && port == "request"),
            "synthetic compactor crash"
        );
    }
}

// This test transport can delay a completed failure's delivery while its
// ledger record is already visible. The real gate is still the subject.
struct DeferredGate {
    inner: context_gate::ContextGate,
    delay_outcomes: bool,
    buffered: Vec<lattice::EventEnvelope>,
}
impl lattice::Component for DeferredGate {
    fn restore(&mut self, ctx: &mut lattice::Ctx) {
        lattice::Component::restore(&mut self.inner, ctx);
    }
    fn handle(&mut self, port: &str, event: &lattice::EventEnvelope, ctx: &mut lattice::Ctx) {
        if event.payload["channel"] == "test.release-handoff" {
            for held in std::mem::take(&mut self.buffered) {
                lattice::Component::handle(&mut self.inner, "handoff", &held, ctx);
            }
        } else if self.delay_outcomes
            && port == "handoff"
            && event.event_type == context_gate::DECISION
            && matches!(
                event.payload["action"].as_str(),
                Some("suspend" | "compact_skipped")
            )
        {
            self.buffered.push(event.clone());
        } else {
            lattice::Component::handle(&mut self.inner, port, event, ctx);
        }
    }
}

fn fixture_with_compactor(
    log: Option<&std::path::Path>,
    deliver_summary: bool,
    crash: bool,
) -> Kernel {
    fixture_controlled(log, deliver_summary, crash, false)
}

fn fixture_controlled(
    log: Option<&std::path::Path>,
    deliver_summary: bool,
    crash: bool,
    delay_outcomes: bool,
) -> Kernel {
    let mut registry: HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.into(), silent_ui::manifest()),
        (minimal_loop::NAME.into(), minimal_loop::manifest()),
        (context_gate::NAME.into(), context_gate::manifest()),
        (scripted_model::NAME.into(), scripted_model::manifest()),
    ]
    .into();
    registry.insert(calc_tools::NAME.into(), calc_tools::manifest());
    registry.insert(
        MUTE_MODEL.into(),
        lattice::ComponentManifest {
            name: MUTE_MODEL.into(),
            entry: format!("builtin:{MUTE_MODEL}"),
            ..scripted_model::manifest()
        },
    );
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        minimal_loop::NAME.into(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        context_gate::NAME.into(),
        Box::new(move |c| {
            Box::new(DeferredGate {
                inner: context_gate::ContextGate::from_config(c),
                delay_outcomes,
                buffered: Vec::new(),
            })
        }),
    );
    factories.insert(
        scripted_model::NAME.into(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(calc_tools::NAME.into(), Box::new(|_| Box::new(SourceTools)));
    factories.insert(
        MUTE_MODEL.into(),
        Box::new(move |_| Box::new(Compactor { crash })),
    );
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
    );
    let mut script = Vec::new();
    for (call, usage) in [("source", 100), ("small-1", 100), ("small-2", 900)] {
        script.push(json!({"status":"ok", "usage":{"input_tokens":usage},
            "toolCalls":[{"id":call,"tool":"calc","arguments":{}}]}));
    }
    script.push(json!({"status":"ok","text":"resumed","usage":{"input_tokens":100}}));
    script.push(json!({"status":"ok","text":"next input","usage":{"input_tokens":100}}));
    let mut assembly = AssemblyManifest {
        instances: [
            ("ui", silent_ui::NAME, None),
            ("loop", minimal_loop::NAME, None),
            (
                "gate",
                context_gate::NAME,
                Some(json!({
                    "profile":{"contextWindow":1000},"ratio":0.5,"condense":true,
                    "keepRecentParts":3,"keepRecentTools":0,"minCondense":3
                })),
            ),
            (
                "model",
                scripted_model::NAME,
                Some(json!({"script":script})),
            ),
            ("cmodel", MUTE_MODEL, None),
            ("tools", calc_tools::NAME, None),
        ]
        .into_iter()
        .map(|(id, component, config)| {
            (
                id.into(),
                ComponentInstance {
                    component: component.into(),
                    requires: vec![],
                    config,
                },
            )
        })
        .collect(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("ui.answer", "gate.dial"),
            Wire::new("loop.ask", "gate.ask"),
            Wire::new("gate.forward", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("gate.condense", "cmodel.request"),
            Wire::new("cmodel.result", "gate.condensed"),
            Wire::new("gate.summary", "gate.handoff"),
            Wire::new("gate.decision", "gate.handoff"),
            Wire::new("gate.interrupted", "loop.faults"),
            Wire::new("ui.interrupt", "gate.faults"),
            Wire::new("ui.interrupt", "cmodel.control"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    if !deliver_summary {
        // Hold delivery at the committed-summary boundary to simulate a crash
        // there, without timing a race or replacing the gate under test.
        assembly.wires.retain(|wire| wire.from != "gate.summary");
    }
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            log_file: log.map(std::path::Path::to_path_buf),
            stream: Some("handoff".into()),
            ..KernelOptions::default()
        },
    )
    .unwrap()
}

fn main_requests(events: &[lattice::EventEnvelope]) -> Vec<&lattice::EventEnvelope> {
    events
        .iter()
        .filter(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.source == "gate"
                && e.payload.get("purpose").is_none()
        })
        .collect()
}

#[test]
fn automatic_compaction_holds_originals_until_an_accepted_replacement() {
    let mut kernel = fixture(None);
    say(&mut kernel, "read source and two more results");
    let before = kernel.log().replay(1).unwrap();
    let compaction = before
        .iter()
        .find(|e| e.payload["purpose"] == context_gate::CONDENSE_PURPOSE)
        .unwrap()
        .clone();
    assert_eq!(
        main_requests(&before).len(),
        3,
        "the fourth main request must wait for compaction, not run on mechanical receipts"
    );
    assert!(!before
        .iter()
        .any(|e| e.event_type == context_gate::DECISION && e.payload["action"] == "digest"));
    let source = before
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "source")
        .unwrap();
    assert!(compaction.payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["event"] == source.id));
    // The kernel remains responsive while the compactor has not answered.
    say(&mut kernel, "input arriving during the wait");
    assert_eq!(main_requests(&kernel.log().replay(1).unwrap()).len(), 3);
    kernel.injector("cmodel").emit(
        "result",
        EventDraft::new(
            ce::MODEL_CALL_COMPLETED,
            &[&compaction.id],
            json!({"status":"ok","text":GOOD_SUMMARY}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let after = kernel.log().replay(1).unwrap();
    let summary = after
        .iter()
        .find(|e| e.event_type == context_gate::SUMMARY)
        .unwrap();
    let requests = main_requests(&after);
    assert_eq!(
        requests.len(),
        5,
        "the held request and queued input each proceed once"
    );
    let resumed = requests[3];
    assert!(resumed.seq > summary.seq);
    assert!(resumed.payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["digest"]["of"] == summary.id));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.causes == resumed.causes)
            .count(),
        1
    );
    let arrived = after
        .iter()
        .find(|e| {
            e.event_type == ce::USER_MESSAGE
                && e.payload["text"] == "input arriving during the wait"
        })
        .unwrap();
    assert!(requests[4].payload["input"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["event"] == arrived.id));
    assert!(resumed.causes.contains(&summary.id));
    let tail = resumed.payload["input"]["parts"].as_array().unwrap();
    for event in before.iter().filter(|e| {
        (e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "small-2")
            || (e.event_type == ce::MODEL_CALL_COMPLETED
                && e.payload["toolCalls"][0]["id"] == "small-2")
    }) {
        assert!(
            tail.iter().any(|p| p["event"] == event.id),
            "the recent tool exchange stays complete"
        );
    }
    assert!(
        !after.iter().any(|e| e.event_type == ce::ERROR),
        "no invalid emissions or lost causality"
    );
    kernel.shutdown();
}
