//! The skill library, end to end: listing, three-tier loading, bundled-file
//! confinement, dedup, and installation with its reasoned decision event.
//! Skills follow the open Agent Skills standard (folder + SKILL.md with YAML
//! frontmatter); the canon is schemas/skill_frontmatter.json.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use lattice::components::{context_gate, minimal_loop, scripted_model, silent_ui, skill_library};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, ComponentManifest, EventDraft, EventEnvelope, Factory,
    Kernel, KernelOptions, Wire,
};

fn with_installer(mut assembly: AssemblyManifest) -> AssemblyManifest {
    let mut config = assembly.instances["skills"].config.clone();
    if let Some(config) = config.as_mut().and_then(Value::as_object_mut) {
        config.remove("prompt");
    }
    assembly.instances.insert(
        "installer".into(),
        ComponentInstance {
            component: skill_library::INSTALLER.into(),
            config,
            requires: vec![],
        },
    );
    assembly.wires.extend([
        Wire::new("loop.run", "installer.execute"),
        Wire::new("installer.outcome", "loop.tools"),
        Wire::new("installer.changed", "skills.refresh"),
    ]);
    assembly
}

fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\n{frontmatter}---\n{body}"),
    )
    .unwrap();
}

/// Run one scripted conversation against a kernel whose skill library scans
/// (and installs into) `skills_root`; returns the full ledger.
fn run_scripted(skills_root: &Path, script: Value) -> Vec<EventEnvelope> {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (
            skill_library::CONSUMER.to_string(),
            skill_library::consumer_manifest(),
        ),
        (
            skill_library::INSTALLER.to_string(),
            skill_library::installer_manifest(),
        ),
    ]
    .into();
    let displayed: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&displayed)))),
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
        skill_library::INSTALLER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillInstaller::from_config(c))),
    );
    factories.insert(
        skill_library::CONSUMER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillConsumer::from_config(c))),
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
            (
                "skills".to_string(),
                ComponentInstance {
                    component: skill_library::CONSUMER.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({
                        "dirs": [skills_root.display().to_string()],
                    })),
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "skills.execute"),
            Wire::new("skills.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };

    let assembly = with_installer(assembly);
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the skill assembly must pass inspection");
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    events
}

fn skill_completions(events: &[EventEnvelope]) -> Vec<&EventEnvelope> {
    events
        .iter()
        .filter(|e| {
            e.event_type == ce::TOOL_EXEC_COMPLETED
                && matches!(e.source.as_str(), "skills" | "installer")
        })
        .collect()
}

// ── Listing ─────────────────────────────────────────────

#[test]
fn the_listing_names_every_skill_and_surfaces_broken_ones() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "release-dance",
        "name: release-dance\ndescription: How to cut a release of this project.\n",
        "Step 1: run the tests.\n",
    );
    // No name in the frontmatter: the directory name fills in (the
    // ecosystem's common shorthand)
    write_skill(
        root.path(),
        "commit-style",
        "description: How to write commit messages here.\n",
        "Subject under 50 chars.\n",
    );
    // Broken frontmatter: listed as unavailable, never silently dropped
    write_skill(root.path(), "ghost", "not yaml: [unclosed\n", "body\n");

    let dirs = vec![root.path().display().to_string()];
    let listing = skill_library::listing_prompt(&dirs).expect("skills exist");
    assert!(listing.contains("release-dance: How to cut a release"));
    assert!(listing.contains("commit-style: How to write commit messages"));
    assert!(listing.contains("ghost: (unavailable"));

    let empty = tempfile::tempdir().unwrap();
    let none = skill_library::listing_prompt(&[empty.path().display().to_string()]);
    assert_eq!(none, None, "no skills must mean no resident fragment");
}

#[test]
fn a_name_that_contradicts_its_directory_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "actual-dir",
        "name: other-name\ndescription: Claims a different identity.\n",
        "body\n",
    );
    let dirs = vec![root.path().display().to_string()];
    let listing = skill_library::listing_prompt(&dirs).expect("the dir is scanned");
    assert!(
        listing.contains("actual-dir: (unavailable"),
        "a name/directory mismatch must be surfaced, got: {listing}"
    );
}

// ── Loading ─────────────────────────────────────────────

