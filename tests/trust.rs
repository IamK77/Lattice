//! The trust gate: install-class calls (declared `admits` surface) pass only
//! when granted. Grants are content-addressed (canonical fingerprint of the
//! call's arguments) plus the declared surface at grant time — a changed
//! source or a changed appetite does not match. Ungranted: the `deny` stance
//! refuses with a how-to-grant hint; the `ask` stance runs the authorization
//! event pair and the turn waits for the human.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui, skill_library, trust_policy};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, EventDraft, EventEnvelope,
    Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

/// The human's proxy: a mute component whose output port lets the test
/// inject authorization answers (exactly what a frontend will do).
struct Human;
impl Component for Human {
    fn handle(&mut self, _p: &str, _e: &EventEnvelope, _c: &mut Ctx) {}
}
use lattice::Ctx;

fn human_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "human".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:human".to_string(),
        inputs: vec![],
        outputs: vec![PortDecl::new("say", &[ce::EXTERNAL_INPUT])],
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

fn write_skill(root: &Path, name: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: A test skill.\n---\nbody\n"),
    )
    .unwrap();
}

/// The declared surface of install_skill, straight from the manifest — the
/// same declaration the gate reads off the ledger.
fn install_skill_effects() -> Value {
    skill_library::manifest()
        .tools
        .iter()
        .find(|t| t["name"] == "InstallSkill")
        .expect("install_skill is declared")["effects"]
        .clone()
}

/// A gated assembly: loop → trust → skill library, with a scripted model
/// driving one install_skill call and a human proxy wired to the answer port.
fn start(
    stance: &str,
    grants: &Path,
    library: &Path,
    script: Value,
) -> (Kernel, std::sync::mpsc::Receiver<()>) {
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (trust_policy::NAME.to_string(), trust_policy::manifest()),
        (skill_library::NAME.to_string(), skill_library::manifest()),
        ("human".to_string(), human_manifest()),
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
    factories.insert(
        trust_policy::NAME.to_string(),
        Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
    );
    factories.insert(
        skill_library::NAME.to_string(),
        Box::new(|c| Box::new(skill_library::SkillLibrary::from_config(c))),
    );
    factories.insert("human".to_string(), Box::new(|_| Box::new(Human)));

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
                "trust".to_string(),
                ComponentInstance {
                    component: trust_policy::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({
                        "stance": stance,
                        "grants": grants.display().to_string(),
                    })),
                },
            ),
            (
                "skills".to_string(),
                ComponentInstance {
                    component: skill_library::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"dirs": [library.display().to_string()]})),
                },
            ),
            (
                "human".to_string(),
                ComponentInstance {
                    component: "human".to_string(),
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
            Wire::new("loop.run", "trust.review"),
            Wire::new("trust.forward", "skills.execute"),
            Wire::new("trust.verdict", "loop.tools"),
            Wire::new("skills.outcome", "loop.tools"),
            Wire::new("human.say", "trust.answer"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the gated assembly must pass inspection");
    let wake = kernel.take_wake_receiver().unwrap();
    (kernel, wake)
}

fn install_script(source: &Path) -> Value {
    json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "t1", "tool": "InstallSkill", "arguments": {
                "source": source.display().to_string(),
                "reason": "the user asked for this capability",
            }},
        ]},
        {"status": "ok", "text": "done"},
    ]})
}

fn events_of(kernel: &Kernel) -> Vec<EventEnvelope> {
    kernel.log().replay(1).unwrap()
}

#[test]
fn an_ungranted_admission_is_refused_with_a_how_to_grant_hint() {
    let grants = tempfile::tempdir().unwrap();
    let library = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(elsewhere.path(), "new-skill");

    let (mut kernel, _wake) = start(
        "deny",
        &grants.path().join("trust.json"),
        library.path(),
        install_script(&elsewhere.path().join("new-skill")),
    );
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = events_of(&kernel);
    kernel.shutdown();

    let verdict = events
        .iter()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "trust")
        .expect("the gate answered");
    assert_eq!(verdict.payload["error"]["code"], "trust.not_granted");
    assert!(verdict.payload["error"]["message"]
        .as_str()
        .unwrap()
        .contains("trust.json"));
    let decision = events
        .iter()
        .find(|e| e.event_type == trust_policy::DECISION)
        .expect("a reasoned decision accompanies the refusal");
    assert_eq!(decision.payload["verdict"], "denied");
    assert!(decision.reason.is_some());
    assert!(
        !library.path().join("new-skill").exists(),
        "the refused skill must not land"
    );
}

