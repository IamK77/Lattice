//! Conformance exams — the standard test papers a component must pass before
//! it may claim a profile in an assembly. The exam drives the candidate
//! through a real kernel: structural fit is inspection's job, behavior is
//! proven here. Machine-graded; no self-assessment.

use std::collections::HashMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::components::model_common::verify_fingerprint;
use crate::contracts::assembly::{AssemblyManifest, ComponentInstance, Wire};
use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::EventDraft;
use crate::contracts::profile::{check_claim, core_profiles};
use crate::kernel::host::{Factory, Kernel, KernelOptions, KERNEL_SOURCE};

/// A FAULT the kernel recorded, as opposed to its ordinary bookkeeping.
///
/// The kernel also settles chains nobody will answer (`core.control.interrupted`)
/// — an exam assembly has no tool providers, so every request it drives gets
/// one, and treating that as a fault failed every candidate for something the
/// exam itself caused.
fn is_kernel_fault(event: &crate::contracts::event::EventEnvelope) -> bool {
    event.source == KERNEL_SOURCE
        && matches!(event.event_type.as_str(), ce::ERROR | ce::COMPONENT_CRASHED)
}

fn exam_driver(event_types: &[&str]) -> ComponentManifest {
    ComponentManifest {
        name: "exam-driver".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:exam-driver".to_string(),
        inputs: vec![],
        outputs: vec![PortDecl::new("out", event_types)],
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

struct Noop;
impl crate::kernel::host::Component for Noop {
    fn handle(
        &mut self,
        _port: &str,
        _event: &crate::contracts::event::EventEnvelope,
        _ctx: &mut crate::kernel::host::Ctx,
    ) {
    }
}

/// The fingerprint of empty material, computed by the same rule the loop uses
fn empty_material_fingerprint() -> String {
    format!("sha256:{:x}", Sha256::new().finalize())
}

/// Exam for the "model-adapter" profile: fed one valid request, the candidate
/// must complete it — exactly one schema-valid `model_call_completed` on the
/// `result` port, no kernel errors. An error completion passes: the exam
/// grades contract shape, not intelligence.
pub fn examine_model_adapter(manifest: &ComponentManifest, factory: Factory) -> Vec<String> {
    let profile = core_profiles()
        .into_iter()
        .find(|p| p.name == "model-adapter")
        .expect("core profile exists");
    let mut problems = check_claim(manifest, &profile);
    if !problems.is_empty() {
        return problems;
    }

    let registry: HashMap<String, ComponentManifest> = [
        (
            "exam-driver".to_string(),
            exam_driver(&[ce::MODEL_CALL_STARTED]),
        ),
        (manifest.name.clone(), manifest.clone()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("exam-driver".to_string(), Box::new(|_| Box::new(Noop)));
    factories.insert(manifest.name.clone(), factory);

    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "exam-driver".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "candidate".to_string(),
                ComponentInstance {
                    component: manifest.name.clone(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "candidate.request")],
    };

    let mut kernel = match Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    ) {
        Ok(kernel) => kernel,
        Err(err) => return vec![format!("exam assembly failed to start: {err}")],
    };
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({
                "model": "exam",
                "input": {"parts": [], "fingerprint": empty_material_fingerprint()},
            }),
        ),
    );
    if let Err(err) = kernel.run_until_quiescent() {
        return vec![format!("exam run failed: {err}")];
    }

    let events = match kernel.log().replay(1) {
        Ok(events) => events,
        Err(error) => return vec![format!("cannot read conformance ledger: {error}")],
    };
    let completions: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_COMPLETED && e.source == "candidate")
        .collect();
    if completions.len() != 1 {
        problems.push(format!(
            "expected exactly one model_call_completed from the candidate, saw {}",
            completions.len()
        ));
    }
    for event in events.iter().filter(|e| is_kernel_fault(e)) {
        problems.push(format!(
            "kernel recorded a fault during the exam: {}",
            event.payload["message"]
        ));
    }
    if let Some(completion) = completions.first() {
        if completion.causes.is_empty() {
            problems.push("the completion must be caused by the request".to_string());
        }
        problems.extend(reasoning_shape_problems(&completion.payload));
    }
    problems
}

