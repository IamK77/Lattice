//! Production activation/resolution/opening with a scripted model as transport.
use super::*;
use lattice::components::{expert_definitions, trust_policy};
use lattice::experts::activation::ACTIVATE;
use lattice::experts::catalog::{Catalog, Config};
use lattice::experts::{Definition, Scope};
use lattice::preset::PresetConfig;
use lattice::subagent_host::MainAndChildren;
use lattice::{Kernel, KernelOptions};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Fixture {
    parent: Kernel,
    wake: mpsc::Receiver<()>,
    template: StreamTemplate,
    config: Config,
    definition: Definition,
    question: EventEnvelope,
    root: tempfile::TempDir,
}

impl Fixture {
    fn begin(background: bool, write: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(root.path().join("probe.txt"), "original").unwrap();
        let model_catalog = root.path().join("models.json");
        std::fs::write(&model_catalog, serde_json::to_vec(&json!({"models":{"selected":{
            "adapter":"scripted","model":"provider-model-A","baseUrl":"https://unused.invalid", "apiKeyEnv":"SYNTHETIC_UNUSED_KEY"
        }}})).unwrap()).unwrap();
        let config = Config {
            workspace: root.path().to_owned(),
            home,
            model_catalog,
            gate: "trust".into(),
            defaults: PresetConfig {
                adapter: "scripted".into(),
                model: "unused-parent-default".into(),
                base_url: String::new(),
                key_env: String::new(),
                workspace: Some(root.path().to_str().unwrap().into()),
                context_window: 64000,
                usage_input_field: "input_tokens".into(),
                profile: None,
                catalog_problems: vec![],
                system: String::new(),
                thinking: None,
                overlay: None,
                assembly: None,
                scripted: Some(json!({"script":[
                    {"status":"ok","toolCalls":[{"id":"write","tool":"Write","arguments":{"path":"probe.txt","content":"changed"}}]},
                    {"status":"ok","text":"CAPTURED_REPLY"}
                ]})),
            },
        };
        let definition = Definition::parse(
            &serde_json::to_vec(&json!({
                "v":1,"id":"reviewer","name":"Reviewer","description":"Review the fixture",
                "instructions":"STANDING_INSTRUCTIONS_A","model":"selected",
                "capabilities":if write {vec!["read","write"]} else {vec!["read"]}
            }))
            .unwrap(),
        )
        .unwrap();
        let catalog = Catalog::new(config.clone()).unwrap();
        let path = catalog
            .definitions
            .path(Scope::Project, "reviewer")
            .unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&definition).unwrap()).unwrap();
        let mut activation = lattice::experts::catalog::mutation_arguments(
            &catalog.inspect("project:reviewer", None).unwrap(),
            "activate",
        )
        .unwrap();
        activation["reason"] = json!("Enable the inspected synthetic expert revision");
        let mut template = template(
            json!([
                {"status":"ok","toolCalls":[{"id":"activate","tool":ACTIVATE,"arguments":activation}]},
                {"status":"ok","toolCalls":[{"id":"delegate","tool":"ask","arguments":{"expert":"project:reviewer","prompt":"ONE_OFF_TASK","background":background}}]},
                {"status":"ok","text":"parent continued"},
                {"status":"ok","text":"parent received result"}
            ]),
            0,
        );
        template
            .assembly
            .instances
            .get_mut("subagent")
            .unwrap()
            .config
            .as_mut()
            .unwrap()["definitions"] = json!(config);
        template.registry.insert(
            expert_definitions::NAME.into(),
            expert_definitions::manifest(),
        );
        template.factories.insert(
            expert_definitions::NAME.into(),
            Box::new(|c| Box::new(expert_definitions::ExpertDefinitions::from_config(c))),
        );
        template.assembly.instances.insert(
            "expert-definitions".into(),
            instance(expert_definitions::NAME, Some(json!(config))),
        );
        template.registry.insert(
            expert_definitions::review::NAME.into(),
            expert_definitions::review::manifest(),
        );
        template.factories.insert(
            expert_definitions::review::NAME.into(),
            Box::new(|c| Box::new(expert_definitions::review::ExpertReview::from_config(c))),
        );
        template.assembly.instances.insert(
            "expert-review".into(),
            instance(expert_definitions::review::NAME, Some(json!(config))),
        );
        template
            .registry
            .insert(trust_policy::NAME.into(), trust_policy::manifest());
        template.factories.insert(
            trust_policy::NAME.into(),
            Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
        );
        template.assembly.instances.insert(
            "trust".into(),
            instance(
                trust_policy::NAME,
                Some(json!({"stance":"ask","grants":root.path().join("grants.json")})),
            ),
        );
        template
            .assembly
            .wires
            .retain(|wire| !(wire.from == "loop.run" && wire.to == "subagent.execute"));
        template.assembly.wires.extend([
            Wire::new("loop.run", "expert-review.review"),
            Wire::new("expert-review.forward", "trust.review"),
            Wire::new("expert-review.verdict", "loop.tools"),
            Wire::new("trust.forward", "subagent.execute"),
            Wire::new("trust.forward", "expert-definitions.execute"),
            Wire::new("expert-definitions.outcome", "loop.tools"),
            Wire::new("trust.verdict", "loop.tools"),
            Wire::new("ui.answer", "trust.answer"),
        ]);
        let mut parent = Kernel::start(
            &template.assembly,
            &template.registry,
            &mut template.factories,
            KernelOptions {
                stream: Some("chat".into()),
                log_file: Some(root.path().join("chat.jsonl")),
                ..Default::default()
            },
        )
        .unwrap();
        let wake = parent.take_wake_receiver().unwrap();
        parent.injector("ui").emit(
            "user",
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text":"activate and delegate"}),
            ),
        );
        parent.run_until_quiescent().unwrap();
        let question = parent
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == trust_policy::AUTH_REQUESTED)
            .unwrap();
        let description = lattice::view::authorization_description(&question.payload);
        for detail in [
            "project:reviewer",
            "STANDING_INSTRUCTIONS_A",
            "selected",
            "Capabilities:",
            "Reviewed revision:",
        ] {
            assert!(
                description.contains(detail),
                "missing authorization detail: {detail}"
            );
        }
        assert!(catalog
            .activations
            .read(&catalog.candidate("project:reviewer").unwrap().identity)
            .unwrap()
            .is_none());
        Self {
            parent,
            wake,
            template,
            config,
            definition,
            question,
            root,
        }
    }

    fn answer(&mut self, approve: bool) {
        self.parent.injector("ui").emit("answer",EventDraft::new(ce::EXTERNAL_INPUT,&[],json!({"channel":trust_policy::AUTH_CHANNEL,"request":self.question.id,"approve":approve})));
        self.parent.run_until_quiescent().unwrap();
    }

    fn accepted(&self) -> EventEnvelope {
        self.parent
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == subagent::STREAM_REQUESTED)
            .expect("accepted delegation")
    }

    fn execute(mut self, background: bool, writable: bool) {
        let accepted = self.accepted();
        assert_eq!(
            accepted.payload["execution"]["assembly"]["instances"]["ctx"]["config"]["modelName"],
            "provider-model-A"
        );
        let directory = self.root.path().to_owned();
        // No named template exists. Only the production snapshot path can start it.
        let mut children = StreamHost::new(HashMap::new())
            .with_ledger_path(move |stream| Some(directory.join(format!("{stream}.jsonl"))));
        let mut host = SubagentHost::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.parent.run_until_quiescent().unwrap();
            host.poll(&mut MainAndChildren {
                main_id: "chat",
                main: &mut self.parent,
                children: &mut children,
            })
            .unwrap();
            let events = self.parent.log().replay(1).unwrap();
            if events.iter().any(|event| {
                if background {
                    event.event_type == ce::WAKE && event.payload["body"]["job"] == 1
                } else {
                    event.event_type == ce::TOOL_EXEC_COMPLETED
                        && event.payload["call"] == "delegate"
                }
            }) {
                break;
            }
            self.wake
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("causal child completion");
        }
        let events = self.parent.log().replay(1).unwrap();
        let completions: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event_type == ce::TOOL_EXEC_COMPLETED && event.payload["call"] == "delegate"
            })
            .collect();
        assert_eq!(completions.len(), 1);
        if background {
            let wakes: Vec<_> = events
                .iter()
                .filter(|event| event.event_type == ce::WAKE && event.payload["body"]["job"] == 1)
                .collect();
            assert_eq!(wakes.len(), 1);
            assert_eq!(wakes[0].payload["body"]["text"], "CAPTURED_REPLY");
        } else {
            assert_eq!(completions[0].payload["result"]["text"], "CAPTURED_REPLY");
            assert!(!events.iter().any(|event| event.event_type == ce::WAKE));
        }
        assert_eq!(
            std::fs::read_to_string(self.root.path().join("probe.txt")).unwrap(),
            if writable { "changed" } else { "original" }
        );
        let child = from_ledger(self.root.path(), "chat-sub-1");
        let documents =
            lattice::document::documents_dir(&self.root.path().join("chat-sub-1.jsonl"));
        let requests: Vec<_> = child
            .iter()
            .filter(|event| event.source == "ctx" && event.event_type == ce::MODEL_CALL_STARTED)
            .collect();
        assert!(!requests.is_empty());
        for request in requests {
            let system =
                lattice::document::resolve(&request.payload["system"], Some(&documents)).unwrap();
            assert!(system.as_str().unwrap().contains("STANDING_INSTRUCTIONS_A"));
            let tools =
                lattice::document::resolve(&request.payload["tools"], Some(&documents)).unwrap();
            assert_eq!(
                tools
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["name"] == "Write"),
                writable
            );
        }
        self.parent.shutdown();
    }
}

