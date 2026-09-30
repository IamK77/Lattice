//! The standard assembly (src/preset.rs) is what every frontend runs, so it
//! must always be a VALID, runnable agent. This locks it: build it keyless,
//! start a kernel (inspection would reject bad wiring), drive one turn, and
//! see the reply — the same assembly the TUI and the daemon use.
#![cfg(unix)]

use serde_json::json;

use lattice::core_events as ce;
use lattice::preset::{self, standard, PresetConfig};
use lattice::{EventDraft, Kernel, KernelOptions};

fn scripted_config() -> PresetConfig {
    PresetConfig {
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
        scripted: Some(json!({"script": [{"status": "ok", "text": "standard assembly reply"}]})),
        thinking: None,
        overlay: None,
        assembly: None,
    }
}

#[test]
fn complete_assembly_replaces_baseline_and_keeps_installs_separate() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = scripted_config();
    let mut document = lattice::preset::assembly_document(&cfg).unwrap();
    document["assembly"]["instances"]
        .as_object_mut()
        .unwrap()
        .remove("net");
    document["assembly"]["wires"]
        .as_array_mut()
        .unwrap()
        .retain(|w| {
            !w["from"].as_str().unwrap().starts_with("net.")
                && !w["to"].as_str().unwrap().starts_with("net.")
        });
    document["assembly"]["instances"]["loop"]["config"] = json!({"custom": true});
    let path = dir.path().join("baseline.json");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    cfg.assembly = Some(path);
    let overlay = dir.path().join("installs.json");
    std::fs::write(
        &overlay,
        serde_json::to_vec(&json!({
            "instances": {"my-ui": {"component": lattice::components::silent_ui::NAME}}, "wires": []
        }))
        .unwrap(),
    )
    .unwrap();
    cfg.overlay = Some(overlay);
    let (_, _, assembly) = standard(&cfg).unwrap();
    assert!(
        !assembly.instances.contains_key("net"),
        "missing baseline slots must not grow back"
    );
    assert_eq!(
        assembly.instances["loop"].config.as_ref().unwrap()["custom"],
        true
    );
    assert!(assembly.instances.contains_key("my-ui"));
    assert_eq!(
        assembly.instances["workshop"].config.as_ref().unwrap()["installedInstances"],
        json!(["my-ui"])
    );
    let exported = lattice::preset::assembly_document(&cfg).unwrap();
    assert!(exported["assembly"]["instances"].get("my-ui").is_none());
}

#[test]
fn complete_assembly_runtime_slots_follow_model_switch_and_resume_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = scripted_config();
    let document = preset::assembly_document(&cfg).unwrap();
    let path = dir.path().join("baseline.json");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    cfg.assembly = Some(path);
    cfg.adapter = "openai".into();
    cfg.model = "changed-model".into();
    cfg.base_url = "https://example.invalid".into();
    cfg.key_env = "EXAMPLE_KEY".into();
    cfg.thinking = Some(json!(false));
    cfg.context_window = 123456;
    let (_, _, resolved) = standard(&cfg).unwrap();
    assert_eq!(
        resolved.instances["model"].component,
        lattice::components::openai_model::NAME
    );
    assert_eq!(
        resolved.instances["model"].config.as_ref().unwrap()["model"],
        "changed-model"
    );
    assert_eq!(
        resolved.instances["ctx"].config.as_ref().unwrap()["profile"]["contextWindow"],
        123456
    );
    assert_eq!(
        resolved.instances["cmodel"].component,
        lattice::components::openai_model::NAME
    );
}

