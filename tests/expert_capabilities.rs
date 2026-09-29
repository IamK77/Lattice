//! Real expert assemblies: capability declarations and execution must agree.
//! All writes/install sources/grants are synthetic temporary fixtures.
use lattice::components::{skill_library, trust_policy};
use lattice::core_events as ce;
use lattice::preset::{expert_assembly, PresetConfig, EXPERTS};
use lattice::{EventDraft, EventEnvelope, Kernel, KernelOptions};
use serde_json::{json, Value};
use std::path::Path;

fn config(root: &Path, script: Value) -> PresetConfig {
    PresetConfig {
        adapter: "scripted".into(),
        model: "scripted".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(root.to_str().unwrap().into()),
        context_window: 64000,
        usage_input_field: "input_tokens".into(),
        profile: None,
        catalog_problems: vec![],
        system: "test".into(),
        scripted: Some(json!({"script":script})),
        thinking: None,
        overlay: None,
        assembly: None,
    }
}

fn run(root: &Path, role: &str, calls: Value, text: &str) -> Vec<EventEnvelope> {
    run_form(root, role, calls, text, false)
}

fn run_form(
    root: &Path,
    role: &str,
    calls: Value,
    text: &str,
    process: bool,
) -> Vec<EventEnvelope> {
    let cfg = config(
        root,
        json!([
            {"status":"ok","toolCalls":calls}, {"status":"ok","text":"done"}
        ]),
    );
    let (mut registry, mut factories, mut assembly) = if role == "main" {
        lattice::preset::standard(&cfg).unwrap()
    } else {
        expert_assembly(&cfg, EXPERTS.iter().find(|e| e.name == role).unwrap()).unwrap()
    };
    if process {
        for name in [
            "fs-reader",
            "fs-writer",
            "skill-consumer",
            "skill-installer",
        ] {
            let manifest = registry.get_mut(name).unwrap();
            manifest.runtime = lattice::RuntimeKind::Process;
            manifest.entry = format!("{} component {name}", env!("CARGO_BIN_EXE_lattice"));
        }
    }
    assembly.instances.get_mut("skills").unwrap().config =
        Some(json!({"dirs":[root.join("skills")]}));
    if let Some(installer) = assembly.instances.get_mut("skill-installer") {
        installer.config = Some(json!({"dirs":[root.join("skills")]}));
    }
    assembly.instances.get_mut("trust").unwrap().config = Some(json!({
        "stance":"deny", "grants":root.join("grants.json")
    }));
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":text})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    assert!(
        events.iter().any(|e| e.event_type == ce::TURN_COMPLETED),
        "turn did not settle"
    );
    for call in calls.as_array().unwrap() {
        let request = events
            .iter()
            .rev()
            .find(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.payload["call"] == call["id"])
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| ce::ends_call(e, &request.id))
                .count(),
            1,
            "each final request needs exactly one terminal answer: {call}"
        );
    }
    kernel.shutdown();
    events
}

fn result<'a>(events: &'a [EventEnvelope], call: &str) -> &'a Value {
    &events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == call)
        .unwrap()
        .payload
}

fn mutations() -> Value {
    json!([
        {"id":"overwrite","tool":"Write","arguments":{"path":"probe.txt","content":"changed"}},
        {"id":"create","tool":"Write","arguments":{"path":"new.txt","content":"new"}},
        {"id":"edit","tool":"Edit","arguments":{"path":"probe.txt","old":"original","new":"edited"}}
    ])
}

#[test]
fn explorer_cannot_modify_the_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("probe.txt");
    std::fs::write(&path, "original").unwrap();
    let events = run(dir.path(), "explorer", mutations(), "Inspect the fixture.");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    assert!(!dir.path().join("new.txt").exists());
    for event in events
        .iter()
        .filter(|e| e.event_type == ce::MODEL_CALL_STARTED)
    {
        for tool in event.payload["tools"].as_array().unwrap() {
            assert!(!matches!(
                tool["name"].as_str(),
                Some("Write" | "Edit" | "InstallSkill")
            ));
        }
    }
}

