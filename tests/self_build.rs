//! The north-star demo, proven without a network: an agent (here a scripted
//! model) calls `install_component` with Python source; the host builds it
//! through the real gates — exam, human approval, hot install — and the model
//! then uses the freshly minted tool. The whole self-design → self-verify →
//! self-install → self-use loop, machine-checked in CI.
#![cfg(unix)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{minimal_loop, scripted_model, silent_ui, workshop_sink};

mod common;
use common::calc_tools;
use lattice::core_events as ce;
use lattice::workshop::{build_and_install, pending_installs, BuildOutcome};
use lattice::{
    AssemblyManifest, ComponentInstance, ComponentManifest, EventDraft, Factory, Kernel,
    KernelOptions, Wire,
};

/// The agent writes ONLY this: a pure handler. The bridge protocol wrapper
/// is the workshop's job — wire knowledge must not live in model memory.
const TEXTKIT_HANDLER: &str = r#"
def handle(tool, arguments):
    text = arguments.get("text", "")
    return {"upper": text.upper(), "words": len(text.split())}
"#;

fn registry() -> HashMap<String, ComponentManifest> {
    [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (calc_tools::NAME.to_string(), calc_tools::manifest()),
        (workshop_sink::NAME.to_string(), workshop_sink::manifest()),
    ]
    .into()
}

fn factories(displayed: Arc<Mutex<Vec<String>>>) -> HashMap<String, Factory> {
    let mut f: HashMap<String, Factory> = HashMap::new();
    f.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&displayed)))),
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
        calc_tools::NAME.to_string(),
        // exclusive=false: silent on foreign tools, so fan-out has one answerer
        Box::new(|c| Box::new(calc_tools::CalcTools::from_config(c))),
    );
    f.insert(
        workshop_sink::NAME.to_string(),
        Box::new(|_| Box::new(workshop_sink::WorkshopSink)),
    );
    f
}