/// Reasoning is optional and its CONTENT is never graded — a model that does
/// not think is not a lesser adapter, and one that thinks badly is still a
/// conforming one. What is graded is usability: a readable part must actually
/// carry its text, and a sealed part must carry the baggage that is its whole
/// reason to exist. The letterhead schema cannot say this, because whether
/// `text` or `opaque` is the required one depends on the kind.
fn reasoning_shape_problems(payload: &Value) -> Vec<String> {
    let Some(parts) = payload["reasoning"].as_array() else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        match part["kind"].as_str() {
            Some(ce::REASONING_TEXT) => {
                if !part["text"].is_string() {
                    problems.push(format!(
                        "reasoning part {index} is readable but has no text"
                    ));
                }
            }
            Some(ce::REASONING_HIDDEN) => {
                if part["opaque"].is_null() {
                    problems.push(format!(
                        "reasoning part {index} is sealed but carries nothing to hand back"
                    ));
                }
            }
            other => problems.push(format!(
                "reasoning part {index} has an unknown kind: {other:?}"
            )),
        }
    }
    problems
}

/// Exam for the "tool-provider" profile. The probe uses one of the
/// candidate's OWN declared tools (under fan-out wiring, providers stay
/// silent on foreign tools — so probing a stranger would test nothing).
/// Bad arguments are part of the exam: an error completion passes, a crash
/// or silence fails. `factory` is None for process-hosted candidates.
pub fn examine_tool_provider(
    manifest: &ComponentManifest,
    factory: Option<Factory>,
    probe_tool: &str,
) -> Vec<String> {
    let profile = core_profiles()
        .into_iter()
        .find(|p| p.name == "tool-provider")
        .expect("core profile exists");
    let mut problems = check_claim(manifest, &profile);
    if !problems.is_empty() {
        return problems;
    }

    let registry: HashMap<String, ComponentManifest> = [
        (
            "exam-driver".to_string(),
            exam_driver(&[ce::TOOL_EXEC_STARTED]),
        ),
        (manifest.name.clone(), manifest.clone()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("exam-driver".to_string(), Box::new(|_| Box::new(Noop)));
    if let Some(factory) = factory {
        factories.insert(manifest.name.clone(), factory);
    }

    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "exam-driver".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "candidate".to_string(),
                ComponentInstance {
                    component: manifest.name.clone(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "candidate.execute")],
    };

    let mut kernel = match Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    ) {
        Ok(kernel) => kernel,
        Err(err) => return vec![format!("exam assembly failed to start: {err}")],
    };
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "exam", "tool": probe_tool, "arguments": {}}),
        ),
    );
    if let Err(err) = kernel.run_until_quiescent() {
        return vec![format!("exam run failed: {err}")];
    }

    let events = match kernel.log().replay(1) {
        Ok(events) => events,
        Err(error) => return vec![format!("cannot read conformance ledger: {error}")],
    };
    let completions: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "candidate")
        .collect();
    if completions.len() != 1 {
        problems.push(format!(
            "expected exactly one tool_exec_completed from the candidate, saw {}",
            completions.len()
        ));
    }
    for event in events.iter().filter(|e| is_kernel_fault(e)) {
        problems.push(format!(
            "kernel recorded a fault during the exam: {}",
            event.payload["message"]
        ));
    }
    if let Some(completion) = completions.first() {
        if completion.causes.is_empty() {
            problems.push("the completion must be caused by the request".to_string());
        }
    }
    kernel.shutdown();
    problems
}

