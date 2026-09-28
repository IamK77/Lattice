//! The assembly overlay: installs write it, startup merges it — so "installed"
//! still means installed after a restart. The merge is atomic (a problematic
//! overlay is skipped whole, never half-applied) and duplicate wires are
//! dropped (a doubled wire would mean doubled delivery).
#![cfg(unix)]

use std::collections::HashMap;

use serde_json::{json, Value};

use lattice::core_events as ce;
use lattice::{
    overlay, AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
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

/// The Python demo component from the bridge tests — a real process-form
/// component, exactly what a network-installed component looks like.
fn hasher_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "hash-tool".to_string(),
        version: "0.1.0".to_string(),
        runtime: RuntimeKind::Process,
        entry: "python3 examples/components/hash_tool.py".to_string(),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["tool-provider".to_string()],
        tools: vec![json!({
            "name": "sha256",
            "description": "hash a text",
            "parameters": {"type": "object", "properties": {"text": {"type": "string"}},
                           "required": ["text"]},
            "effects": {"reversible": true},
        })],
        prompt: None,
        handle_timeout_ms: Some(10_000),
        concurrency: None,
    }
}

fn baseline() -> (HashMap<String, ComponentManifest>, AssemblyManifest) {
    let registry: HashMap<String, ComponentManifest> =
        [("driver".to_string(), driver_manifest())].into();
    let assembly = AssemblyManifest {
        instances: [(
            "driver".to_string(),
            ComponentInstance {
                component: "driver".to_string(),
                requires: Vec::new(),
                config: None,
            },
        )]
        .into(),
        wires: vec![],
    };
    (registry, assembly)
}

fn overlay_canon() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(include_str!("../schemas/assembly_overlay.json"))
        .expect("canon schema files are valid JSON");
    jsonschema::validator_for(&schema).expect("canon schema files compile")
}

#[test]
fn record_then_apply_boots_the_installed_component_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");

    // "Install": record the component, its instance and its wires — this is
    // what build_and_install writes after a successful hot install
    overlay::record_install(
        &path,
        &hasher_manifest(),
        "hasher",
        None,
        &[Wire::new("driver.out", "hasher.execute")],
    )
    .unwrap();

    // The written document satisfies its canon
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(overlay_canon().validate(&written).is_ok());

    // "Restart": a fresh baseline plus the overlay — the installed component
    // is back, wired, and answers
    let (mut registry, mut assembly) = baseline();
    let report = overlay::apply(&mut registry, &mut assembly, &path).unwrap();
    assert_eq!(report.components, 1);
    assert_eq!(report.instances, 1);
    assert_eq!(report.wires, 1);

    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the merged assembly passes inspection");
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": "c1", "tool": "sha256", "arguments": {"text": "lattice"}}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let answered = kernel.log().replay(1).unwrap().into_iter().any(|e| {
        e.event_type == ce::TOOL_EXEC_COMPLETED
            && e.source == "hasher"
            && e.payload["status"] == "ok"
    });
    kernel.shutdown();
    assert!(answered, "the overlay-restored component answers its tool");
}

#[test]
fn a_missing_overlay_is_an_empty_overlay() {
    let (mut registry, mut assembly) = baseline();
    let report = overlay::apply(
        &mut registry,
        &mut assembly,
        std::path::Path::new("/no/such/overlay.json"),
    )
    .unwrap();
    assert_eq!(report, overlay::OverlayReport::default());
    assert_eq!(assembly.instances.len(), 1);
}

#[test]
fn a_problematic_overlay_is_skipped_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");

    // Not JSON at all
    std::fs::write(&path, "not json {").unwrap();
    let (mut registry, mut assembly) = baseline();
    assert!(overlay::apply(&mut registry, &mut assembly, &path).is_err());

    // A valid new component AND a colliding instance name: nothing may land
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "components": [serde_json::to_value(hasher_manifest()).unwrap()],
            "instances": {
                "driver": {"component": "hash-tool"}
            },
            "wires": []
        }))
        .unwrap(),
    )
    .unwrap();
    let (mut registry, mut assembly) = baseline();
    let refused = overlay::apply(&mut registry, &mut assembly, &path);
    assert!(
        refused.is_err(),
        "an instance collision refuses the overlay"
    );
    assert!(
        !registry.contains_key("hash-tool"),
        "atomicity: the valid component must not land when a sibling entry is refused"
    );
}

#[test]
fn duplicate_wires_are_dropped_on_merge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "components": [serde_json::to_value(hasher_manifest()).unwrap()],
            "instances": {"hasher": {"component": "hash-tool"}},
            "wires": [
                {"from": "driver.out", "to": "hasher.execute"},
                {"from": "driver.out", "to": "hasher.execute"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let (mut registry, mut assembly) = baseline();
    assembly
        .wires
        .push(Wire::new("driver.out", "hasher.execute"));
    let report = overlay::apply(&mut registry, &mut assembly, &path).unwrap();
    assert_eq!(
        report.wires, 0,
        "a wire already on the baseline (and its repeat) must not double"
    );
    assert_eq!(assembly.wires.len(), 1);
}

#[test]
fn recording_twice_upserts_instead_of_duplicating() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");
    let wires = [Wire::new("driver.out", "hasher.execute")];
    overlay::record_install(&path, &hasher_manifest(), "hasher", None, &wires).unwrap();
    overlay::record_install(
        &path,
        &hasher_manifest(),
        "hasher",
        Some(&json!({"verbose": true})),
        &wires,
    )
    .unwrap();

    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["components"].as_array().unwrap().len(), 1);
    assert_eq!(written["wires"].as_array().unwrap().len(), 1);
    assert_eq!(written["instances"]["hasher"]["config"]["verbose"], true);
}

