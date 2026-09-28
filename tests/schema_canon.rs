//! The machine-readable contract canon, pinned to the implementation.
//!
//! schemas/envelope.json, schemas/component_manifest.json and
//! schemas/assembly_manifest.json are the language-neutral canon of the three
//! top-level contract documents (docs/contracts/ is the human-readable half).
//! This suite pins the canon from both directions: everything the Rust types
//! serialize must validate against it, and shapes the canon rejects must be
//! genuinely out of contract (so the schemas keep their teeth). The Ink
//! golden wire file is validated too: the envelopes embedded in it must both
//! satisfy the canon and round-trip through the Rust types unchanged, so the
//! hand-aligned JS client, the Rust types and the canon cannot drift apart
//! silently.

use serde_json::{json, Value};

use lattice::components::{
    anthropic_model, context_gate, effects_policy, fs_tools, fs_watch, minimal_loop, net_tools,
    openai_model, scripted_model, shell_tools, silent_ui, skill_library, timer_tools, trust_policy,
    workshop_sink,
};
use lattice::preset::{standard, PresetConfig};
use lattice::{deferred_dispatcher_decl, ComponentManifest, EventEnvelope};

fn validator(source: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(source).expect("canon schema files are valid JSON");
    jsonschema::validator_for(&schema).expect("canon schema files compile")
}

fn envelope_validator() -> jsonschema::Validator {
    validator(include_str!("../schemas/envelope.json"))
}

fn component_validator() -> jsonschema::Validator {
    validator(include_str!("../schemas/component_manifest.json"))
}

fn assembly_validator() -> jsonschema::Validator {
    validator(include_str!("../schemas/assembly_manifest.json"))
}

fn assert_valid(validator: &jsonschema::Validator, instance: &Value, what: &str) {
    if let Err(error) = validator.validate(instance) {
        panic!("{what} violates the canon: {error}");
    }
}

fn assert_rejected(validator: &jsonschema::Validator, instance: &Value, what: &str) {
    assert!(
        !validator.is_valid(instance),
        "the canon should reject {what}, but accepted it"
    );
}

fn scripted_cfg() -> PresetConfig {
    PresetConfig {
        adapter: "scripted".to_string(),
        model: "scripted".to_string(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some("./workspace".to_string()),
        context_window: 64000,
        usage_input_field: "input_tokens".to_string(),
        profile: None,
        catalog_problems: Vec::new(),
        system: "test".to_string(),
        scripted: Some(json!({"script": []})),
        thinking: None,
        overlay: None,
        assembly: None,
    }
}

// ── Envelope ────────────────────────────────────────────

#[test]
fn envelope_canon_accepts_what_rust_serializes() {
    let canon = envelope_validator();
    let full = EventEnvelope {
        v: 1,
        id: "ev_7_a1b2c3d4".to_string(),
        seq: 7,
        stream: "main".to_string(),
        time: "2026-07-21T00:00:00Z".to_string(),
        event_type: "core.input.user_message".to_string(),
        source: "ui".to_string(),
        causes: vec!["ev_5".to_string(), "ev_6".to_string()],
        origin: Some(lattice::StreamRef {
            stream: "parent".to_string(),
            event: "ev_3".to_string(),
        }),
        reason: Some("a decision needs a reason".to_string()),
        payload: json!({"text": "hello"}),
    };
    assert_valid(
        &canon,
        &serde_json::to_value(&full).unwrap(),
        "a fully-populated envelope",
    );

    let minimal = EventEnvelope {
        origin: None,
        reason: None,
        causes: vec![],
        ..full
    };
    assert_valid(
        &canon,
        &serde_json::to_value(&minimal).unwrap(),
        "a minimal root-event envelope",
    );
}

#[test]
fn envelope_canon_rejects_out_of_contract_shapes() {
    let canon = envelope_validator();
    let good = json!({
        "v": 1, "id": "ev_1", "seq": 1, "stream": "main",
        "time": "2026-07-21T00:00:00Z", "type": "core.input.user_message",
        "source": "ui", "causes": [], "payload": {}
    });
    assert_valid(&canon, &good, "the reference envelope");

    let mut missing_causes = good.clone();
    missing_causes.as_object_mut().unwrap().remove("causes");
    assert_rejected(&canon, &missing_causes, "an envelope without causes");

    let mut zero_seq = good.clone();
    zero_seq["seq"] = json!(0);
    assert_rejected(&canon, &zero_seq, "seq 0 (numbering starts at 1)");

    let mut non_string_cause = good.clone();
    non_string_cause["causes"] = json!([42]);
    assert_rejected(&canon, &non_string_cause, "a non-string cause id");

    let mut half_origin = good.clone();
    half_origin["origin"] = json!({"stream": "parent"});
    assert_rejected(&canon, &half_origin, "an origin without the event half");
}

#[test]
fn golden_wire_envelopes_satisfy_the_canon_and_round_trip() {
    let canon = envelope_validator();
    let golden = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("clients/ink/test/golden/wire.jsonl"),
    )
    .expect("golden wire file exists");

    let mut envelopes: Vec<Value> = Vec::new();
    for line in golden.lines().filter(|l| !l.trim().is_empty()) {
        let entry: Value = serde_json::from_str(line).expect("golden lines are valid JSON");
        let message = &entry["message"];
        if let Some(replay) = message["attached"]["replay"].as_array() {
            envelopes.extend(replay.iter().cloned());
        }
        if message["appended"]["event"].is_object() {
            envelopes.push(message["appended"]["event"].clone());
        }
    }
    assert!(
        envelopes.len() >= 2,
        "expected at least the replay and appended envelopes in the golden file, found {}",
        envelopes.len()
    );

    for envelope in &envelopes {
        assert_valid(&canon, envelope, "a golden wire envelope");
        // Round-trip through the Rust type: parsing then re-serializing must
        // reproduce the pinned JSON exactly, or serde and the canon disagree
        // about some field the JS client is pinned to.
        let typed: EventEnvelope =
            serde_json::from_value(envelope.clone()).expect("golden envelope parses via serde");
        assert_eq!(
            &serde_json::to_value(&typed).unwrap(),
            envelope,
            "serde round-trip changed a golden envelope"
        );
    }
}