#[test]
fn loading_returns_the_body_and_a_second_identical_load_only_a_note() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "release-dance",
        "name: release-dance\ndescription: How to cut a release.\n",
        "Step 1: run the tests.\nStep 2: tag.\n",
    );
    let events = run_scripted(
        root.path(),
        json!({"script": [
            {"status": "ok", "toolCalls": [
                {"id": "t1", "tool": "LoadSkill", "arguments": {"name": "release-dance"}}]},
            {"status": "ok", "toolCalls": [
                {"id": "t2", "tool": "LoadSkill", "arguments": {"name": "release-dance"}}]},
            {"status": "ok", "text": "done"},
        ]}),
    );
    let completions = skill_completions(&events);
    assert_eq!(completions.len(), 2);

    let first = &completions[0].payload;
    assert_eq!(first["status"], "ok");
    let content = first["result"]["content"].as_str().unwrap();
    assert!(content.contains("Step 1: run the tests."));
    assert!(
        !content.contains("---"),
        "the frontmatter must not travel with the body"
    );
    assert!(first["result"]["dir"]
        .as_str()
        .is_some_and(|d| !d.is_empty()));
    assert!(!completions[0].causes.is_empty());

    let second = &completions[1].payload;
    assert_eq!(second["status"], "ok");
    assert!(
        second["result"]["content"].is_null(),
        "an unchanged reload must not re-inject the body"
    );
    assert!(second["result"]["note"]
        .as_str()
        .unwrap()
        .contains(completions[0].id.as_str()));
}

#[test]
fn unknown_skills_and_escaping_paths_are_refused_as_data() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "release-dance",
        "name: release-dance\ndescription: How to cut a release.\n",
        "See references/notes.md\n",
    );
    std::fs::create_dir_all(root.path().join("release-dance/references")).unwrap();
    std::fs::write(
        root.path().join("release-dance/references/notes.md"),
        "the fine print",
    )
    .unwrap();
    // A real file one level above the skill folder: the escape target exists,
    // so only the confinement checks stand between it and the model
    std::fs::write(root.path().join("secret.txt"), "must never leak").unwrap();

    let events = run_scripted(
        root.path(),
        json!({"script": [
            {"status": "ok", "toolCalls": [
                {"id": "t1", "tool": "LoadSkill", "arguments": {"name": "nope"}},
            ]},
            {"status": "ok", "toolCalls": [
                {"id": "t2", "tool": "LoadSkill",
                 "arguments": {"name": "release-dance", "file": "references/notes.md"}},
            ]},
            {"status": "ok", "toolCalls": [
                {"id": "t3", "tool": "LoadSkill",
                 "arguments": {"name": "release-dance", "file": "../secret.txt"}},
            ]},
            {"status": "ok", "text": "done"},
        ]}),
    );
    let completions = skill_completions(&events);
    assert_eq!(completions.len(), 3);
    assert_eq!(completions[0].payload["error"]["code"], "skill.unknown");
    assert_eq!(
        completions[1].payload["result"]["content"]
            .as_str()
            .unwrap(),
        "the fine print"
    );
    assert_eq!(
        completions[2].payload["error"]["code"],
        "skill.invalid_path"
    );
}

/// The `file` argument was guarded from the start. The `name` was not — and
/// it is the one a model always supplies.
///
/// Rust's `join` REPLACES the path it is given when that path is absolute, so
/// a name of `/somewhere/else` was read straight out of the filesystem, and
/// `../..` walked out the same way. "Skills are read only from the configured
/// folders" was true of everything except the argument that chooses which
/// skill to read.
#[test]
fn a_skill_name_that_is_really_a_path_is_refused() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "release-dance",
        "name: release-dance\ndescription: How to cut a release.\n",
        "Cut it carefully.\n",
    );
    // A perfectly valid skill, sitting somewhere the library was never
    // pointed at. Only the name check keeps it out.
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(
        elsewhere.path(),
        "outsider",
        "name: outsider\ndescription: Not in the library.\n",
        "secret instructions\n",
    );
    let absolute = elsewhere.path().join("outsider").display().to_string();

    let events = run_scripted(
        root.path(),
        json!({"script": [
            {"status": "ok", "toolCalls": [
                {"id": "t1", "tool": "LoadSkill", "arguments": {"name": absolute}},
            ]},
            {"status": "ok", "toolCalls": [
                {"id": "t2", "tool": "LoadSkill",
                 "arguments": {"name": "../../etc"}},
            ]},
            {"status": "ok", "text": "done"},
        ]}),
    );
    let completions = skill_completions(&events);
    assert_eq!(completions.len(), 2);
    for (n, completion) in completions.iter().enumerate() {
        assert_eq!(
            completion.payload["error"]["code"], "skill.unknown",
            "call {n} reached outside the library: {}",
            completion.payload
        );
    }
    let said = serde_json::to_string(&completions[0].payload).unwrap();
    assert!(
        !said.contains("secret instructions"),
        "the outsider's body must not come back: {said}"
    );
}

