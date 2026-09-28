//! The finish line of the hot-install route: a MINIMAL runtime (no skill
//! library assembled) gains the skill library at runtime — as a separate
//! process running the code already inside the product binary
//! (`lattice component skill-library`), wired by rule (default_wiring
//! consumed, the rest like its peers), persisted through the overlay, and
//! still there after a restart. Nothing is downloaded and the kernel is not
//! changed: install = a manifest + wires, exactly as promised.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui, skill_library};
use lattice::core_events as ce;
use lattice::workshop::suggested_wires;
use lattice::{
    overlay, AssemblyManifest, ComponentInstance, ComponentManifest, EventDraft, Factory, Kernel,
    KernelOptions, RuntimeKind, Wire,
};

fn write_skill(root: &Path, name: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: A test skill.\n---\nfollow these steps\n"),
    )
    .unwrap();
}

/// The skill library in its PROCESS form: same self-description, the entry
/// is the product binary running its own builtin as a bridge child.
fn process_form_skill_library() -> ComponentManifest {
    let mut manifest = skill_library::manifest();
    manifest.runtime = RuntimeKind::Process;
    manifest.entry = format!("{} component skill-library", env!("CARGO_BIN_EXE_lattice"));
    manifest
}

/// A minimal runtime: ui, loop, scripted model. No tools, no skills.
fn minimal(
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
            process_form_skill_library().name.clone(),
            process_form_skill_library(),
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

fn load_script() -> Value {
    json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "LoadSkill", "arguments": {"name": "release-dance"}}]},
        {"status": "ok", "text": "done"},
    ]})
}

fn skills_answered(kernel: &Kernel) -> bool {
    kernel.log().replay(1).unwrap().iter().any(|e| {
        e.event_type == ce::TOOL_EXEC_COMPLETED
            && e.source == "skills"
            && e.payload["status"] == "ok"
            && e.payload["result"]["content"]
                .as_str()
                .is_some_and(|c| c.contains("follow these steps"))
    })
}

#[test]
fn a_builtin_installs_as_a_process_serves_skills_and_survives_a_restart() {
    let library = tempfile::tempdir().unwrap();
    write_skill(library.path(), "release-dance");
    let overlay_dir = tempfile::tempdir().unwrap();
    let overlay_path = overlay_dir.path().join("assembly.json");

    // Life one: a minimal runtime; the skill library arrives at runtime
    let (registry, mut factories, assembly) = minimal(load_script());
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();

    let manifest = process_form_skill_library();
    let config = json!({"dirs": [library.path().display().to_string()]});
    let wires = suggested_wires(
        kernel.assembly(),
        kernel.component_registry(),
        &manifest,
        "skills",
        "loop",
    );
    // default_wiring consumed: the self-referential ring is there…
    assert!(
        wires
            .iter()
            .any(|w| w.from == "skills.changed" && w.to == "skills.refresh"),
        "the manifest's own suggestion is consumed"
    );
    // …and with no peers, the tool line falls back to the loop
    assert!(wires
        .iter()
        .any(|w| w.from == "loop.run" && w.to == "skills.execute"));

    kernel
        .install(
            manifest.clone(),
            "skills",
            Some(config.clone()),
            &wires,
            "the user asked for skills on a minimal runtime",
            &[],
        )
        .unwrap();
    overlay::record_install(&overlay_path, &manifest, "skills", Some(&config), &wires).unwrap();

    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "use the skill"})),
    );
    kernel.run_until_quiescent().unwrap();
    assert!(
        skills_answered(&kernel),
        "the process-form skill library answered load_skill over the bridge"
    );
    kernel.shutdown();

    // Life two: a FRESH minimal runtime plus the overlay — the install is back
    let (mut registry, mut factories, mut assembly) = minimal(load_script());
    registry.remove(&process_form_skill_library().name); // truly fresh: the overlay carries it
    let report = overlay::apply(&mut registry, &mut assembly, &overlay_path).unwrap();
    assert_eq!(report.instances, 1);
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the merged assembly boots the process component");
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "again"})),
    );
    kernel.run_until_quiescent().unwrap();
    assert!(
        skills_answered(&kernel),
        "the restart brought the installed component back, still answering"
    );
    kernel.shutdown();
}