#[test]
fn the_agent_builds_and_uses_its_own_tool() {
    // The scripted "agent": first turn asks to install text_stats, then uses it
    let script = json!({
        "script": [
            {"status": "ok", "toolCalls": [{"id": "t1", "tool": "InstallComponent", "arguments": {
                "instance": "textkit",
                "tool_name": "text_stats",
                "tool_description": "upper-case text and count words",
                "tool_parameters": {"type": "object", "properties": {"text": {"type": "string"}}},
                "handler": TEXTKIT_HANDLER,
                "reason": "the user wants text processing",
            }}]},
            {"status": "ok", "toolCalls": [{"id": "t2", "tool": "text_stats", "arguments": {"text": "hello lattice world"}}]},
            {"status": "ok", "text": "HELLO LATTICE WORLD — 3 words"},
        ]
    });

    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut facs = factories(Arc::clone(&displayed));
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
                    config: Some(json!({"exclusive": false})),
                },
            ),
            (
                "workshop".to_string(),
                ComponentInstance {
                    component: workshop_sink::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("loop.run", "workshop.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };

    let mut kernel =
        Kernel::start(&assembly, &registry(), &mut facs, KernelOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();

    kernel.injector("ui").emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": "make yourself a text tool then use it"}),
        ),
    );

    // The host loop: run to quiescence, then service any install the agent
    // asked for, then run again — exactly what the chat example does.
    loop {
        kernel.run_until_quiescent().unwrap();
        let pending = pending_installs(&kernel).unwrap();
        if pending.is_empty() {
            break;
        }
        for req in pending {
            let outcome = build_and_install(
                &mut kernel,
                &req,
                dir.path(),
                "loop",
                Some(&dir.path().join("assembly.json")),
                |_, _| true,
            )
            .unwrap();
            let payload = match outcome {
                BuildOutcome::Installed(msg) => {
                    json!({"call": req.call, "status": "ok", "result": msg})
                }
                BuildOutcome::Rejected(why) => json!({"call": req.call, "status": "error",
                    "error": {"code": "workshop.rejected", "message": why, "blame": "request"}}),
            };
            // Answer the install call in the workshop's name (it witnessed the request)
            kernel.injector("workshop").emit(
                "outcome",
                EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&req.cause], payload),
            );
        }
    }

    // The agent used its own creation, and the final answer reached the user
    assert_eq!(
        *displayed.lock().unwrap(),
        vec!["HELLO LATTICE WORLD — 3 words".to_string()]
    );

    // Installed means installed: the overlay took the write-back, and a
    // fresh baseline (a restart) gets the built component back by merging it
    let mut restarted_registry = registry();
    let mut restarted_assembly = lattice::AssemblyManifest::default();
    let report = lattice::overlay::apply(
        &mut restarted_registry,
        &mut restarted_assembly,
        &dir.path().join("assembly.json"),
    )
    .unwrap();
    assert_eq!(report.instances, 1, "the hot install survives a restart");
    assert!(restarted_registry.keys().any(|k| k.starts_with("agent:")));

    let events = kernel.log().replay(1).unwrap();
    // The install is on the record as a decision, with the agent's reason
    let install = events
        .iter()
        .find(|e| e.event_type == ce::COMPONENT_INSTALLED)
        .expect("the installation was recorded");
    assert!(install
        .reason
        .as_deref()
        .unwrap()
        .contains("text processing"));
    // The new tool actually ran, from its own process, caused across the boundary
    let used = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "textkit")
        .expect("the agent's tool ran");
    assert_eq!(used.payload["result"]["upper"], "HELLO LATTICE WORLD");
    assert_eq!(used.payload["result"]["words"], 3);
    // And from the first ask AFTER the install, the model was OFFERED the
    // new tool — the built component's manifest carried its declaration,
    // provider-stamped, no config edited anywhere
    let offered_after_install = events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.seq > install.seq)
        .all(|e| {
            e.payload["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "text_stats" && t["provider"] == "textkit")
        });
    assert!(
        offered_after_install,
        "a hot-installed tool must appear in every subsequent offer"
    );

    kernel.shutdown();
}