#[test]
fn complete_assembly_can_insert_a_required_gate_without_the_old_bypass() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = scripted_config();
    cfg.scripted = Some(json!({"script": [
        {"status":"ok", "toolCalls":[{"id":"list", "tool":"ask", "arguments":{"prompt":"list experts"}}]},
        {"status":"ok", "text":"done"}
    ]}));
    let mut document = preset::assembly_document(&cfg).unwrap();
    document["assembly"]["instances"]["extra-gate"] = json!({
        "component": lattice::components::trust_policy::NAME, "config":{"stance":"deny"}, "requires":["policy"]
    });
    let wires = document["assembly"]["wires"].as_array_mut().unwrap();
    wires.retain(|w| !(w["from"] == "loop.run" && w["to"] == "expert-review.review"));
    wires.extend([
        json!({"from":"loop.run", "to":"extra-gate.review"}),
        json!({"from":"extra-gate.forward", "to":"expert-review.review"}),
        json!({"from":"extra-gate.verdict", "to":"loop.tools"}),
    ]);
    let path = dir.path().join("gated.json");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    cfg.assembly = Some(path);
    let (registry, mut factories, assembly) = standard(&cfg).unwrap();
    assert!(!assembly
        .wires
        .iter()
        .any(|w| w.from == "loop.run" && w.to == "expert-review.review"));
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"list"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    let requests: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_STARTED)
        .collect();
    assert_eq!(
        requests
            .iter()
            .map(|e| e.source.as_str())
            .collect::<Vec<_>>(),
        ["loop", "extra-gate", "expert-review", "trust"]
    );
    for pair in requests.windows(2) {
        assert_eq!(pair[1].causes, vec![pair[0].id.clone()]);
    }
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "done"));
    kernel.shutdown();
}

#[test]
fn complete_assembly_errors_never_fall_back_to_builtin_wiring() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = scripted_config();
    let path = dir.path().join("chosen.json");
    cfg.assembly = Some(path.clone());
    assert!(
        standard(&cfg).is_err(),
        "a selected missing document must fail"
    );
    for bad in [
        json!({}),
        json!({"assembly": {"instances": {}, "wires": [{"from":"absent.out", "to":"absent.in"}]}}),
    ] {
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(standard(&cfg).is_err(), "invalid baseline must fail: {bad}");
    }
    let mut document = lattice::preset::assembly_document(&scripted_config()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    cfg.overlay = Some(path.clone());
    assert!(standard(&cfg).err().unwrap().contains("separate files"));
    cfg.overlay = None;
    let mut missing_slot = document.clone();
    missing_slot["assembly"]["instances"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    std::fs::write(&path, serde_json::to_vec(&missing_slot).unwrap()).unwrap();
    assert!(standard(&cfg).err().unwrap().contains("runtime slot"));
    document["assembly"]["wires"]
        .as_array_mut()
        .unwrap()
        .retain(|w| w["to"] != "trust.answer");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let error = standard(&cfg).err().unwrap();
    assert!(error.contains("contradiction"), "{error}");
}

#[test]
fn assembly_command_exports_and_rejects_a_broken_selected_file() {
    let dir = tempfile::tempdir().unwrap();
    let run = |selected: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_lattice"))
            .arg("assembly")
            .env_clear()
            .env("HOME", dir.path())
            .env("LATTICE_SCRIPTED", "1")
            .env("LATTICE_OVERLAY", "")
            .env("LATTICE_ASSEMBLY", selected)
            .output()
            .unwrap()
    };
    let exported = run(std::path::Path::new(""));
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(document["runtimeSlots"], json!(["model", "cmodel", "ctx"]));
    let path = dir.path().join("baseline.json");
    std::fs::write(&path, &exported.stdout).unwrap();
    let round_trip = run(&path);
    assert!(
        round_trip.status.success(),
        "{}",
        String::from_utf8_lossy(&round_trip.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&round_trip.stdout).unwrap(),
        document
    );
    std::fs::write(&path, "{").unwrap();
    let rejected = run(&path);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("complete assembly"));
    assert!(
        rejected.stdout.is_empty(),
        "failure must not print a fallback baseline"
    );
}