#[test]
fn worker_write_control_reaches_the_real_tool() {
    for role in ["worker", "main"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("probe.txt"), "original").unwrap();
        std::fs::write(dir.path().join("edit.txt"), "original").unwrap();
        let events = run(
            dir.path(),
            role,
            json!([
                {"id":"write","tool":"Write","arguments":{"path":"probe.txt","content":"changed"}},
                {"id":"create","tool":"Write","arguments":{"path":"new.txt","content":"new"}},
                {"id":"edit","tool":"Edit","arguments":{"path":"edit.txt","old":"original","new":"edited"}}
            ]),
            "Modify the fixtures.",
        );
        assert_eq!(result(&events, "write")["status"], "ok");
        assert_eq!(result(&events, "create")["status"], "ok");
        assert_eq!(result(&events, "edit")["status"], "ok");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("edit.txt")).unwrap(),
            "edited"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("probe.txt")).unwrap(),
            "changed"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "new"
        );
    }
}

fn install_fixture(root: &Path) -> Value {
    let source = root.join("source/example");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "---\nname: example\ndescription: Example fixture.\n---\nFIXTURE_SKILL\n",
    )
    .unwrap();
    let arguments = json!({"source":source,"reason":"test capability boundary"});
    let effects = skill_library::installer_manifest().tools[0]["effects"].clone();
    std::fs::write(
        root.join("grants.json"),
        serde_json::to_vec(&json!({"grants":[{
            "key":trust_policy::admission_key(&arguments),"admits":"skills","effects":effects,
            "summary":"fixture","granted_by":"manual"
        }]}))
        .unwrap(),
    )
    .unwrap();
    arguments
}

#[test]
fn read_only_experts_cannot_install_even_with_a_grant() {
    for role in ["explorer", "researcher"] {
        let dir = tempfile::tempdir().unwrap();
        let args = install_fixture(dir.path());
        let events = run(
            dir.path(),
            role,
            json!([{"id":"install","tool":"InstallSkill","arguments":args}]),
            "Inspect only.",
        );
        assert!(
            !dir.path().join("skills").exists(),
            "consumer must not create an installation or staging directory"
        );
        assert!(!events
            .iter()
            .any(|e| e.event_type == skill_library::SKILL_INSTALLED));
        assert!(events.iter().any(|e| e.event_type == ce::INTERRUPTED));
        assert!(
            !events
                .iter()
                .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED
                    && e.payload["error"]["code"] == "trust.not_granted"),
            "a trust refusal cannot prove the boundary"
        );
    }
}

#[test]
fn main_and_worker_install_and_refresh_with_a_causal_notification() {
    for role in ["main", "worker"] {
        let dir = tempfile::tempdir().unwrap();
        let args = install_fixture(dir.path());
        let events = run(
            dir.path(),
            role,
            json!([{"id":"install","tool":"InstallSkill","arguments":args}]),
            "Install fixture.",
        );
        assert_eq!(result(&events, "install")["status"], "ok");
        assert!(dir.path().join("skills/example/SKILL.md").exists());
        let wake = events
            .iter()
            .find(|e| e.event_type == ce::WAKE && e.source == "skill-installer")
            .unwrap();
        assert!(events.iter().any(|e| e.event_type == skill_library::SKILL_LISTING && e.causes.contains(&wake.id)), "explicit notification must refresh the menu even when the directory did not exist at restore");
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == ce::MODEL_CALL_COMPLETED)
                .count(),
            2,
            "refresh must not start an extra model turn"
        );
    }
}