// ── Component manifest ──────────────────────────────────

#[test]
fn every_builtin_manifest_satisfies_the_canon() {
    let canon = component_validator();
    let manifests: Vec<ComponentManifest> = vec![
        silent_ui::manifest(),
        minimal_loop::manifest(),
        context_gate::manifest(),
        scripted_model::manifest(),
        anthropic_model::manifest(),
        openai_model::manifest(),
        lattice::components::responses_model::manifest(),
        fs_tools::manifest(),
        shell_tools::manifest(),
        net_tools::manifest(),
        timer_tools::manifest(),
        fs_watch::manifest(),
        workshop_sink::manifest(),
        effects_policy::manifest(),
        skill_library::manifest(),
        trust_policy::manifest(),
    ];
    for manifest in manifests {
        let name = manifest.name.clone();
        assert_valid(
            &canon,
            &serde_json::to_value(&manifest).unwrap(),
            &format!("builtin manifest \"{name}\""),
        );
    }
}

#[test]
fn dispatcher_declaration_satisfies_the_tool_canon() {
    let canon = component_validator();
    let carrier = json!({
        "name": "carrier", "version": "0", "runtime": "inproc", "entry": "builtin:carrier",
        "tools": [deferred_dispatcher_decl()]
    });
    assert_valid(&canon, &carrier, "the deferred-dispatcher tool declaration");
}

#[test]
fn component_canon_accepts_a_foreign_minimal_manifest() {
    let canon = component_validator();
    // What a foreign-language author writes by hand: only the required four.
    let minimal = json!({
        "name": "py-tools", "version": "0.1.0",
        "runtime": "process", "entry": "python3 tools.py"
    });
    assert_valid(&canon, &minimal, "a minimal process-component manifest");
    let typed: ComponentManifest =
        serde_json::from_value(minimal).expect("a canon-valid manifest parses via serde");
    assert_eq!(typed.runtime, lattice::RuntimeKind::Process);
}