/// The thinking knob splits the two brains: the main model reasons, the
/// condenser is told not to. Reformatting history into four sections needs no
/// reasoning, and its budget is 1024 tokens — thinking would eat it before the
/// summary began. Left unset, NEITHER instance says anything about thinking,
/// so an endpoint that rejects the parameter never meets it.
/// The model's own effort rungs have to REACH the adapter, or the ladder is
/// decoration: nearest-matching would run on an empty list, fall back to the
/// dialect's defaults, and quietly aim every request at the wrong rung. The
/// profile files sat in the repository read by nothing before this, so the
/// wiring is the part worth pinning, not the file.
/// The ceiling on a reply travels with the model, like its window does.
///
/// Every shipped profile carries `maxOutputTokens` and nothing read it, so
/// every call went out with the adapter's fallback of 4096. With thinking on
/// — where the thought is charged to the same budget — a long turn hit that
/// and came back cut off, from a model that had told us it could write
/// sixteen times as much.
#[test]
fn the_models_own_output_ceiling_reaches_the_adapter() {
    let entry = lattice::models::Entry {
        id: "roomy".to_string(),
        adapter: "openai".to_string(),
        model: "roomy-1".to_string(),
        base_url: "https://example.invalid".to_string(),
        key_env: "SOME_KEY".to_string(),
        profile: Some(serde_json::json!({"maxOutputTokens": 64000})),
    };
    let config = lattice::preset::main_model_config(&entry, None);
    assert_eq!(config["maxTokens"], 64000);

    // A model that says nothing about it says nothing here either — the
    // adapter keeps its own fallback rather than being handed a guess.
    let quiet = lattice::models::Entry {
        profile: None,
        ..entry
    };
    assert!(
        lattice::preset::main_model_config(&quiet, None)
            .get("maxTokens")
            .is_none(),
        "an unstated ceiling must not become a stated one"
    );
}

#[test]
fn the_models_own_rungs_reach_the_adapter_that_will_send_them() {
    let mut cfg = scripted_config();
    cfg.adapter = "openai".to_string();
    cfg.scripted = None;
    cfg.model = "deepseek-v4-flash".to_string();
    let (_, _, assembly) = preset::standard(&cfg).expect("preset builds");
    let config = assembly.instances["model"].config.clone().unwrap();
    assert_eq!(
        config["effort"],
        json!(["high", "max"]),
        "the adapter must be told what this model actually offers"
    );

    // A model nobody shipped a profile for says nothing rather than a guess,
    // and the adapter falls back to its own wire defaults.
    cfg.model = "some-model-nobody-shipped".to_string();
    let (_, _, assembly) = preset::standard(&cfg).expect("preset builds");
    let config = assembly.instances["model"].config.clone().unwrap();
    assert!(
        config.get("effort").is_none(),
        "an unknown model must not be handed an invented ladder"
    );
}

#[test]
fn thinking_is_on_for_the_main_brain_and_off_for_the_condenser() {
    let mut cfg = scripted_config();
    cfg.adapter = "openai".to_string();
    cfg.scripted = None;
    cfg.thinking = Some(json!("high"));
    let (_, _, assembly) = preset::standard(&cfg).expect("preset builds");
    let config = |name: &str| assembly.instances[name].config.clone().unwrap();
    assert_eq!(config("model")["thinking"], "high");
    assert_eq!(config("cmodel")["thinking"], false);

    cfg.thinking = None;
    let (_, _, assembly) = preset::standard(&cfg).expect("preset builds");
    let config = |name: &str| assembly.instances[name].config.clone().unwrap();
    assert!(config("model").get("thinking").is_none());
    assert!(config("cmodel").get("thinking").is_none());
}

#[test]
fn the_standard_assembly_starts_and_runs_a_turn() {
    let cfg = scripted_config();
    let (registry, mut factories, assembly) = preset::standard(&cfg).expect("preset builds");

    // Kernel::start runs full inspection — a mis-wire in the shared assembly
    // fails right here, catching drift before any frontend sees it
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the standard assembly must pass inspection");

    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "hello"})),
    );
    kernel.run_until_quiescent().unwrap();

    let replied = kernel.log().replay(1).unwrap().iter().any(|e| {
        e.event_type == ce::OUTPUT_REPLY && e.payload["text"] == "standard assembly reply"
    });
    assert!(replied, "the standard assembly must complete a turn");

    // The wake sources are wired: shell.wake and timer.wake reach loop.input
    // (proven structurally by inspection passing above with those wires; here
    // we just confirm the tool providers are present and offered)
    let events = kernel.log().replay(1).unwrap();
    let ask = events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .unwrap();
    let tools = ask.payload["tools"].as_array().unwrap();
    for expected in [
        "Read",
        "Run",
        "Fetch",
        "Schedule",
        "Watch",
        "LoadSkill",
        "Desktop",
    ] {
        assert!(
            tools.iter().any(|t| t["name"] == expected),
            "the standard assembly must offer {expected}"
        );
    }

    kernel.shutdown();
}