/// TWO installed components, and a call to one must not touch the other.
///
/// In-process components "stay silent" on a foreign tool by returning from
/// handle. Across a pipe that convention is a trap: the bridge writes the
/// delivery and then WAITS for the child to say it is done, so a child that
/// stays silent hangs its bridge until the watchman kills it. Seen in the
/// wild — four installed components, one tool call, all four declared crashed
/// and the turn could never end.
#[test]
fn one_components_tool_call_does_not_hang_the_others() {
    let handler = |word: &str| {
        format!("\ndef handle(tool, arguments):\n    return {{\"said\": \"{word}\"}}\n")
    };
    let install = |instance: &str, tool: &str, word: &str| {
        json!({"status": "ok", "toolCalls": [{"id": instance, "tool": "InstallComponent",
        "arguments": {
            "instance": instance,
            "tool_name": tool,
            "tool_description": "say a word",
            "tool_parameters": {"type": "object", "properties": {}},
            "handler": handler(word),
            "reason": "testing two of them",
        }}]})
    };
    let script = json!({"script": [
        install("alphakit", "alpha", "A"),
        install("betakit", "beta", "B"),
        // Call the FIRST one. Every other installed component is handed this
        // request too (fan-out), and must survive being handed it.
        {"status": "ok", "toolCalls": [{"id": "c1", "tool": "alpha", "arguments": {}}]},
        // If betakit was hung by the call above, this never comes back
        {"status": "ok", "toolCalls": [{"id": "c2", "tool": "beta", "arguments": {}}]},
        {"status": "ok", "text": "both still alive"},
    ]});

    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut facs = factories(Arc::clone(&displayed));
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
            (
                "tools".to_string(),
                ComponentInstance::new(calc_tools::NAME, Some(json!({"exclusive": false}))),
            ),
            (
                "workshop".to_string(),
                ComponentInstance::new(workshop_sink::NAME, None),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("loop.run", "workshop.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel =
        Kernel::start(&assembly, &registry(), &mut facs, KernelOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": "build two tools and use both"}),
        ),
    );

    let shop = lattice::workshop::Workshop {
        workshop_dir: dir.path().to_path_buf(),
        components_dir: dir.path().join("components"),
        loop_instance: "loop".to_string(),
        overlay: None,
    };
    shop.run(&mut kernel).unwrap();

    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    let crashed: Vec<&str> = events
        .iter()
        .filter(|e| e.event_type == ce::COMPONENT_CRASHED)
        .filter_map(|e| e.payload["component"].as_str())
        .collect();
    assert!(
        crashed.is_empty(),
        "a foreign tool request must not kill anybody: {crashed:?}"
    );
    assert_eq!(
        *displayed.lock().unwrap(),
        vec!["both still alive".to_string()]
    );
}

/// A tool request goes to the ONE component that provides it — not to every
/// provider on the wire. Inspection already guarantees the name has a single
/// owner, and the kernel already keeps that map (it is what offers the tool
/// list to the model), so broadcasting it only made every provider
/// responsible for discarding other people's mail.
///
/// The exception is a tool NOBODY provides: that still reaches everyone, so a
/// provider willing to say "no such tool" can answer. Without it the round
/// would wait for a result that could never come.
#[test]
fn a_tool_request_reaches_only_its_provider_unless_nobody_provides_it() {
    let handler = "\ndef handle(tool, arguments):\n    return {\"ok\": True}\n";
    let install = |instance: &str, tool: &str| {
        json!({"status": "ok", "toolCalls": [{"id": instance, "tool": "InstallComponent",
        "arguments": {
            "instance": instance, "tool_name": tool,
            "tool_description": "a tool", "tool_parameters": {"type": "object"},
            "handler": handler, "reason": "testing dispatch",
        }}]})
    };
    let script = json!({"script": [
        install("onekit", "one"),
        install("twokit", "two"),
        {"status": "ok", "toolCalls": [{"id": "c1", "tool": "one", "arguments": {}}]},
        // Nobody provides this one; it must still be ANSWERED, or the round
        // waits forever on a result that can never come
        {"status": "ok", "toolCalls": [{"id": "c2", "tool": "nosuchtool", "arguments": {}}]},
        {"status": "ok", "text": "done"},
    ]});
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut facs = factories(Arc::clone(&displayed));
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
            (
                "tools".to_string(),
                ComponentInstance::new(calc_tools::NAME, None),
            ),
            (
                "workshop".to_string(),
                ComponentInstance::new(workshop_sink::NAME, None),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("loop.run", "workshop.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel =
        Kernel::start(&assembly, &registry(), &mut facs, KernelOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    let shop = lattice::workshop::Workshop {
        workshop_dir: dir.path().to_path_buf(),
        components_dir: dir.path().join("components"),
        loop_instance: "loop".to_string(),
        overlay: None,
    };
    shop.run(&mut kernel).unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    // The known tool was answered by ITS provider, and only that one saw it
    let answered_by = |call: &str| -> Vec<&str> {
        events
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == call)
            .map(|e| e.source.as_str())
            .collect()
    };
    assert_eq!(answered_by("c1"), vec!["onekit"], "answered by its owner");
    // The unknown one still came back — as a SETTLEMENT by the kernel. It says
    // the chain ended and says nothing about whether anything happened, which
    // is the truth: the request reached nobody. The round could close on it.
    let settled = events.iter().any(|e| {
        e.event_type == ce::INTERRUPTED
            && e.payload["by"] == "no_provider"
            && e.payload["call"] == "c2"
    });
    assert!(
        settled,
        "a tool nobody owns must still be settled, not left hanging"
    );
    assert_eq!(*displayed.lock().unwrap(), vec!["done".to_string()]);
}

/// Installing was a ONE-WAY DOOR: nothing could take a component back out, so
/// a bad install could only be undone by hand-editing the overlay file — and a
/// component that crashed on every start came back on every start.
///
/// Removal is the inverse of an install and nothing more: the component stops,
/// its tools stop being offered, its wires go, and the overlay forgets it so
/// the removal survives a restart. What it never does is rewrite history.
#[test]
fn an_installed_component_can_be_taken_back_out_and_stays_out() {
    let handler = "\ndef handle(tool, arguments):\n    return {\"ok\": True}\n";
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "i1", "tool": "InstallComponent", "arguments": {
            "instance": "spare", "tool_name": "spare_tool",
            "tool_description": "a tool", "tool_parameters": {"type": "object"},
            "handler": handler, "reason": "testing removal",
        }}]},
        {"status": "ok", "text": "installed"},
    ]});
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut facs = factories(Arc::clone(&displayed));
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
            (
                "tools".to_string(),
                ComponentInstance::new(calc_tools::NAME, None),
            ),
            (
                "workshop".to_string(),
                ComponentInstance::new(workshop_sink::NAME, None),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "tools.execute"),
            Wire::new("loop.run", "workshop.execute"),
            Wire::new("tools.outcome", "loop.tools"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel =
        Kernel::start(&assembly, &registry(), &mut facs, KernelOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let overlay_path = dir.path().join("assembly.json");
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install one"})),
    );
    let shop = lattice::workshop::Workshop {
        workshop_dir: dir.path().to_path_buf(),
        components_dir: dir.path().join("components"),
        loop_instance: "loop".to_string(),
        overlay: Some(overlay_path.clone()),
    };
    shop.run(&mut kernel).unwrap();
    assert!(
        kernel.assembly().instances.contains_key("spare"),
        "installed"
    );

    // The base assembly is NOT the agent's to change — this is the rule that
    // makes an uninstall tool safe to hand a model at all, since "remove the
    // component called trust" would otherwise disable the gate judging it
    assert!(
        shop.remove(&mut kernel, "loop").is_err(),
        "what an install did not put here cannot be taken out"
    );
    assert!(kernel.assembly().instances.contains_key("loop"));

    // A disk document cannot retroactively claim a baseline instance.
    let original_overlay = std::fs::read(&overlay_path).unwrap();
    let mut forged: serde_json::Value = serde_json::from_slice(&original_overlay).unwrap();
    forged["instances"]["loop"] = json!({"component": minimal_loop::NAME});
    std::fs::write(&overlay_path, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(shop.remove(&mut kernel, "loop").is_err());
    assert!(kernel.assembly().instances.contains_key("loop"));
    std::fs::write(&overlay_path, &original_overlay).unwrap();

    // Persistence failure must not remove the live component.
    std::fs::write(&overlay_path, "not json").unwrap();
    assert!(shop.remove(&mut kernel, "spare").is_err());
    assert!(kernel.assembly().instances.contains_key("spare"));
    assert_eq!(std::fs::read_to_string(&overlay_path).unwrap(), "not json");
    std::fs::write(&overlay_path, original_overlay).unwrap();

    // What an install DID put here goes
    shop.remove(&mut kernel, "spare").expect("removable");
    assert!(!kernel.assembly().instances.contains_key("spare"));
    assert!(
        !kernel
            .assembly()
            .wires
            .iter()
            .any(|w| w.from.starts_with("spare.") || w.to.starts_with("spare.")),
        "its wires went with it"
    );
    // Recorded, not erased: the install is still on the ledger, and so is this
    let events = kernel.log().replay(1).unwrap();
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::COMPONENT_INSTALLED));
    let removed = events
        .iter()
        .find(|e| e.event_type == ce::COMPONENT_REMOVED)
        .expect("the removal is on the record too");
    assert_eq!(removed.payload["instance"], "spare");
    assert!(removed.reason.is_some(), "a decision states its reason");
    kernel.shutdown();

    // And it stays out: the overlay is what brings hot installs back, so a
    // removal that forgot to touch it would last only until the next start
    let mut registry = registry();
    let mut baseline = lattice::AssemblyManifest::default();
    let report = lattice::overlay::apply(&mut registry, &mut baseline, &overlay_path).unwrap();
    assert_eq!(report.instances, 0, "the overlay no longer brings it back");
    assert!(!baseline.instances.contains_key("spare"));
}