#[test]
fn component_canon_rejects_out_of_contract_shapes() {
    let canon = component_validator();
    let good = json!({
        "name": "x", "version": "0", "runtime": "inproc", "entry": "builtin:x"
    });
    assert_valid(&canon, &good, "the reference manifest");

    let mut unknown_runtime = good.clone();
    unknown_runtime["runtime"] = json!("wasm");
    assert_rejected(&canon, &unknown_runtime, "an unknown runtime kind");

    let mut missing_entry = good.clone();
    missing_entry.as_object_mut().unwrap().remove("entry");
    assert_rejected(&canon, &missing_entry, "a manifest without entry");

    let mut star_output = good.clone();
    star_output["outputs"] = json!([{"name": "out", "events": ["*"]}]);
    assert_rejected(&canon, &star_output, "an output port promising \"*\"");

    let mut nameless_tool = good.clone();
    nameless_tool["tools"] = json!([{"description": "who am I"}]);
    assert_rejected(&canon, &nameless_tool, "a tool declaration without a name");

    let mut bad_async = good.clone();
    bad_async["tools"] = json!([{"name": "t", "async": "sometimes"}]);
    assert_rejected(&canon, &bad_async, "an async value outside the enum");
}

// ── Assembly manifest ───────────────────────────────────

#[test]
fn the_standard_assembly_satisfies_the_canon() {
    let canon = assembly_validator();
    let (registry, _factories, assembly) = standard(&scripted_cfg()).expect("preset builds");
    assert_valid(
        &canon,
        &serde_json::to_value(&assembly).unwrap(),
        "the standard assembly manifest",
    );
    // The registry the standard assembly ships with must satisfy the
    // component canon too — same data, seen through the other document.
    let component_canon = component_validator();
    for (name, manifest) in registry {
        assert_valid(
            &component_canon,
            &serde_json::to_value(&manifest).unwrap(),
            &format!("standard-registry manifest \"{name}\""),
        );
    }
}

#[test]
fn assembly_canon_accepts_a_handwritten_manifest() {
    let canon = assembly_validator();
    let handwritten = json!({
        "instances": {
            "ui": {"component": "silent-ui"},
            "loop": {"component": "minimal-loop", "config": {"maxDispatches": 100}}
        },
        "wires": [{"from": "ui.outgoing", "to": "loop.incoming"}]
    });
    assert_valid(&canon, &handwritten, "a handwritten assembly manifest");
    let typed: lattice::AssemblyManifest =
        serde_json::from_value(handwritten).expect("a canon-valid assembly parses via serde");
    assert_eq!(typed.instances.len(), 2);
    assert_eq!(typed.wires.len(), 1);
}

#[test]
fn assembly_canon_rejects_out_of_contract_shapes() {
    let canon = assembly_validator();
    let good = json!({
        "instances": {"ui": {"component": "silent-ui"}},
        "wires": [{"from": "ui.out", "to": "ui.in"}]
    });
    assert_valid(&canon, &good, "the reference assembly");

    let reserved = json!({
        "instances": {"core": {"component": "silent-ui"}},
        "wires": []
    });
    assert_rejected(&canon, &reserved, "an instance named \"core\"");

    let mut dotless = good.clone();
    dotless["wires"] = json!([{"from": "ui", "to": "ui.in"}]);
    assert_rejected(
        &canon,
        &dotless,
        "an endpoint without \"instance.port\" form",
    );

    let mut componentless = good.clone();
    componentless["instances"]["ui"] = json!({"config": {}});
    assert_rejected(&canon, &componentless, "an instance without a component");

    let mut wireless = good.clone();
    wireless.as_object_mut().unwrap().remove("wires");
    assert_rejected(&canon, &wireless, "a manifest without the wires field");

    let mut bounded = good.clone();
    bounded["instances"]["ui"] = json!({"component": "silent-ui", "requires": ["frontend"]});
    assert_valid(&canon, &bounded, "an instance carrying a slot bound");

    let mut stringly = good.clone();
    stringly["instances"]["ui"] = json!({"component": "silent-ui", "requires": "frontend"});
    assert_rejected(&canon, &stringly, "a requires bound that is not an array");
}

// ── The user-directory canon ───────────────────────────────────────────────
//
// Six schemas describe the files a person (or the agent) writes into
// ~/.lattice. None of them was pinned here, and the cost of that showed up
// exactly where it always does: `preferences.json` had a `oneOf` where two
// branches both matched every legal rung, so the canon rejected the values it
// was written to bless and accepted the ones it called an escape hatch. It
// had been wrong since it was written, because nothing ever loaded it.