#[test]
fn accepted_production_snapshot_survives_deleting_definition_and_model_catalog() {
    let mut fixture = Fixture::begin(true, false);
    fixture.answer(true);
    fixture.accepted();
    let catalog = Catalog::new(fixture.config.clone()).unwrap();
    std::fs::remove_file(
        catalog
            .definitions
            .path(Scope::Project, "reviewer")
            .unwrap(),
    )
    .unwrap();
    std::fs::remove_file(&fixture.config.model_catalog).unwrap();
    fixture.execute(true, false);
}

#[test]
fn accepted_production_snapshot_does_not_expand_when_definition_and_model_change() {
    let mut fixture = Fixture::begin(false, false);
    fixture.answer(true);
    fixture.accepted();
    let catalog = Catalog::new(fixture.config.clone()).unwrap();
    let mut changed = fixture.definition.clone();
    changed.instructions = "UNAPPROVED_INSTRUCTIONS_B".into();
    changed
        .capabilities
        .push(lattice::experts::Capability::Write);
    std::fs::write(
        catalog
            .definitions
            .path(Scope::Project, "reviewer")
            .unwrap(),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &fixture.config.model_catalog,
        "broken catalog after acceptance",
    )
    .unwrap();
    assert!(catalog
        .resolve("project:reviewer", Some(&fixture.parent.log().reader()))
        .is_err());
    fixture.execute(false, false);
}