/// The MODEL's route to the same door — and the rule that makes it safe to
/// hand over. "Remove the component called trust" is a sentence that would
/// otherwise disable the gate judging the very call that said it. Only what an
/// install put here may go; the assembly the agent was started with is not
/// its to change, and the refusal comes back as an ordinary tool error it can
/// read and act on.
#[test]
fn the_model_may_remove_only_what_an_install_put_there() {
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "u1", "tool": "UninstallComponent",
            "arguments": {"instance": "loop", "reason": "it is in my way"}}]},
        {"status": "ok", "text": "refused, understood"},
    ]});
    let displayed = Arc::new(Mutex::new(Vec::new()));
    let mut facs = factories(Arc::clone(&displayed));
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
            (
                "workshop".to_string(),
                ComponentInstance::new(workshop_sink::NAME, None),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "workshop.execute"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel =
        Kernel::start(&assembly, &registry(), &mut facs, KernelOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "clean yourself up"})),
    );
    let shop = lattice::workshop::Workshop {
        workshop_dir: dir.path().to_path_buf(),
        components_dir: dir.path().join("components"),
        loop_instance: "loop".to_string(),
        overlay: Some(dir.path().join("assembly.json")),
    };
    shop.run(&mut kernel).unwrap();

    let events = kernel.log().replay(1).unwrap();
    assert!(
        kernel.assembly().instances.contains_key("loop"),
        "the assembly it was started with survives"
    );
    let refusal = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "u1")
        .expect("the request was answered");
    assert_eq!(refusal.payload["status"], "error");
    assert!(
        refusal.payload["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not yours to change"),
        "and told why, in words it can act on: {}",
        refusal.payload["error"]["message"]
    );
    assert!(!events.iter().any(|e| e.event_type == ce::COMPONENT_REMOVED));
    kernel.shutdown();
}