fn preferences_validator() -> jsonschema::Validator {
    validator(include_str!("../schemas/preferences.json"))
}

#[test]
fn the_preferences_canon_blesses_every_rung_and_the_saved_model() {
    let canon = preferences_validator();

    assert_valid(&canon, &json!({}), "an empty document");
    assert_valid(&canon, &json!({"thinking": false}), "thinking turned off");
    for rung in lattice::components::model_common::RUNGS {
        assert_valid(
            &canon,
            &json!({ "thinking": rung }),
            &format!("the {rung} rung"),
        );
    }
    assert_valid(
        &canon,
        &json!({"thinking": "provider-specific-word"}),
        "a word off the ladder (the escape hatch)",
    );
    assert_valid(&canon, &json!({"model": "sonnet"}), "a saved model choice");
    assert_valid(
        &canon,
        &json!({"unknown-to-this-version": 1}),
        "a key this version does not know",
    );

    assert_rejected(&canon, &json!({"thinking": 3}), "a numeric thinking rung");
    assert_rejected(&canon, &json!({"model": 7}), "a numeric model name");
    assert_rejected(&canon, &json!([]), "a document that is not an object");
}

/// What this installation actually writes has to satisfy it, or the canon is
/// describing a file that does not exist.
#[test]
fn what_the_preferences_writer_produces_satisfies_its_canon() {
    let canon = preferences_validator();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preferences.json");
    std::env::set_var("LATTICE_PREFERENCES", &path);
    lattice::preferences::set("thinking", json!("high")).unwrap();
    lattice::preferences::set("model", json!("deepseek")).unwrap();
    std::env::remove_var("LATTICE_PREFERENCES");

    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_valid(&canon, &written, "the document this version writes");
}