/// In a GATED assembly the peer rule hangs the new provider off the gate's
/// forward — a hardcoded loop.run would silently bypass the gates.
#[test]
fn peer_wiring_follows_the_gate() {
    use lattice::components::{fs_tools, trust_policy};
    let registry: HashMap<String, ComponentManifest> = [
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (trust_policy::NAME.to_string(), trust_policy::manifest()),
        (fs_tools::NAME.to_string(), fs_tools::manifest()),
    ]
    .into();
    let assembly = AssemblyManifest {
        instances: [
            (
                "loop".to_string(),
                ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "trust".to_string(),
                ComponentInstance {
                    component: trust_policy::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "fs".to_string(),
                ComponentInstance {
                    component: fs_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("loop.run", "trust.review"),
            Wire::new("trust.forward", "fs.execute"),
            Wire::new("trust.verdict", "loop.tools"),
            Wire::new("fs.outcome", "loop.tools"),
        ],
    };

    let wires = suggested_wires(
        &assembly,
        &registry,
        &skill_library::manifest(),
        "skills",
        "loop",
    );
    assert!(
        wires
            .iter()
            .any(|w| w.from == "trust.forward" && w.to == "skills.execute"),
        "the request line hangs off the gate, like the peers: {wires:?}"
    );
    assert!(wires
        .iter()
        .any(|w| w.from == "skills.outcome" && w.to == "loop.tools"));
    assert!(
        !wires
            .iter()
            .any(|w| w.from == "loop.run" && w.to == "skills.execute"),
        "nothing may bypass the gate"
    );
}

/// Profile-first wiring: a crowd of tools-having components that never
/// CLAIMED the tool-provider profile cannot outvote the one claiming peer.
/// Two unclaimed oddballs are fed straight from loop.run; the claiming
/// provider sits behind the gate. Event-type majority would pick loop.run
/// (2 votes to 1) — the profile rule listens only to the claiming peer.
#[test]
fn unclaimed_components_cannot_outvote_the_claiming_peer() {
    use lattice::components::{fs_tools, trust_policy};
    use lattice::{PortDecl, RuntimeKind};

    // Has tools, accepts tool requests on its own port name — but never
    // declared implements. Grandfathered shape, not a standard citizen.
    let oddball = ComponentManifest {
        name: "oddball".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:oddball".to_string(),
        inputs: vec![PortDecl::new("intake", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("done", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: vec![json!({"name": "odd", "description": "odd",
                           "parameters": {"type": "object"}})],
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };

    let registry: HashMap<String, ComponentManifest> = [
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (trust_policy::NAME.to_string(), trust_policy::manifest()),
        (fs_tools::NAME.to_string(), fs_tools::manifest()),
        ("oddball".to_string(), oddball),
    ]
    .into();
    let instance = |component: &str| ComponentInstance {
        component: component.to_string(),
        config: None,
        requires: Vec::new(),
    };
    let assembly = AssemblyManifest {
        instances: [
            ("loop".to_string(), instance(minimal_loop::NAME)),
            ("trust".to_string(), instance(trust_policy::NAME)),
            ("fs".to_string(), instance(fs_tools::NAME)),
            ("odd1".to_string(), instance("oddball")),
            ("odd2".to_string(), instance("oddball")),
        ]
        .into(),
        wires: vec![
            Wire::new("loop.run", "trust.review"),
            Wire::new("trust.forward", "fs.execute"),
            Wire::new("trust.verdict", "loop.tools"),
            Wire::new("fs.outcome", "loop.tools"),
            // The oddballs bypass the gate — legal, but not the standard
            Wire::new("loop.run", "odd1.intake"),
            Wire::new("loop.run", "odd2.intake"),
            Wire::new("odd1.done", "loop.tools"),
            Wire::new("odd2.done", "loop.tools"),
        ],
    };

    let wires = suggested_wires(
        &assembly,
        &registry,
        &skill_library::manifest(),
        "skills",
        "loop",
    );
    assert!(
        wires
            .iter()
            .any(|w| w.from == "trust.forward" && w.to == "skills.execute"),
        "the claiming newcomer follows the claiming peer, not the crowd: {wires:?}"
    );
    assert!(
        !wires
            .iter()
            .any(|w| w.from == "loop.run" && w.to == "skills.execute"),
        "the unclaimed crowd must not pull the newcomer around the gate: {wires:?}"
    );
}