/// Exam for the "policy" profile. Config-agnostic — it grades the contract
/// shape, not the stance: every reviewed request must take exactly one of
/// the two paths. Either it is forwarded (one re-emission, same call, caused
/// by the request), or it is answered (one error completion, plus a reasoned
/// decision event) — never both, never neither.
pub fn examine_policy(manifest: &ComponentManifest, factory: Option<Factory>) -> Vec<String> {
    let profile = core_profiles()
        .into_iter()
        .find(|p| p.name == "policy")
        .expect("core profile exists");
    let mut problems = check_claim(manifest, &profile);
    if !problems.is_empty() {
        return problems;
    }

    let registry: HashMap<String, ComponentManifest> = [
        (
            "exam-driver".to_string(),
            exam_driver(&[ce::TOOL_EXEC_STARTED]),
        ),
        (manifest.name.clone(), manifest.clone()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("exam-driver".to_string(), Box::new(|_| Box::new(Noop)));
    if let Some(factory) = factory {
        factories.insert(manifest.name.clone(), factory);
    }
    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "exam-driver".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "candidate".to_string(),
                ComponentInstance {
                    component: manifest.name.clone(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "candidate.review")],
    };

    let mut kernel = match Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    ) {
        Ok(kernel) => kernel,
        Err(err) => return vec![format!("exam assembly failed to start: {err}")],
    };
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "exam", "tool": "exam_probe", "arguments": {}}),
        ),
    );
    if let Err(err) = kernel.run_until_quiescent() {
        return vec![format!("exam run failed: {err}")];
    }

    let events = match kernel.log().replay(1) {
        Ok(events) => events,
        Err(error) => return vec![format!("cannot read conformance ledger: {error}")],
    };
    // The request under review — named, not "the first event on the ledger",
    // which is now the stream's own opening event.
    let Some(reviewed) = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "driver")
    else {
        return vec!["the reviewed request never reached the ledger".to_string()];
    };
    let forwards: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "candidate")
        .collect();
    let verdicts: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "candidate")
        .collect();
    match (forwards.len(), verdicts.len()) {
        (1, 0) => {
            if forwards[0].causes.is_empty() {
                problems.push("the forward must be caused by the reviewed request".to_string());
            }
            if forwards[0].payload["call"] != reviewed.payload["call"] {
                problems.push("the forward must carry the same call id".to_string());
            }
        }
        (0, 1) => {
            if verdicts[0].causes.is_empty() {
                problems.push("the verdict must be caused by the reviewed request".to_string());
            }
            let reasoned = events.iter().any(|e| {
                e.source == "candidate" && e.reason.as_deref().is_some_and(|r| !r.is_empty())
            });
            if !reasoned {
                problems
                    .push("a denial must be accompanied by a reasoned decision event".to_string());
            }
        }
        (f, v) => problems.push(format!(
            "a reviewed request must be forwarded XOR answered; saw {f} forwards and {v} verdicts"
        )),
    }
    for event in events.iter().filter(|e| is_kernel_fault(e)) {
        problems.push(format!(
            "kernel recorded a fault during the exam: {}",
            event.payload["message"]
        ));
    }
    kernel.shutdown();
    problems
}

/// Exam for the "context-manager" profile: fed one valid ask, the candidate
/// must forward exactly ONE model call on `forward`, caused by the ask, with
/// a fingerprint that verifies against the parts it ACTUALLY forwards, and
/// any digest part must name its original and carry note text. Grades the
/// contract shape, not the trimming stance (config-agnostic, like policy).
pub fn examine_context_manager(manifest: &ComponentManifest, factory: Factory) -> Vec<String> {
    let profile = core_profiles()
        .into_iter()
        .find(|p| p.name == "context-manager")
        .expect("core profile exists");
    let mut problems = check_claim(manifest, &profile);
    if !problems.is_empty() {
        return problems;
    }

    let registry: HashMap<String, ComponentManifest> = [
        (
            "exam-driver".to_string(),
            exam_driver(&[ce::MODEL_CALL_STARTED]),
        ),
        (manifest.name.clone(), manifest.clone()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("exam-driver".to_string(), Box::new(|_| Box::new(Noop)));
    factories.insert(manifest.name.clone(), factory);

    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "exam-driver".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "candidate".to_string(),
                ComponentInstance {
                    component: manifest.name.clone(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "candidate.ask")],
    };

    let mut kernel = match Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    ) {
        Ok(kernel) => kernel,
        Err(err) => return vec![format!("exam assembly failed to start: {err}")],
    };
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::MODEL_CALL_STARTED,
            &[],
            json!({
                "model": "exam",
                "input": {"parts": [], "fingerprint": empty_material_fingerprint()},
            }),
        ),
    );
    if let Err(err) = kernel.run_until_quiescent() {
        return vec![format!("exam run failed: {err}")];
    }

    let events = match kernel.log().replay(1) {
        Ok(events) => events,
        Err(error) => return vec![format!("cannot read conformance ledger: {error}")],
    };
    let forwards: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "candidate")
        .collect();
    if forwards.len() != 1 {
        problems.push(format!(
            "expected exactly one forwarded model call from the candidate, saw {}",
            forwards.len()
        ));
    }
    if let Some(forward) = forwards.first() {
        if forward.causes.is_empty() {
            problems.push("the forward must be caused by the ask".to_string());
        }
        let parts = forward.payload["input"]["parts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Err(err) =
            verify_fingerprint(&parts, forward.payload["input"]["fingerprint"].as_str())
        {
            problems.push(format!(
                "the forwarded fingerprint must verify against the forwarded parts: {err}"
            ));
        }
        for part in &parts {
            if let Some(digest) = part.get("digest") {
                if digest["of"].as_str().is_none()
                    || digest["text"].as_str().is_none_or(str::is_empty)
                {
                    problems.push(
                        "every digest part must name its original and carry note text".to_string(),
                    );
                }
            }
        }
    }
    for event in events.iter().filter(|e| is_kernel_fault(e)) {
        problems.push(format!(
            "kernel recorded a fault during the exam: {}",
            event.payload["message"]
        ));
    }
    kernel.shutdown();
    problems
}