#[test]
fn product_expert_chat_redacts_startup_credentials_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let key = "LATTICE_TEST_EXPERT_AUDIT_KEY";
    let secret = "fake-expert-audit-secret-92837";
    std::env::set_var(key, secret);
    let mut cfg = scripted_config();
    cfg.key_env = key.into();
    cfg.scripted = Some(json!({"script": [{"status": "ok", "text": secret}]}));
    let path = dir.path().join("expert.jsonl");
    let destination = path.clone();
    let mut host = preset::expert_host(&cfg).with_ledger_path(move |_| Some(destination.clone()));
    std::env::remove_var(key);
    host.open("audit-expert", "explorer").unwrap();
    host.injector("audit-expert", "ui").unwrap().emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": "repeat the scripted reply"}),
        ),
    );
    host.run_all().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.iter().any(|e| e["type"] == ce::OUTPUT_REPLY),
        "the expert actually answered"
    );
    assert!(
        !text.contains(secret),
        "expert chat must not persist the startup key"
    );
}

#[test]
fn workshop_overlay_environment_is_shared_with_startup() {
    const CHILD: &str = "LATTICE_TEST_OVERLAY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let shop = lattice::workshop::Workshop::standard();
        let config = PresetConfig::from_env();
        assert_eq!(shop.overlay, config.overlay);
        let value = std::env::var("LATTICE_OVERLAY").unwrap();
        assert_eq!(
            shop.overlay,
            if value.is_empty() {
                None
            } else {
                Some(value.into())
            }
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for value in [
        String::new(),
        dir.path().join("custom.json").display().to_string(),
    ] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workshop_overlay_environment_is_shared_with_startup",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", dir.path())
            .env("LATTICE_OVERLAY", value)
            .env("LATTICE_ADAPTER", "scripted")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert!(!dir.path().join(".lattice/assembly.json").exists());
}

#[test]
fn only_a_successfully_merged_overlay_grants_removal_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("assembly.json");
    let mut cfg = scripted_config();
    cfg.overlay = Some(path.clone());
    for (instance, expected) in [("trust", json!([])), ("extra", json!(["extra"]))] {
        std::fs::write(&path, serde_json::to_vec(&json!({
            "instances": {(instance): {"component": "trust-policy", "config": {"stance": "deny"}}},
            "wires": []
        })).unwrap()).unwrap();
        let (registry, mut factories, assembly) = preset::standard(&cfg).unwrap();
        assert_eq!(
            assembly.instances["workshop"].config.as_ref().unwrap()["installedInstances"],
            expected
        );
        let mut kernel = Kernel::start(
            &assembly,
            &registry,
            &mut factories,
            KernelOptions::default(),
        )
        .unwrap();
        let shop = lattice::workshop::Workshop {
            workshop_dir: dir.path().join("workshop"),
            components_dir: dir.path().join("components"),
            loop_instance: "loop".into(),
            overlay: Some(path.clone()),
        };
        // A later forged file must not add authority to the running assembly.
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "instances": {"trust": {"component": "trust-policy"}}, "wires": []
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(shop.remove(&mut kernel, "trust").is_err());
        assert!(kernel.assembly().instances.contains_key("trust"));
        if instance == "extra" {
            shop.remove(&mut kernel, "extra").unwrap();
            assert!(!kernel.assembly().instances.contains_key("extra"));
        }
        kernel.shutdown();
    }
}

/// The assembler's stance-ask rule: an overlay may add a second trust gate,
/// but if nothing that can answer is wired into it, its questions would hang
/// forever — the preset must refuse to build, saying so plainly.
#[test]
fn an_ask_gate_nobody_answers_fails_the_build() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = dir.path().join("assembly.json");
    std::fs::write(
        &overlay,
        serde_json::to_string_pretty(&json!({
            "instances": {"trust2": {"component": "trust-policy", "config": {"stance": "ask"}}},
            "wires": [],
        }))
        .unwrap(),
    )
    .unwrap();

    let mut cfg = scripted_config();
    cfg.overlay = Some(overlay);
    let err = match preset::standard(&cfg) {
        Err(err) => err,
        Ok(_) => panic!("an unanswerable ask gate must fail the build"),
    };
    assert!(err.contains("trust2"), "{err}");
    assert!(err.contains("frontend-authorize"), "{err}");
}