#[test]
fn a_pre_granted_admission_is_forwarded_silently() {
    let grants_dir = tempfile::tempdir().unwrap();
    let grants = grants_dir.path().join("trust.json");
    let library = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(elsewhere.path(), "new-skill");
    let source = elsewhere.path().join("new-skill");

    // The grant, as a frontend or the user would have written it: the
    // canonical key of the call's arguments plus the declared surface
    let key = trust_policy::admission_key(&json!({
        "source": source.display().to_string(),
        "reason": "the user asked for this capability",
    }));
    std::fs::write(
        &grants,
        serde_json::to_string_pretty(&json!({"grants": [{
            "key": key,
            "admits": "skills",
            "effects": install_skill_effects(),
            "summary": "test grant",
            "granted_by": "manual",
        }]}))
        .unwrap(),
    )
    .unwrap();

    let (mut kernel, _wake) = start("deny", &grants, library.path(), install_script(&source));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = events_of(&kernel);
    kernel.shutdown();

    assert!(
        library.path().join("new-skill/SKILL.md").is_file(),
        "the granted skill installs"
    );
    assert!(
        events
            .iter()
            .all(|e| e.event_type != trust_policy::AUTH_REQUESTED),
        "a granted admission asks no questions"
    );
}

#[test]
fn a_changed_appetite_does_not_match_the_old_grant() {
    let grants_dir = tempfile::tempdir().unwrap();
    let grants = grants_dir.path().join("trust.json");
    let library = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(elsewhere.path(), "new-skill");
    let source = elsewhere.path().join("new-skill");

    // Same key, but the grant was made when the tool declared a SMALLER
    // surface — the current declaration wants more, so it must not match
    let key = trust_policy::admission_key(&json!({
        "source": source.display().to_string(),
        "reason": "the user asked for this capability",
    }));
    std::fs::write(
        &grants,
        serde_json::to_string_pretty(&json!({"grants": [{
            "key": key,
            "admits": "skills",
            "effects": {"writes": ["<skills>"]},
            "summary": "an old, narrower grant",
            "granted_by": "manual",
        }]}))
        .unwrap(),
    )
    .unwrap();

    let (mut kernel, _wake) = start("deny", &grants, library.path(), install_script(&source));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = events_of(&kernel);
    kernel.shutdown();

    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED
                && e.payload["error"]["code"] == "trust.not_granted"),
        "a grant made for a narrower surface must not admit a wider one"
    );
    assert!(!library.path().join("new-skill").exists());
}

#[test]
fn failed_grant_persistence_settles_without_forwarding_or_claiming_success() {
    let home = tempfile::tempdir().unwrap();
    let grants = home.path().join("trust.json");
    std::fs::write(&grants, "not json").unwrap();
    let library = tempfile::tempdir().unwrap();
    let source = home.path().join("new-skill");
    write_skill(home.path(), "new-skill");
    let (mut kernel, _wake) = start("ask", &grants, library.path(), install_script(&source));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    kernel.run_until_quiescent().unwrap();
    let requested = events_of(&kernel)
        .into_iter()
        .find(|e| e.event_type == trust_policy::AUTH_REQUESTED)
        .unwrap();
    kernel.injector("human").emit(
        "say",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": trust_policy::AUTH_CHANNEL,
            "request": requested.id, "approve": true}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let events = events_of(&kernel);
    let failures: Vec<_> = events
        .iter()
        .filter(|e| {
            e.event_type == ce::TOOL_EXEC_COMPLETED
                && e.payload["error"]["code"] == "trust.persistence"
        })
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].causes.len(),
        2,
        "failure joins request and authorization"
    );
    assert!(!events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "trust"));
    assert!(!events
        .iter()
        .any(|e| e.event_type == trust_policy::DECISION && e.payload["verdict"] == "granted"));
    assert!(!library.path().join("new-skill").exists());
    assert_eq!(std::fs::read_to_string(grants).unwrap(), "not json");
    assert!(
        events.iter().any(|e| e.event_type == ce::OUTPUT_REPLY),
        "the turn does not hang"
    );
    kernel.shutdown();
}