#[test]
fn explicitly_activated_writable_definition_reaches_the_real_writer() {
    let mut fixture = Fixture::begin(false, true);
    fixture.answer(true);
    fixture.execute(false, true);
}

#[test]
fn changing_the_definition_while_activation_waits_commits_nothing() {
    let mut fixture = Fixture::begin(false, false);
    let catalog = Catalog::new(fixture.config.clone()).unwrap();
    let original = catalog.candidate("project:reviewer").unwrap();
    let mut changed = fixture.definition.clone();
    changed.instructions.push_str(" Changed during approval.");
    std::fs::write(
        catalog
            .definitions
            .path(Scope::Project, "reviewer")
            .unwrap(),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    fixture.answer(true);
    assert!(catalog
        .activations
        .read(&original.identity)
        .unwrap()
        .is_none());
    let events = fixture.parent.log().replay(1).unwrap();
    assert!(!events
        .iter()
        .any(|event| event.event_type == subagent::STREAM_REQUESTED));
    let answers: Vec<_> = events
        .iter()
        .filter(|event| {
            event.event_type == ce::TOOL_EXEC_COMPLETED && event.payload["call"] == "activate"
        })
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].payload["status"], "error");
    fixture.parent.shutdown();
}

#[test]
fn refused_activation_has_no_persistent_availability() {
    let mut fixture = Fixture::begin(true, false);
    fixture.answer(false);
    let catalog = Catalog::new(fixture.config.clone()).unwrap();
    assert!(catalog
        .activations
        .read(&catalog.candidate("project:reviewer").unwrap().identity)
        .unwrap()
        .is_none());
    assert!(!fixture
        .parent
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|event| event.event_type == subagent::STREAM_REQUESTED));
    fixture.parent.shutdown();
}

#[test]
fn restarting_restores_availability_without_reapplying_activation_or_starting_old_work() {
    let mut fixture = Fixture::begin(true, false);
    fixture.answer(true);
    fixture.accepted();
    let catalog = Catalog::new(fixture.config.clone()).unwrap();
    let identity = catalog.candidate("project:reviewer").unwrap().identity;
    let before = catalog.activations.read(&identity).unwrap().unwrap();
    fixture.parent.shutdown();
    // Restoration legitimately wakes the parent about the interrupted job.
    // Give that NEW model turn a reply, not a freshly reset activation script.
    fixture
        .template
        .assembly
        .instances
        .get_mut("model")
        .unwrap()
        .config =
        Some(json!({"script":[{"status":"ok","text":"Acknowledged the interrupted job"}]}));
    let mut reopened = Kernel::start(
        &fixture.template.assembly,
        &fixture.template.registry,
        &mut fixture.template.factories,
        KernelOptions {
            stream: Some("chat".into()),
            log_file: Some(fixture.root.path().join("chat.jsonl")),
            ..Default::default()
        },
    )
    .unwrap();
    reopened.run_until_quiescent().unwrap();
    let restored = Catalog::new(fixture.config.clone()).unwrap();
    restored
        .resolve("project:reviewer", Some(&reopened.log().reader()))
        .unwrap();
    restored.resolve("project:reviewer", None).unwrap();
    assert_eq!(
        restored.activations.read(&identity).unwrap().unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("probe.txt")).unwrap(),
        "original"
    );
    let events = reopened.log().replay(1).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == ce::TOOL_EXEC_COMPLETED
                && event.payload["call"] == "activate")
            .count(),
        1
    );
    let mut children = StreamHost::new(HashMap::new());
    SubagentHost::new()
        .poll(&mut MainAndChildren {
            main_id: "chat",
            main: &mut reopened,
            children: &mut children,
        })
        .unwrap();
    assert!(!fixture.root.path().join("chat-sub-1.jsonl").exists());
    reopened.shutdown();
}