// ── Installation ────────────────────────────────────────

#[test]
fn installing_from_a_local_path_validates_lands_and_records_a_decision() {
    let library = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(
        elsewhere.path(),
        "review-checklist",
        "name: review-checklist\ndescription: What to check in review.\n",
        "Check the tests first.\n",
    );
    let source = elsewhere.path().join("review-checklist");

    let events = run_scripted(
        library.path(),
        json!({"script": [
            {"status": "ok", "toolCalls": [
                {"id": "t1", "tool": "InstallSkill", "arguments": {
                    "source": source.display().to_string(),
                    "reason": "the team wants the review procedure on call",
                }},
            ]},
            // Loadable immediately, in the same run — no restart needed
            {"status": "ok", "toolCalls": [
                {"id": "t2", "tool": "LoadSkill",
                 "arguments": {"name": "review-checklist"}},
            ]},
            {"status": "ok", "text": "done"},
        ]}),
    );

    let completions = skill_completions(&events);
    assert_eq!(completions.len(), 2);
    assert_eq!(completions[0].payload["status"], "ok");
    assert_eq!(
        completions[0].payload["result"]["installed"],
        "review-checklist"
    );
    assert!(completions[1].payload["result"]["content"]
        .as_str()
        .unwrap()
        .contains("Check the tests first."));
    assert!(library.path().join("review-checklist/SKILL.md").is_file());

    // The reasoned decision record, caused by the request
    let installed: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == skill_library::SKILL_INSTALLED)
        .collect();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].payload["name"], "review-checklist");
    assert_eq!(
        installed[0].reason.as_deref(),
        Some("the team wants the review procedure on call")
    );
    assert!(!installed[0].causes.is_empty());
}