/// Same overlay plus the answer wire from a frontend that claims the
/// authorize capability: the contradiction is gone, the build succeeds.
#[test]
fn an_ask_gate_with_an_answering_frontend_builds() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = dir.path().join("assembly.json");
    std::fs::write(
        &overlay,
        serde_json::to_string_pretty(&json!({
            "instances": {"trust2": {"component": "trust-policy", "config": {"stance": "ask"}}},
            "wires": [{"from": "ui.answer", "to": "trust2.answer"}],
        }))
        .unwrap(),
    )
    .unwrap();

    let mut cfg = scripted_config();
    cfg.overlay = Some(overlay);
    preset::standard(&cfg).expect("an answered ask gate builds");
}

/// The standard assembly's slots carry bounds: hand-editing the frontend
/// slot to a component that never claimed the frontend profile fails
/// inspection with a message naming the profile — not just port-mismatch
/// noise.
#[test]
fn the_standard_assembly_slots_carry_bounds() {
    let cfg = scripted_config();
    let (registry, _factories, mut assembly) = preset::standard(&cfg).expect("preset builds");
    assembly.instances.get_mut("ui").unwrap().component =
        lattice::components::workshop_sink::NAME.to_string();
    let issues = lattice::inspect_assembly(&assembly, &registry);
    assert!(
        issues
            .iter()
            .any(|i| i.problem.contains("frontend") && i.problem.contains("does not implement")),
        "{issues:?}"
    );
}

/// Confinement is opt-in. With no workspace configured — the default, since
/// LATTICE_WORKSPACE is normally unset — the file, search and shell tools get
/// no root at all and work wherever lattice was started. Setting one puts all
/// three back inside it, under whichever config key each of them names it.
#[test]
fn the_tools_are_unconfined_unless_a_workspace_is_configured() {
    let mut cfg = scripted_config();

    cfg.workspace = None;
    let (_, _, open) = preset::standard(&cfg).expect("preset builds");
    for instance in ["fs", "search", "shell"] {
        assert!(
            open.instances[instance].config.is_none(),
            "{instance} carries no confinement by default: {:?}",
            open.instances[instance].config
        );
    }

    cfg.workspace = Some("/srv/box".to_string());
    let (_, _, confined) = preset::standard(&cfg).expect("preset builds");
    for (instance, key) in [("fs", "root"), ("search", "root"), ("shell", "cwd")] {
        let config = confined.instances[instance]
            .config
            .as_ref()
            .unwrap_or_else(|| panic!("{instance} is confined"));
        assert_eq!(config[key], "/srv/box", "{instance}.{key}");
    }
}

// ── The system prompt ──────────────────────────────────────────────────────

/// The assembled fragments, without making a model call: `Kernel::start` runs
/// every component's `restore`, which is where the environment probes.
/// The agent is told where its own ledger is, by name.
///
/// The product prompt already told it that "the ledger is a file" and that
/// `Run` reaches it. That was true and unusable: asked to read its own
/// ledger, it searched for a tool, misused the one whose name mentioned the
/// ledger, and then went looking through `git log` for its own conversation.
/// The address is what turns the claim into something it can act on, and it
/// comes from the kernel rather than from config — the daemon builds one
/// assembly for many streams, each writing its own file.
#[test]
fn the_environment_fragment_names_this_conversations_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("main.jsonl");
    let cfg = scripted_config();
    let (registry, mut factories, assembly) = preset::standard(&cfg).expect("preset builds");
    let kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            log_file: Some(ledger.clone()),
            ..KernelOptions::default()
        },
    )
    .expect("the standard assembly must pass inspection");
    let fragments = kernel.prompt_fragments();
    kernel.shutdown();

    let env = fragments
        .iter()
        .find(|(instance, _)| instance == "env")
        .map(|(_, text)| text.clone())
        .expect("the environment fragment is present");
    assert!(
        env.contains(&ledger.display().to_string()),
        "the fragment must name the file: {env}"
    );
    assert!(
        env.contains("ev_42") || env.contains("line number"),
        "and say how to reach one event in it: {env}"
    );

    // A kernel with no ledger file must not invent a path
    let (registry, mut factories, assembly) = preset::standard(&cfg).expect("preset builds");
    let memory_only = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    let fragments = memory_only.prompt_fragments();
    memory_only.shutdown();
    let env = fragments
        .iter()
        .find(|(instance, _)| instance == "env")
        .map(|(_, text)| text.clone())
        .unwrap();
    assert!(
        !env.contains("ledger:"),
        "a memory-only run has no file to name: {env}"
    );
}