#[test]
fn the_model_catalog_canon_matches_what_the_loader_accepts() {
    let canon = validator(include_str!("../schemas/model_catalog.json"));

    let good = json!({"models": {"kimi": {
        "adapter": "openai",
        "model": "kimi-k2",
        "baseUrl": "https://api.moonshot.cn/v1",
        "apiKeyEnv": "MOONSHOT_API_KEY",
        "profile": {"contextWindow": 256000, "effort": ["low", "high"]},
    }}});
    assert_valid(&canon, &good, "a hand-written catalog entry");
    assert_valid(&canon, &json!({"models": {}}), "an empty catalog");

    let mut nameless = good.clone();
    nameless["models"]["kimi"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    assert_rejected(&canon, &nameless, "an entry naming no model");

    let mut misspelt = good.clone();
    misspelt["models"]["kimi"]["baseurl"] = json!("https://x");
    assert_rejected(&canon, &misspelt, "a misspelt field");

    let mut shouted = good.clone();
    let entry = shouted["models"]["kimi"].clone();
    shouted["models"] = json!({ "Kimi K2": entry });
    assert_rejected(&canon, &shouted, "a short name that is not a short name");

    // A key may be written here, deliberately: the alternative left a person
    // with nowhere to put a variable unable to configure a model at all. What
    // it costs is documented in the canon rather than forbidden — this is a
    // file the agent can read, and a tool result is on the ledger forever.
    // The loader keeps the value out of everything downstream by staging it
    // into the process environment, so only a NAME ever travels onward.
    let mut inline = good.clone();
    inline["models"]["kimi"]["apiKey"] = json!("sk-live-yes");
    assert_valid(&canon, &inline, "an entry carrying its own key");

    let mut both = good.clone();
    both["models"]["kimi"]["apiKey"] = json!("sk-live-yes");
    both["models"]["kimi"]["apiKeyEnv"] = json!("SOMEWHERE");
    assert_valid(&canon, &both, "an entry naming both (the literal wins)");
}

#[test]
fn the_trust_grant_canon_matches_what_the_gate_writes() {
    let canon = validator(include_str!("../schemas/trust_grants.json"));
    assert_valid(&canon, &json!({"grants": []}), "a store with no grants yet");

    // What a granting really writes is checked where one really happens —
    // `tests/trust.rs`, which drives the gate through a kernel. Here: the
    // shapes the canon must refuse, so it keeps its teeth.
    assert_rejected(&canon, &json!({"grants": {}}), "grants that are not a list");
    assert_rejected(&canon, &json!({}), "a store with no grants field");
    assert_rejected(
        &canon,
        &json!({"grants": [{"effects": {}}]}),
        "a grant with no key",
    );
}

#[test]
fn the_overlay_canon_keeps_its_teeth() {
    let canon = validator(include_str!("../schemas/assembly_overlay.json"));
    assert_valid(&canon, &json!({}), "an empty overlay");

    let mut componentless = json!({"instances": {"extra": {"config": {}}}});
    assert_rejected(&canon, &componentless, "an instance naming no component");
    componentless["instances"]["extra"]["component"] = json!("greeter");
    assert_valid(&canon, &componentless, "an instance naming its component");

    assert_rejected(
        &canon,
        &json!({"wires": [{"from": "loop.run"}]}),
        "a wire with only one end",
    );
    assert_rejected(&canon, &json!({"wires": {}}), "wires that are not a list");
}

#[test]
fn the_skill_frontmatter_canon_matches_the_name_rule_the_loader_enforces() {
    let canon = validator(include_str!("../schemas/skill_frontmatter.json"));
    assert_valid(
        &canon,
        &json!({"name": "pdf-forms", "description": "Fill in PDF forms."}),
        "an ordinary skill header",
    );
    assert_rejected(
        &canon,
        &json!({"name": "pdf-forms"}),
        "a header with no description",
    );

    // The canon's name rule and the loader's must agree, or a name the canon
    // blesses is one the loader will not open (and the other way round is a
    // way out of the library — see `resolve`).
    for name in ["pdf-forms", "a1", "one-two-three"] {
        assert_valid(
            &canon,
            &json!({"name": name, "description": "x"}),
            &format!("{name} in the canon"),
        );
        assert!(
            lattice::components::skill_library::is_skill_name(name),
            "{name} must be loadable too"
        );
    }
    for name in [
        "/etc/passwd",
        "../up",
        "Upper",
        "has space",
        "double--hyphen",
    ] {
        assert_rejected(
            &canon,
            &json!({"name": name, "description": "x"}),
            &format!("{name} in the canon"),
        );
        assert!(
            !lattice::components::skill_library::is_skill_name(name),
            "{name} must not be loadable either"
        );
    }
}

/// A field on one side and not the other is drift the canon cannot see.
///
/// `additionalProperties` is deliberately open here (foreign components may
/// carry more than this version knows), which is exactly why "every built-in
/// manifest validates" could not catch either of these: `concurrency` existed
/// in Rust and five built-ins used it while the canon never mentioned it — so
/// a component author reading the canon could not learn about the only switch
/// that lets a tool serve several calls at once. And `admits` existed in the
/// canon while the Rust type had no field for it, so the typed path dropped
/// it silently: a component declaring that it admits code round-tripped
/// through an overlay declaring that it admits nothing.
#[test]
fn the_canon_and_the_types_name_the_same_fields() {
    let canon: Value = serde_json::from_str(include_str!("../schemas/component_manifest.json"))
        .expect("the canon parses");
    let named = |pointer: &str| -> Vec<String> {
        canon
            .pointer(pointer)
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };

    for field in ["concurrency", "handleTimeoutMs", "implements", "tools"] {
        assert!(
            named("/properties").iter().any(|k| k == field),
            "the canon must document {field} — a component author has only it to read"
        );
    }

    // Serializing a surface that uses every dimension shows what the type can
    // say; the canon must have a word for each.
    let full = serde_json::to_value(lattice::EffectSurface {
        reads: vec!["*".to_string()],
        writes: vec!["*".to_string()],
        network: vec!["*".to_string()],
        executes: true,
        reversible: true,
        admits: Some("components".to_string()),
    })
    .unwrap();
    let surface_fields = named("/$defs/effectSurface/properties");
    for key in full.as_object().unwrap().keys() {
        assert!(
            surface_fields.iter().any(|k| k == key),
            "the effect surface canon is missing {key}, which the type serializes"
        );
    }
    // And the other direction: nothing the canon names may vanish on the way
    // through the type.
    let round_tripped: lattice::EffectSurface = serde_json::from_value(full.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(round_tripped).unwrap(),
        full,
        "a surface must survive a round trip through the Rust type intact"
    );
}