/// The deferred discipline, skill edition: a mid-session install must not
/// touch the system prompt while the provider cache is warm (the model knows
/// the new skill from the conversation that installed it); the listing is
/// refreshed the first time the cache is cold anyway, on the record — the
/// same terms as deferred tools.
#[test]
fn a_new_skill_enters_the_resident_listing_only_when_the_cache_is_cold() {
    let library = tempfile::tempdir().unwrap();
    write_skill(
        library.path(),
        "skill-a",
        "name: skill-a\ndescription: The one installed from the start.\n",
        "body a\n",
    );
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(
        elsewhere.path(),
        "skill-b",
        "name: skill-b\ndescription: The one installed mid-session.\n",
        "body b\n",
    );
    let dirs = vec![library.path().display().to_string()];
    let initial_listing = skill_library::listing_prompt(&dirs).expect("skill-a exists");

    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (
            skill_library::CONSUMER.to_string(),
            skill_library::consumer_manifest(),
        ),
        (
            skill_library::INSTALLER.to_string(),
            skill_library::installer_manifest(),
        ),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::default()))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        skill_library::INSTALLER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillInstaller::from_config(c))),
    );
    factories.insert(
        skill_library::CONSUMER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillConsumer::from_config(c))),
    );

    // Turn 1: install skill-b (cache WARM at 400), then answer with a COLD
    // final usage. Turn 2: a plain answer — the refresh must happen on its
    // opening call.
    let script = json!({"script": [
        {"status": "ok",
         "usage": {"input_tokens": 100, "cache_read_input_tokens": 400},
         "toolCalls": [{"id": "t1", "tool": "InstallSkill", "arguments": {
             "source": elsewhere.path().join("skill-b").display().to_string(),
             "reason": "the model wants it on call",
         }}]},
        {"status": "ok", "text": "turn one done",
         "usage": {"input_tokens": 120, "cache_read_input_tokens": 0}},
        {"status": "ok", "text": "turn two done",
         "usage": {"input_tokens": 50, "cache_read_input_tokens": 300}},
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
                "ctx".to_string(),
                ComponentInstance {
                    component: context_gate::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"profile": {
                        "contextWindow": 64000,
                        "usageFields": {"input": "input_tokens",
                                        "cacheRead": "cache_read_input_tokens"},
                    }})),
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
                "skills".to_string(),
                ComponentInstance {
                    component: skill_library::CONSUMER.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({
                        "dirs": dirs,
                        "prompt": initial_listing,
                    })),
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "ctx.ask"),
            Wire::new("ctx.forward", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "skills.execute"),
            Wire::new("skills.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };

    let assembly = with_installer(assembly);
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the gated skill assembly must pass inspection");
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "turn one"})),
    );
    kernel.run_until_quiescent().unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "turn two"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    let systems: Vec<&str> = events
        .iter()
        .filter(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.payload.get("purpose").is_none()
                && e.payload["system"].is_string()
        })
        .map(|e| e.payload["system"].as_str().unwrap())
        .collect();
    assert_eq!(systems.len(), 3, "three forwarded calls carry a system");
    assert!(systems[0].contains("skill-a") && !systems[0].contains("skill-b"));
    assert_eq!(
        systems[1], systems[0],
        "the cache was warm when the follow-up call went out: the listing must hold"
    );
    assert!(
        systems[2].contains("skill-b"),
        "the cache was cold at the next turn: the listing must refresh"
    );

    let refreshes: Vec<_> = events
        .iter()
        .filter(|e| {
            e.event_type == context_gate::DECISION && e.payload["action"] == "refresh_system"
        })
        .collect();
    assert_eq!(refreshes.len(), 1, "one reasoned refresh decision");
    assert!(refreshes[0].reason.is_some());
}