#[test]
fn consumption_and_search_remain_available() {
    let dir = tempfile::tempdir().unwrap();
    let args = install_fixture(dir.path());
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::rename(
        args["source"].as_str().unwrap(),
        dir.path().join("skills/example"),
    )
    .unwrap();
    std::fs::write(dir.path().join("probe.txt"), "needle").unwrap();
    let events = run(
        dir.path(),
        "explorer",
        json!([
            {"id":"read","tool":"Read","arguments":{"path":"probe.txt"}},
            {"id":"ls","tool":"Ls","arguments":{"path":"."}},
            {"id":"find","tool":"Find","arguments":{"glob":"*.txt"}},
            {"id":"grep","tool":"Grep","arguments":{"pattern":"needle"}},
            {"id":"load","tool":"LoadSkill","arguments":{"name":"example"}},
            {"id":"catalog","tool":"FindTools","arguments":{"query":"InstallSkill"}}
        ]),
        "/example",
    );
    for call in ["read", "ls", "find", "grep", "load", "catalog"] {
        assert_eq!(result(&events, call)["status"], "ok", "{call}");
    }
    assert!(!result(&events, "catalog")["result"]
        .to_string()
        .contains("InstallSkill"));
    assert!(events.iter().any(|e| e.source == "skills"
        && e.event_type == ce::USER_MESSAGE
        && e.payload["text"]
            .as_str()
            .is_some_and(|text| text.contains("FIXTURE_SKILL"))));
}

#[test]
fn expert_catalogs_only_advertise_active_instances() {
    let dir = tempfile::tempdir().unwrap();
    for expert in EXPERTS {
        let (registry, _, assembly) =
            expert_assembly(&config(dir.path(), json!([])), expert).unwrap();
        let available: Vec<_> = assembly
            .instances
            .values()
            .flat_map(|i| registry[&i.component].tools.iter())
            .filter_map(|t| t["name"].as_str())
            .collect();
        for (instance, field) in [("tool-catalog", "deferred"), ("ctx", "deferTools")] {
            for name in assembly.instances[instance].config.as_ref().unwrap()[field]
                .as_array()
                .unwrap()
            {
                assert!(
                    available.contains(&name.as_str().unwrap()),
                    "{} advertises absent tool {name}",
                    expert.name
                );
            }
        }
        for name in ["Write", "Edit", "InstallSkill"] {
            assert_eq!(available.contains(&name), expert.name == "worker");
        }
        assert!(!registry.contains_key("fs-tools"));
        assert!(!registry.contains_key("skill-library"));
    }
}

#[test]
fn split_providers_work_across_the_process_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let args = install_fixture(dir.path());
    std::fs::write(dir.path().join("probe.txt"), "original").unwrap();
    let events = run_form(
        dir.path(),
        "worker",
        json!([
            {"id":"read","tool":"Read","arguments":{"path":"probe.txt"}},
            {"id":"write","tool":"Write","arguments":{"path":"new.txt","content":"new"}},
            {"id":"install","tool":"InstallSkill","arguments":args}
        ]),
        "Use the process providers.",
        true,
    );
    for call in ["read", "write", "install"] {
        assert_eq!(result(&events, call)["status"], "ok");
    }
    assert!(events
        .iter()
        .any(|e| e.event_type == skill_library::SKILL_LISTING
            && e.source == "skills"
            && e.payload.to_string().contains("example")));
    let events = run_form(
        dir.path(),
        "explorer",
        json!([
            {"id":"load","tool":"LoadSkill","arguments":{"name":"example"}},
            {"id":"write","tool":"Write","arguments":{"path":"probe.txt","content":"changed"}},
            {"id":"install","tool":"InstallSkill","arguments":args}
        ]),
        "Inspect only.",
        true,
    );
    assert_eq!(result(&events, "load")["status"], "ok");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("probe.txt")).unwrap(),
        "original"
    );
    assert!(!events
        .iter()
        .any(|e| e.event_type == skill_library::SKILL_INSTALLED));
}

#[test]
fn obsolete_combined_process_commands_are_rejected() {
    for name in ["fs-tools", "skill-library"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lattice"))
            .args(["component", name])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown"));
    }
}

#[test]
fn failed_installs_do_not_announce_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let args = install_fixture(dir.path());
    std::fs::write(
        Path::new(args["source"].as_str().unwrap()).join("SKILL.md"),
        "invalid skill",
    )
    .unwrap();
    let events = run(
        dir.path(),
        "worker",
        json!([{"id":"install","tool":"InstallSkill","arguments":args}]),
        "Try the invalid fixture.",
    );
    assert_eq!(result(&events, "install")["status"], "error");
    assert!(!events.iter().any(|e| e.source == "skill-installer"
        && (e.event_type == ce::WAKE || e.event_type == skill_library::SKILL_INSTALLED)));
}