#[test]
fn the_standard_assembly_merges_its_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");
    overlay::record_install(
        &path,
        &hasher_manifest(),
        "hasher",
        None,
        &[Wire::new("loop.run", "hasher.execute")],
    )
    .unwrap();

    let cfg = lattice::preset::PresetConfig {
        adapter: "scripted".to_string(),
        model: "scripted".to_string(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(".".to_string()),
        context_window: 64000,
        usage_input_field: "input_tokens".to_string(),
        profile: None,
        catalog_problems: Vec::new(),
        system: "test".to_string(),
        scripted: Some(json!({"script": []})),
        thinking: None,
        overlay: Some(path),
        assembly: None,
    };
    let (registry, _factories, assembly) = lattice::preset::standard(&cfg).expect("preset builds");
    assert!(registry.contains_key("hash-tool"));
    assert!(assembly.instances.contains_key("hasher"));
}

/// A definition goes with the LAST instance standing on it — but not before.
///
/// Two instances can share one component, so a removal cannot simply take the
/// definition with it. Keeping it forever is the other mistake: the file then
/// gains a definition per install and never loses one, and someone reading it
/// sees components that no longer run anywhere.
#[test]
fn a_definition_leaves_with_its_last_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "components": [
                {"name": "agent:calc", "tools": [{"name": "calc"}]},
                {"name": "agent:other", "tools": []},
            ],
            "instances": {
                "calc-a": {"component": "agent:calc"},
                "calc-b": {"component": "agent:calc"},
                "lonely": {"component": "agent:other"},
            },
            "wires": [
                {"from": "loop.run", "to": "calc-a.execute"},
                {"from": "calc-b.outcome", "to": "loop.tools"},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let read = || -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    };
    let names = |v: &serde_json::Value| -> Vec<String> {
        v["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect()
    };

    // One of two sharers goes: the definition stays, because one still stands
    assert!(overlay::record_removal(&path, "calc-a").unwrap());
    let after = read();
    assert!(names(&after).contains(&"agent:calc".to_string()));
    assert_eq!(after["wires"].as_array().unwrap().len(), 1, "its wire went");

    // The last one goes: the definition goes with it
    assert!(overlay::record_removal(&path, "calc-b").unwrap());
    let after = read();
    assert_eq!(
        names(&after),
        vec!["agent:other".to_string()],
        "nothing stands on agent:calc any more"
    );
    assert!(after["wires"].as_array().unwrap().is_empty());
    assert!(
        after["instances"]["lonely"].is_object(),
        "the other is untouched"
    );

    // Removing what is not there says so, and changes nothing
    assert!(!overlay::record_removal(&path, "calc-a").unwrap());
    assert_eq!(names(&read()), vec!["agent:other".to_string()]);
}

/// A hand-edited overlay of the wrong SHAPE is refused, not walked into.
///
/// Only nulls were filled in before writing, so a `wires` that was an object
/// (or an `instances` that was a list) went straight into code reaching for
/// the shape it expected — and reaching for it with `unwrap`, on the thread
/// that owns the kernel. A typo in a file people are invited to edit ended
/// the session.
#[test]
fn recording_into_a_malformed_overlay_refuses_instead_of_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");
    let manifest = lattice::ComponentManifest {
        name: "greeter".to_string(),
        version: "0".to_string(),
        runtime: lattice::RuntimeKind::Process,
        entry: "cat".to_string(),
        inputs: vec![lattice::PortDecl::new(
            "execute",
            &[lattice::core_events::TOOL_EXEC_STARTED],
        )],
        outputs: vec![lattice::PortDecl::new(
            "outcome",
            &[lattice::core_events::TOOL_EXEC_COMPLETED],
        )],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let wire = lattice::Wire::new("trust.forward", "greeter.execute");

    for wrong in [
        serde_json::json!({"wires": {}}),
        serde_json::json!({"instances": []}),
        serde_json::json!({"components": {}}),
    ] {
        std::fs::write(&path, serde_json::to_string(&wrong).unwrap()).unwrap();
        let refused = lattice::overlay::record_install(
            &path,
            &manifest,
            "greeter",
            None,
            std::slice::from_ref(&wire),
        );
        assert!(
            refused.is_err(),
            "an overlay shaped like {wrong} must be refused, not amended"
        );
        // And left exactly as it was: a document nobody could parse is not
        // improved by half-writing over it.
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after, wrong, "the bad document is left untouched");
    }

    // A document that is merely EMPTY is fine — that is the ordinary first
    // install, and refusing it would mean nothing could ever be recorded.
    std::fs::write(&path, "{}").unwrap();
    lattice::overlay::record_install(&path, &manifest, "greeter", None, &[wire])
        .expect("an empty overlay is a valid overlay");
}