#[test]
fn the_ask_stance_runs_the_authorization_pair_and_approval_grants_durably() {
    let grants_dir = tempfile::tempdir().unwrap();
    let grants = grants_dir.path().join("trust.json");
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(elsewhere.path(), "new-skill");
    let source = elsewhere.path().join("new-skill");

    // Run one: ask → approve → install proceeds with joined causes
    let library = tempfile::tempdir().unwrap();
    let (mut kernel, _wake) = start("ask", &grants, library.path(), install_script(&source));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    kernel.run_until_quiescent().unwrap();

    let requested = events_of(&kernel)
        .into_iter()
        .find(|e| e.event_type == trust_policy::AUTH_REQUESTED)
        .expect("the question is on the record");
    assert!(
        events_of(&kernel)
            .iter()
            .all(|e| e.event_type != ce::TOOL_EXEC_COMPLETED),
        "while the question stands, nothing is answered — the turn waits"
    );

    // The human approves — external input on the authorization channel (a
    // root input, like any user message)
    kernel.injector("human").emit(
        "say",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": trust_policy::AUTH_CHANNEL,
                   "request": requested.id, "approve": true, "note": "looks fine"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let events = events_of(&kernel);

    assert!(
        library.path().join("new-skill/SKILL.md").is_file(),
        "approval lets the install proceed"
    );
    let forward = events
        .iter()
        .rfind(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "trust")
        .expect("the gate forwarded the call");
    assert_eq!(
        forward.causes.len(),
        2,
        "the forward joins the reviewed call and the human's answer"
    );
    let granted = events
        .iter()
        .find(|e| e.event_type == trust_policy::DECISION && e.payload["verdict"] == "granted")
        .expect("the granting is a reasoned decision");
    assert!(granted.reason.is_some());
    kernel.shutdown();

    // What the gate actually wrote must satisfy the canon that describes it.
    // The file is the durable half of a security decision and the canon is
    // what a second implementation would read; neither had ever been checked
    // against the other.
    let store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&grants).unwrap()).unwrap();
    let canon: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/trust_grants.json")).unwrap();
    if let Err(problem) = jsonschema::validator_for(&canon).unwrap().validate(&store) {
        panic!("the grant store this version writes violates its canon: {problem}");
    }

    // Run two: a fresh conversation, same grants file — no question asked
    let library2 = tempfile::tempdir().unwrap();
    let (mut kernel, _wake) = start("ask", &grants, library2.path(), install_script(&source));
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it again"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = events_of(&kernel);
    kernel.shutdown();
    assert!(
        events
            .iter()
            .all(|e| e.event_type != trust_policy::AUTH_REQUESTED),
        "the grant is durable across conversations"
    );
    assert!(library2.path().join("new-skill/SKILL.md").is_file());
}