/// Event-driven hot-sensing: a skill folder dropped in BY HAND (no tool call
/// anywhere) is noticed by the standing watch on the skill directories, the
/// wake routes back to the library through the `changed`→`refresh` ring — no
/// model turn, no token — and the refreshed listing enters the system prompt
/// at the next cache-cold call, as ever.
#[test]
fn a_folder_dropped_in_by_hand_refreshes_the_listing_without_a_model_turn() {
    let library = tempfile::tempdir().unwrap();
    write_skill(
        library.path(),
        "skill-a",
        "name: skill-a\ndescription: The one installed from the start.\n",
        "body a\n",
    );
    let dirs = vec![library.path().display().to_string()];
    let initial_listing = skill_library::listing_prompt(&dirs).expect("skill-a exists");

    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (
            skill_library::CONSUMER.to_string(),
            skill_library::consumer_manifest(),
        ),
        (
            skill_library::INSTALLER.to_string(),
            skill_library::installer_manifest(),
        ),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::default()))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        skill_library::INSTALLER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillInstaller::from_config(c))),
    );
    factories.insert(
        skill_library::CONSUMER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillConsumer::from_config(c))),
    );

    let script = json!({"script": [
        {"status": "ok", "text": "turn one done",
         "usage": {"input_tokens": 100, "cache_read_input_tokens": 0}},
        {"status": "ok", "text": "turn two done",
         "usage": {"input_tokens": 50, "cache_read_input_tokens": 300}},
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
                "ctx".to_string(),
                ComponentInstance {
                    component: context_gate::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"profile": {
                        "contextWindow": 64000,
                        "usageFields": {"input": "input_tokens",
                                        "cacheRead": "cache_read_input_tokens"},
                    }})),
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
                "skills".to_string(),
                ComponentInstance {
                    component: skill_library::CONSUMER.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({
                        "dirs": dirs,
                        "prompt": initial_listing,
                    })),
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "ctx.ask"),
            Wire::new("ctx.forward", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "skills.execute"),
            Wire::new("skills.outcome", "loop.tools"),
            // The standing watch on the skill folders, wired back to the
            // library — the ring that makes hand-drops visible
            Wire::new("skills.changed", "skills.refresh"),
            Wire::new("loop.out", "ui.display"),
        ],
    };

    let assembly = with_installer(assembly);
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the ring-wired skill assembly must pass inspection");
    let wake_rx = kernel.take_wake_receiver().unwrap();

    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "turn one"})),
    );
    kernel.run_until_quiescent().unwrap();
    let calls_before = |k: &Kernel| {
        k.log()
            .replay(1)
            .unwrap()
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
            .count()
    };
    let model_calls_after_turn_one = calls_before(&kernel);

    // The hand-drop: no tool call anywhere near this. The standing watch is
    // armed asynchronously by restore, so keep touching the folder until the
    // refresh wake proves it live.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let refresh_seen = |k: &Kernel| {
        k.log()
            .replay(1)
            .unwrap()
            .iter()
            .any(|e| e.event_type == ce::WAKE && e.source == "skills")
    };
    let mut round = 0;
    while !refresh_seen(&kernel) {
        assert!(
            std::time::Instant::now() < deadline,
            "the standing watch never sensed the hand-drop"
        );
        round += 1;
        write_skill(
            library.path(),
            "skill-b",
            &format!("name: skill-b\ndescription: Dropped in by hand (round {round}).\n"),
            "body b\n",
        );
        if wake_rx
            .recv_timeout(std::time::Duration::from_millis(400))
            .is_ok()
        {
            kernel.run_until_quiescent().unwrap();
        }
    }
    assert_eq!(
        calls_before(&kernel),
        model_calls_after_turn_one,
        "sensing a hand-drop must not cost a model call"
    );

    // Next turn: the cache was cold, so the refreshed listing is adopted
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "turn two"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    let last_system = events
        .iter()
        .rev()
        .find(|e| {
            e.event_type == ce::MODEL_CALL_STARTED
                && e.payload.get("purpose").is_none()
                && e.payload["system"].is_string()
        })
        .map(|e| e.payload["system"].as_str().unwrap().to_string())
        .expect("turn two forwarded a system");
    assert!(
        last_system.contains("skill-b"),
        "the hand-dropped skill entered the resident listing"
    );
}

#[test]
fn an_invalid_source_is_refused_and_leaves_no_trace() {
    let library = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    // description missing: fails the canon
    write_skill(elsewhere.path(), "broken", "name: broken\n", "body\n");
    let source = elsewhere.path().join("broken");

    let events = run_scripted(
        library.path(),
        json!({"script": [
            {"status": "ok", "toolCalls": [
                {"id": "t1", "tool": "InstallSkill", "arguments": {
                    "source": source.display().to_string(),
                    "reason": "trying anyway",
                }},
            ]},
            {"status": "ok", "toolCalls": [
                {"id": "t2", "tool": "InstallSkill", "arguments": {
                    "source": source.display().to_string(),
                }},
            ]},
            {"status": "ok", "text": "done"},
        ]}),
    );
    let completions = skill_completions(&events);
    assert_eq!(completions.len(), 2);
    assert_eq!(completions[0].payload["error"]["code"], "skill.invalid");
    assert_eq!(
        completions[1].payload["error"]["code"], "skill.bad_request",
        "an install without a reason must be refused"
    );
    assert!(
        events
            .iter()
            .all(|e| e.event_type != skill_library::SKILL_INSTALLED),
        "no decision event for a failed install"
    );
    assert!(
        std::fs::read_dir(library.path()).unwrap().next().is_none(),
        "a refused install must leave the library untouched"
    );
}

// ── The expansion station and the palette's menu ────────