fn fragments_of(cfg: &PresetConfig) -> Vec<(String, String)> {
    let (registry, mut factories, assembly) = preset::standard(cfg).expect("preset builds");
    let kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the standard assembly must pass inspection");
    let fragments = kernel.prompt_fragments();
    kernel.shutdown();
    fragments
}

/// Every system prompt this assembly sent, in order.
fn systems_from_two_turns(cfg: &PresetConfig) -> Vec<String> {
    let (registry, mut factories, assembly) = preset::standard(cfg).expect("preset builds");
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the standard assembly must pass inspection");
    for text in ["first", "second"] {
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({ "text": text })),
        );
        kernel.run_until_quiescent().unwrap();
    }
    let systems: Vec<String> = kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter_map(|e| e.payload["system"].as_str().map(str::to_string))
        .collect();
    kernel.shutdown();
    systems
}

/// The system prompt is the cached prefix of every call: one changed character
/// and the provider recomputes all of it. So the invariant is not "the prompt
/// is good" but "the prompt HOLDS STILL" — anything that moves turn to turn
/// belongs in a tool result, not in here.
#[test]
fn the_system_prompt_is_byte_identical_from_one_turn_to_the_next() {
    // The product's own config, not a stand-in: this pins the shipped prompt.
    std::env::set_var("LATTICE_SCRIPTED", "1");
    let cfg = PresetConfig::from_env();
    let systems = systems_from_two_turns(&cfg);
    assert!(
        systems.len() >= 2,
        "two turns, two calls: {}",
        systems.len()
    );
    for (i, system) in systems.iter().enumerate() {
        assert_eq!(
            system, &systems[0],
            "turn {i} rewrote the cached prefix and cost every later call"
        );
    }
    assert!(
        systems[0].starts_with("# Who you are"),
        "the product layer opens it: {}",
        &systems[0][..60.min(systems[0].len())]
    );
}

/// The prompt is four parts in a stated order: who you are, who the work
/// belongs to, the setup you are working in, and the standing rules. The
/// order is the point — the base can only open the prompt and the component
/// fragments land in the middle, so the rules need the gate's tail to end up
/// last instead of being buried under the tool notes.
#[test]
fn the_system_prompt_reads_as_four_parts_in_order() {
    std::env::set_var("LATTICE_SCRIPTED", "1");
    let system = systems_from_two_turns(&PresetConfig::from_env()).remove(0);
    let mut at = 0;
    for heading in [
        "# Who you are",
        "# You and the person",
        "# Your setup",
        "# House rules",
    ] {
        let found = system[at..]
            .find(heading)
            .unwrap_or_else(|| panic!("{heading} is missing or out of order in:\n{system}"));
        at += found + heading.len();
    }
    // The setup block really is the components speaking, not an empty heading
    let setup = system
        .split("# House rules")
        .next()
        .unwrap()
        .split("# Your setup")
        .nth(1)
        .unwrap();
    assert!(setup.contains("Working directory:"), "{setup}");
    assert!(
        setup.contains("`Edit`"),
        "the tool notes are in it too: {setup}"
    );
    // And nothing trails the rules: they are the last word.
    assert!(system.trim_end().ends_with("- No emoji."), "{system}");
}