/// The product path end to end: the frontend drives the session, the gate's
/// card comes back as a rendered event, and Session::authorize — the same
/// call the TUI's y key makes — answers it through ui.answer. No test-only
/// answerer component: this is exactly the wiring the preset ships.
#[test]
fn the_frontend_answers_the_card_through_the_session() {
    let grants_dir = tempfile::tempdir().unwrap();
    let grants = grants_dir.path().join("trust.json");
    let library = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(elsewhere.path(), "new-skill");
    let source = elsewhere.path().join("new-skill");

    let script = install_script(&source);
    let grants_path = grants.display().to_string();
    let library_path = library.path().display().to_string();
    let session = lattice::Session::spawn("ui", move |render_tx| {
        let registry: HashMap<String, ComponentManifest> = [
            (silent_ui::NAME.to_string(), silent_ui::manifest()),
            (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
            (scripted_model::NAME.to_string(), scripted_model::manifest()),
            (trust_policy::NAME.to_string(), trust_policy::manifest()),
            (skill_library::NAME.to_string(), skill_library::manifest()),
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
        factories.insert(
            trust_policy::NAME.to_string(),
            Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
        );
        factories.insert(
            skill_library::NAME.to_string(),
            Box::new(|c| Box::new(skill_library::SkillLibrary::from_config(c))),
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
                    "trust".to_string(),
                    ComponentInstance {
                        component: trust_policy::NAME.to_string(),
                        requires: Vec::new(),
                        config: Some(json!({"stance": "ask", "grants": grants_path})),
                    },
                ),
                (
                    "skills".to_string(),
                    ComponentInstance {
                        component: skill_library::NAME.to_string(),
                        requires: Vec::new(),
                        config: Some(json!({"dirs": [library_path]})),
                    },
                ),
            ]
            .into(),
            wires: vec![
                Wire::new("ui.user", "loop.input"),
                Wire::new("loop.ask", "model.request"),
                Wire::new("model.result", "loop.model"),
                Wire::new("loop.run", "trust.review"),
                Wire::new("trust.forward", "skills.execute"),
                Wire::new("trust.verdict", "loop.tools"),
                Wire::new("skills.outcome", "loop.tools"),
                // The preset's answer wiring, verbatim
                Wire::new("ui.answer", "trust.answer"),
                Wire::new("loop.out", "ui.display"),
            ],
        };
        let mut kernel = Kernel::start(
            &assembly,
            &registry,
            &mut factories,
            KernelOptions::default(),
        )?;
        kernel.subscribe_log(move |event| {
            let _ = render_tx.send(lattice::RenderEvent::Appended(Box::new(event.clone())));
        });
        Ok(kernel)
    })
    .unwrap();

    session.send_text("install it");
    // The card arrives as a rendered event — what the TUI folds and shows
    let request = loop {
        match session.next_render() {
            Some(lattice::RenderEvent::Appended(event)) => {
                if event.event_type == trust_policy::AUTH_REQUESTED {
                    break event.id.clone();
                }
            }
            Some(_) => continue,
            None => panic!("the session ended before the card appeared"),
        }
    };

    // The y key
    session.authorize(&request, true);
    // The install proceeds: wait for the skills provider's completion
    loop {
        match session.next_render() {
            Some(lattice::RenderEvent::Appended(event)) => {
                if event.event_type == ce::TOOL_EXEC_COMPLETED
                    && event.source == "skills"
                    && event.payload["status"] == "ok"
                {
                    break;
                }
            }
            Some(_) => continue,
            None => panic!("the session ended before the install completed"),
        }
    }
    session.shutdown();

    assert!(library.path().join("new-skill/SKILL.md").is_file());
    assert!(grants.exists(), "the approval was recorded durably");
}

#[test]
fn a_refused_answer_settles_the_call_and_grants_nothing() {
    let grants_dir = tempfile::tempdir().unwrap();
    let grants = grants_dir.path().join("trust.json");
    let library = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write_skill(elsewhere.path(), "new-skill");

    let (mut kernel, _wake) = start(
        "ask",
        &grants,
        library.path(),
        install_script(&elsewhere.path().join("new-skill")),
    );
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "install it"})),
    );
    kernel.run_until_quiescent().unwrap();
    let requested = events_of(&kernel)
        .into_iter()
        .find(|e| e.event_type == trust_policy::AUTH_REQUESTED)
        .unwrap();

    kernel.injector("human").emit(
        "say",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel": trust_policy::AUTH_CHANNEL,
                   "request": requested.id, "approve": false}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    let events = events_of(&kernel);
    kernel.shutdown();

    assert!(
        events
            .iter()
            .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED
                && e.payload["error"]["code"] == "trust.refused"),
        "refusal settles the waiting call as an error"
    );
    assert!(!library.path().join("new-skill").exists());
    assert!(
        !grants.exists(),
        "a refusal must not write anything into the grant store"
    );
}