/// Exam for the "frontend" profile. A frontend's emissions are human-driven,
/// so the paper is deliberately lighter than the others: it proves the
/// candidate SURVIVES everything the assembly will send it — a text reply,
/// an error reply, a turn boundary — without crashing or emitting anything
/// illegal (a bad emission would surface as a kernel fault). The sending
/// half (user messages, authorization answers) is graded structurally by
/// the claim check; no exam can press keys. `factory` is None for
/// process-hosted candidates.
pub fn examine_frontend(manifest: &ComponentManifest, factory: Option<Factory>) -> Vec<String> {
    let profile = core_profiles()
        .into_iter()
        .find(|p| p.name == "frontend")
        .expect("core profile exists");
    let mut problems = check_claim(manifest, &profile);
    // A frontend claiming the authorize capability is graded on that claim
    // too (structural: the answer port with the external-input letter)
    if manifest
        .implements
        .iter()
        .any(|p| p == "frontend-authorize")
    {
        let authorize = core_profiles()
            .into_iter()
            .find(|p| p.name == "frontend-authorize")
            .expect("core profile exists");
        problems.extend(check_claim(manifest, &authorize));
    }
    if !problems.is_empty() {
        return problems;
    }

    let registry: HashMap<String, ComponentManifest> = [
        (
            "exam-driver".to_string(),
            exam_driver(&[ce::OUTPUT_REPLY, ce::TURN_COMPLETED]),
        ),
        (manifest.name.clone(), manifest.clone()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("exam-driver".to_string(), Box::new(|_| Box::new(Noop)));
    if let Some(factory) = factory {
        factories.insert(manifest.name.clone(), factory);
    }

    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "exam-driver".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "candidate".to_string(),
                ComponentInstance {
                    component: manifest.name.clone(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "candidate.display")],
    };

    let mut kernel = match Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    ) {
        Ok(kernel) => kernel,
        Err(err) => return vec![format!("exam assembly failed to start: {err}")],
    };
    let deliveries = [
        EventDraft::new(ce::OUTPUT_REPLY, &[], json!({"text": "exam reply"})),
        EventDraft::new(
            ce::OUTPUT_REPLY,
            &[],
            json!({"text": null, "error": {
                "code": "exam.error", "message": "exam error reply", "blame": "provider",
            }}),
        ),
        EventDraft::new(ce::TURN_COMPLETED, &[], json!({})),
    ];
    for draft in deliveries {
        kernel.injector("driver").emit("out", draft);
    }
    if let Err(err) = kernel.run_until_quiescent() {
        return vec![format!("exam run failed: {err}")];
    }

    let events = match kernel.log().replay(1) {
        Ok(events) => events,
        Err(error) => return vec![format!("cannot read conformance ledger: {error}")],
    };
    for event in events.iter().filter(|e| is_kernel_fault(e)) {
        problems.push(format!(
            "kernel recorded a fault during the exam: {}",
            event.payload["message"]
        ));
    }
    kernel.shutdown();
    problems
}