/// What the environment fragment may and may not carry, spelled out: the facts
/// that hold still for the life of the process, and nothing that moves. Each
/// line here is a line the prompt cache pays for once instead of every turn.
#[test]
fn the_environment_fragment_carries_only_facts_that_hold_still() {
    let fragments = fragments_of(&scripted_config());
    let (instance, block) = fragments.first().expect("there are fragments").clone();
    assert_eq!(
        instance, "env",
        "where you are comes before what you can do"
    );
    assert!(block.starts_with("Where you are:"), "{block}");

    let keys: Vec<&str> = block
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .map(|l| l.split_whitespace().next().unwrap_or(l))
        .collect();
    assert_eq!(
        keys,
        vec!["Working", "Platform:", "Shell:", "Today:", "Git"],
        "a fact here costs the cache once and must never move; \
         the branch, a dirty tree and a directory listing all move"
    );

    let cwd = std::env::current_dir().unwrap().display().to_string();
    assert!(block.contains(&cwd), "it says where we are: {block}");
    // And it says what it deliberately left out, so the model reads the gap as
    // a boundary rather than assuming the block is live.
    assert!(block.contains("does not update"), "{block}");
}

/// The model adapter is instantiated twice — the brain and the condenser — and
/// EVERY instance's fragment lands in the one system prompt. Without the
/// condenser's `prompt: null` the interruption passage arrives twice, at full
/// price, on every call forever.
#[test]
fn a_fragment_from_a_twice_instantiated_component_is_not_said_twice() {
    let mut cfg = scripted_config();
    // A real adapter, which is what carries the fragment; nothing is called.
    cfg.adapter = "openai".to_string();
    cfg.scripted = None;
    let fragments = fragments_of(&cfg);
    let said: Vec<&String> = fragments
        .iter()
        .filter(|(_, text)| text.contains("[interrupted: …]"))
        .map(|(instance, _)| instance)
        .collect();
    assert_eq!(said, vec!["model"], "said once, not once per instance");
}

/// `run` is the one tool whose starting point the model cannot infer: the
/// environment fragment names the PROCESS's working directory, and nothing
/// says the shell shares it. Left unsaid, a real session prefixed
/// `cd <repo> &&` onto every single command. So the fragment states it — and
/// when the shell IS pointed somewhere else, it states that instead.
#[test]
fn the_shell_fragment_says_where_commands_start() {
    let mut cfg = scripted_config();

    cfg.workspace = None;
    let open = fragments_of(&cfg);
    let (_, text) = open
        .iter()
        .find(|(instance, _)| instance == "shell")
        .expect("the shell fragment is there");
    assert!(
        !text.contains("cd"),
        "unconfined, where commands start is the schema's business and the \
         result's, not a claim in a prompt the model cannot check: {text}"
    );

    cfg.workspace = Some("/srv/box".to_string());
    let confined = fragments_of(&cfg);
    let (_, text) = confined
        .iter()
        .find(|(instance, _)| instance == "shell")
        .expect("the shell fragment is there");
    assert!(
        text.contains("starts its commands in /srv/box"),
        "confined, it names the directory — the one fact no result can teach \
         before the first call: {text}"
    );
}

/// Project rules remain in the prompt even after earlier tool results leave
/// the conversation context. The rules fragment follows the environment.
#[test]
fn the_project_rules_ride_in_the_prompt_behind_the_environment() {
    let fragments = fragments_of(&scripted_config());
    let order: Vec<&str> = fragments.iter().map(|(i, _)| i.as_str()).collect();
    assert_eq!(order[0], "env", "where you are comes first: {order:?}");
    assert_eq!(
        order.last().unwrap(),
        &"zz-project-rules",
        "and the rules come last, so nothing is printed under their headings: {order:?}"
    );
    // Compare the actual body, not a phrase tied to one wording or language.
    let expected =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("CLAUDE.md"))
            .unwrap();
    assert!(!expected.trim().is_empty(), "the fixture has rules to load");
    let (_, rules) = fragments.last().unwrap();
    // Heading levels are deliberately demoted in the prompt. Each original
    // nonempty line must still be present, including the complete prose.
    for line in expected.lines().filter(|line| !line.trim().is_empty()) {
        assert!(
            rules.contains(line),
            "the rules themselves are in it, not a pointer to them: missing {line:?}"
        );
    }
}