/// The input-line station: a /skill-name message is expanded (body in,
/// $ARGUMENTS substituted, causal link back to the typed original); a plain
/// message and an unknown /name pass through untouched. The user already
/// decided — no model round-trip anywhere.
#[test]
fn slash_messages_expand_on_the_input_line_and_the_rest_pass_through() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "greeting",
        "name: greeting\ndescription: greet someone\n",
        "Greet like a pirate: $ARGUMENTS.",
    );

    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (
            skill_library::CONSUMER.to_string(),
            skill_library::consumer_manifest(),
        ),
        (
            skill_library::INSTALLER.to_string(),
            skill_library::installer_manifest(),
        ),
    ]
    .into();
    let displayed: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&displayed)))),
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
        skill_library::INSTALLER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillInstaller::from_config(c))),
    );
    factories.insert(
        skill_library::CONSUMER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillConsumer::from_config(c))),
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
                    config: Some(json!({"script": [
                        {"status": "ok", "text": "one"},
                        {"status": "ok", "text": "two"},
                        {"status": "ok", "text": "three"},
                    ]})),
                },
            ),
            (
                "skills".to_string(),
                ComponentInstance {
                    component: skill_library::CONSUMER.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"dirs": [root.path().display().to_string()]})),
                },
            ),
        ]
        .into(),
        // The expansion station sits on the input line, like the preset
        wires: vec![
            Wire::new("ui.user", "skills.input"),
            Wire::new("skills.expanded", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "skills.execute"),
            Wire::new("skills.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let assembly = with_installer(assembly);
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the expansion assembly must pass inspection");

    for text in ["/greeting to Ana", "plain sailing", "/nope args"] {
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": text})),
        );
        kernel.run_until_quiescent().unwrap();
    }
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();

    let forwarded: Vec<&EventEnvelope> = events
        .iter()
        .filter(|e| e.event_type == ce::USER_MESSAGE && e.source == "skills")
        .collect();
    assert_eq!(forwarded.len(), 3, "every user message passes the station");

    // The invocation: body in, arguments substituted, cause = the original
    let expanded = forwarded[0].payload["text"].as_str().unwrap();
    assert_eq!(expanded, "Greet like a pirate: to Ana.");
    let original = events
        .iter()
        .find(|e| e.payload["text"] == "/greeting to Ana" && e.causes.is_empty())
        .expect("the typed original is on the ledger too");
    assert_eq!(forwarded[0].causes, vec![original.id.clone()]);

    // A plain message and an unknown skill pass through untouched
    assert_eq!(forwarded[1].payload["text"], "plain sailing");
    assert_eq!(forwarded[2].payload["text"], "/nope args");

    // The model must be shown each message ONCE — the station's re-emission,
    // never the superseded original beside it. The first turn is where this
    // bites: the loop rebuilds from the ledger before its first delivery, and
    // the typed original is already sitting there.
    let first_ask = events
        .iter()
        .find(|e| e.event_type == ce::MODEL_CALL_STARTED)
        .expect("a first ask exists");
    let parts = first_ask.payload["input"]["parts"].as_array().unwrap();
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|p| p["event"].as_str())
        .filter_map(|id| events.iter().find(|e| e.id == id))
        .filter_map(|e| e.payload["text"].as_str())
        .collect();
    assert_eq!(
        texts,
        vec!["Greet like a pirate: to Ana."],
        "the model sees the expansion alone, not the typed original as well"
    );
}