/// A model can only name what it can SEE, and what it sees is tool names —
/// the instance an install chose is never shown to it. Asking it for an
/// instance name was asking for something invisible: it named its tools, was
/// refused four times, and told the user nothing had been installed at all.
///
/// So a tool name resolves to its provider, and a refusal says what CAN go.
#[test]
fn a_removal_may_be_named_by_the_tool_it_provides() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = dir.path().join("assembly.json");
    std::fs::write(
        &overlay,
        json!({
            "components": [{"name": "agent:mytools", "tools": [{"name": "reverse_text"}]}],
            "instances": {"mytools": {"component": "agent:mytools"}},
            "wires": [],
        })
        .to_string(),
    )
    .unwrap();
    let at = Some(overlay.as_path());

    // The name it knows — the tool — finds the instance that provides it
    assert_eq!(
        lattice::workshop::resolve_removable(at, "reverse_text").as_deref(),
        Some("mytools")
    );
    // The instance name still works, for whoever does know it
    assert_eq!(
        lattice::workshop::resolve_removable(at, "mytools").as_deref(),
        Some("mytools")
    );
    // And nothing else does
    assert_eq!(lattice::workshop::resolve_removable(at, "loop"), None);
    assert_eq!(lattice::workshop::resolve_removable(None, "mytools"), None);

    // The listing a refusal quotes, so a wrong guess is fixable on the next try
    let installed = lattice::workshop::installed_instances(at);
    assert_eq!(
        installed,
        vec![("mytools".to_string(), vec!["reverse_text".to_string()])]
    );
}