/// Agent identity is separate from the selected model and from names in
/// repository files. Changing models must not leave a stale identity label.
#[test]
fn the_prompt_says_what_it_is_and_which_model_is_thinking() {
    std::env::set_var("LATTICE_SCRIPTED", "1");
    let system = systems_from_two_turns(&PresetConfig::from_env()).remove(0);
    let opening = system.split("# You and the person").next().unwrap();
    assert!(opening.contains("You are Eva"), "it has a name: {opening}");
    assert!(
        opening.contains("Lattice is the runtime"),
        "Eva is the agent, Lattice is what runs it: {opening}"
    );
    assert!(
        opening.contains("scripted"),
        "and which model is doing the thinking: {opening}"
    );
    assert!(
        opening.contains("never take a name from a file"),
        "and that a name in a handed-over file is not its own: {opening}"
    );

    assert!(
        !system.contains("{model}"),
        "no placeholder survives into the prompt: {system}"
    );

    // The name is filled in where the prompt is ASSEMBLED, not baked into the
    // base text at startup, because the model can be changed mid-conversation
    // and a name filled in once would go on saying the old one.
    let named = |model| {
        lattice::components::context_gate::assemble_system(
            Some(&PresetConfig::from_env().system),
            None,
            &[],
            None,
            model,
        )
        .expect("something to assemble")
    };
    assert!(named(Some("some-model-9")).contains("some-model-9"));
    assert!(
        named(None).contains("{model}"),
        "with no model named, the placeholder is left visibly unfilled rather \
         than replaced by something vague the agent would then repeat"
    );
}

/// `lattice prompt` exists so the prompt can be reviewed BEFORE it ships, and
/// it is only worth having if what it prints is what gets sent. Both go
/// through one assembler for that reason; this is the check that they still
/// do, because a second copy that drifts is worse than no command at all.
#[test]
fn the_printed_prompt_is_the_one_that_gets_sent() {
    std::env::set_var("LATTICE_SCRIPTED", "1");
    let cfg = PresetConfig::from_env();
    let sent = systems_from_two_turns(&cfg).remove(0);

    let (registry, mut factories, assembly) = preset::standard(&cfg).expect("preset builds");
    let kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("starts");
    let printed = lattice::components::context_gate::assemble_system(
        Some(&cfg.system),
        Some(preset::FRAGMENTS_HEADING),
        &kernel.prompt_fragments(),
        Some(&preset::house_rules()),
        Some(&cfg.model),
    )
    .expect("something to print");
    kernel.shutdown();

    assert_eq!(printed, sent, "what is shown is what is sent");
}

/// A model described in the user's own catalog must have that description
/// REACH the two places that act on it: the gate that budgets the context, and
/// the adapter that places an effort request. Before inline profiles existed a
/// hand-added model got neither — it ran budgeted against the startup fallback
/// of a million tokens with an empty effort ladder. The profile file was read
/// by nothing for months before anyone noticed; the wiring is the part worth
/// pinning, not the reading.
#[test]
fn a_profile_written_into_the_catalog_reaches_the_gate_and_the_adapter() {
    let mut cfg = scripted_config();
    cfg.adapter = "openai".to_string();
    cfg.scripted = None;
    cfg.model = "kimi-k2".to_string(); // nothing ships a profile for this
    cfg.profile = Some(json!({
        "contextWindow": 256000,
        "effort": ["low", "high"],
        "usageFields": {"input": "prompt_tokens", "cacheRead": "prompt_cache_hit_tokens"},
    }));

    let (_, _, assembly) = preset::standard(&cfg).expect("preset builds");
    let config = |name: &str| assembly.instances[name].config.clone().unwrap();

    assert_eq!(
        config("model")["effort"],
        json!(["low", "high"]),
        "the adapter must be told what THIS model offers, or every request is \
         aimed by the wire's defaults"
    );
    let profile = &config("ctx")["profile"];
    assert_eq!(
        profile["contextWindow"], 256000,
        "the gate must budget against the model's own window, not the fallback"
    );
    assert_eq!(
        profile["usageFields"]["cacheRead"], "prompt_cache_hit_tokens",
        "and read the cache count under the name this provider uses — the wrong \
         name reads nothing and reports a permanently cold cache"
    );

    // Without the profile, the same model falls back to the startup numbers.
    cfg.profile = None;
    let (_, _, assembly) = preset::standard(&cfg).expect("preset builds");
    assert_eq!(
        assembly.instances["ctx"].config.clone().unwrap()["profile"]["contextWindow"],
        cfg.context_window,
        "nothing describes it, so nothing is invented"
    );
}