/// The menu rides the ledger: announced when the library arrives with skills,
/// silent on an unchanged restart, announced again when the set changes.
/// A skill-less boot says nothing at all.
#[test]
fn the_menu_is_announced_on_change_and_silent_otherwise() {
    let root = tempfile::tempdir().unwrap();
    let ledger = tempfile::tempdir().unwrap();
    let ledger_path = ledger.path().join("stream.jsonl");

    let boot = |script: Value, install: bool| {
        let mut registry: HashMap<String, ComponentManifest> = [
            (silent_ui::NAME.to_string(), silent_ui::manifest()),
            (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
            (scripted_model::NAME.to_string(), scripted_model::manifest()),
            (
                skill_library::CONSUMER.to_string(),
                skill_library::consumer_manifest(),
            ),
            (
                skill_library::INSTALLER.to_string(),
                skill_library::installer_manifest(),
            ),
        ]
        .into();
        let displayed: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut factories: HashMap<String, Factory> = HashMap::new();
        factories.insert(
            silent_ui::NAME.to_string(),
            Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&displayed)))),
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
            skill_library::CONSUMER.to_string(),
            Box::new(|c| Box::new(skill_library::SkillConsumer::from_config(c))),
        );
        factories.insert(
            skill_library::INSTALLER.into(),
            Box::new(|c| Box::new(skill_library::SkillInstaller::from_config(c))),
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
                (
                    "skills".to_string(),
                    ComponentInstance {
                        component: skill_library::CONSUMER.to_string(),
                        requires: Vec::new(),
                        config: Some(json!({"dirs": [root.path().display().to_string()]})),
                    },
                ),
            ]
            .into(),
            wires: vec![
                Wire::new("ui.user", "skills.input"),
                Wire::new("skills.expanded", "loop.input"),
                Wire::new("loop.ask", "model.request"),
                Wire::new("model.result", "loop.model"),
                Wire::new("loop.run", "skills.execute"),
                Wire::new("skills.outcome", "loop.tools"),
                Wire::new("loop.out", "ui.display"),
            ],
        };
        let assembly = if install {
            with_installer(assembly)
        } else {
            registry.remove(skill_library::INSTALLER);
            assembly
        };
        Kernel::start(
            &assembly,
            &registry,
            &mut factories,
            KernelOptions {
                stream: Some("main".to_string()),
                log_file: Some(ledger_path.clone()),
                ..KernelOptions::default()
            },
        )
        .expect("boots")
    };
    let listings = |events: &[EventEnvelope]| -> Vec<Value> {
        events
            .iter()
            .filter(|e| e.event_type == skill_library::SKILL_LISTING)
            .map(|e| e.payload["skills"].clone())
            .collect()
    };

    // The restore announcement rides the component thread; a message pushed
    // THROUGH the station is the causal fence (the mailbox opens only after
    // restore, and central preserves order) — no clocks, no sleeps.
    let fence = |kernel: &mut Kernel| {
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "ping"})),
        );
        kernel.run_until_quiescent().unwrap();
    };
    let one_reply = || json!({"script": [{"status": "ok", "text": "pong"}]});

    // Boot 1: no skills — a skill-less library says nothing
    let mut kernel = boot(one_reply(), false);
    fence(&mut kernel);
    assert_eq!(listings(&kernel.log().replay(1).unwrap()).len(), 0);
    kernel.shutdown();

    // Boot 2: a skill appeared on disk — one announcement, naming it
    write_skill(
        root.path(),
        "greeting",
        "name: greeting\ndescription: greet someone\n",
        "body",
    );
    let mut kernel = boot(one_reply(), false);
    fence(&mut kernel);
    let seen = listings(&kernel.log().replay(1).unwrap());
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0][0]["name"], "greeting");
    kernel.shutdown();

    // Boot 3: nothing changed — the menu is NOT repeated
    let mut kernel = boot(one_reply(), false);
    fence(&mut kernel);
    assert_eq!(
        listings(&kernel.log().replay(1).unwrap()).len(),
        1,
        "no repeat"
    );
    kernel.shutdown();

    // An install announces the grown menu in the same conversation
    let donor = tempfile::tempdir().unwrap();
    write_skill(
        donor.path(),
        "farewell",
        "name: farewell\ndescription: say goodbye\n",
        "body",
    );
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "t1", "tool": "InstallSkill", "arguments": {
            "source": donor.path().join("farewell").display().to_string(),
            "reason": "the user asked",
        }}]},
        {"status": "ok", "text": "done"},
    ]});
    let mut kernel = boot(script, true);
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    kernel.run_until_quiescent().unwrap();
    let seen = listings(&kernel.log().replay(1).unwrap());
    assert_eq!(seen.len(), 2, "the install announced the new menu");
    let names: Vec<&str> = seen[1]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["farewell", "greeting"]);
    kernel.shutdown();

    // Reopen the same history without any installer declaration. A skill
    // landed while the runtime was down: recovery discovers it from disk,
    // including when its installation notification was never observed.
    write_skill(
        root.path(),
        "offline",
        "name: offline\ndescription: offline fixture\n",
        "offline body",
    );
    let mut kernel = boot(one_reply(), false);
    fence(&mut kernel);
    let events = kernel.log().replay(1).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == skill_library::SKILL_INSTALLED)
            .count(),
        1,
        "restoring must not execute the old installation again"
    );
    let seen = listings(&events);
    assert_eq!(seen.len(), 3);
    assert!(seen
        .last()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "offline"));
    kernel.shutdown();
}
