//! Architecture probe, not the custom-expert implementation.
//!
//! Real: trust policy, subagent, event causality, child assembly/kernel, host,
//! results. Synthetic: definition format/model catalog, admission declaration
//! on ask, and a late template adapter reading the accepted request's ancestry.
//! Nothing here exposes custom definitions or adds a production activation store.
use super::*;
use lattice::components::trust_policy;
use lattice::preset::{expert_assembly, PresetConfig, EXPERTS};
use lattice::subagent_host::MainAndChildren;
use lattice::{Kernel, KernelOptions};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn candidate(version: &str, writes: bool) -> Value {
    json!({
        "identity":"project:reviewer",
        "instructions":format!("STANDING_INSTRUCTIONS_{version}"),
        "model":{"id":format!("synthetic-{version}"),"reply":format!("ANSWER_{version}")},
        "capabilities":if writes {vec!["read", "write"]} else {vec!["read"]}
    })
}

fn revision(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn save(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn read_candidate(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

struct ProbeRun {
    candidate_path: PathBuf,
    parent: Kernel,
    wake: mpsc::Receiver<()>,
    question: EventEnvelope,
    reviewed: Value,
    background: bool,
    // Drop the kernel before removing the files it owns.
    root: tempfile::TempDir,
}

impl ProbeRun {
    fn begin(reviewed: Value, background: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let candidate_path = root.path().join("candidate.json");
        save(&candidate_path, &reviewed);
        std::fs::write(root.path().join("probe.txt"), "original").unwrap();
        // Candidate content is reviewed, not just a mutable path. The revision
        // is the synthetic template identity, NOT a proposed user-facing name.
        let args = json!({
            "expert":revision(&reviewed), "prompt":"ONE_OFF_TASK", "background":background,
            "candidate":read_candidate(&candidate_path), "candidatePath":candidate_path,
            "reason":"authorize this synthetic revision for the architecture probe"
        });
        let mut parent = template(
            json!([
                {"status":"ok","toolCalls":[{"id":"delegate","tool":"ask","arguments":args}]},
                {"status":"ok","text":"parent continued"},
                {"status":"ok","text":"parent received result"}
            ]),
            0,
        );
        parent
            .assembly
            .instances
            .get_mut("subagent")
            .unwrap()
            .config = Some(json!({
            "experts":[{"name":revision(&reviewed),"description":"synthetic immutable revision"}]
        }));
        let ask = parent
            .registry
            .get_mut(subagent::NAME)
            .unwrap()
            .tools
            .iter_mut()
            .find(|tool| tool["name"] == "ask")
            .unwrap();
        // The real ask tool has no admission marker. This is deliberately a
        // test-only producer declaration, not a silent change to its contract.
        ask["effects"]["admits"] = json!("test-expert-definition");
        ask["parameters"]["properties"]["candidate"] = json!({"type":"object"});
        ask["parameters"]["properties"]["candidatePath"] = json!({"type":"string"});
        ask["parameters"]["properties"]["reason"] = json!({"type":"string"});
        parent
            .registry
            .insert(trust_policy::NAME.into(), trust_policy::manifest());
        parent.factories.insert(
            trust_policy::NAME.into(),
            Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
        );
        parent.assembly.instances.insert(
            "trust".into(),
            instance(
                trust_policy::NAME,
                Some(json!({"stance":"ask","grants":root.path().join("grants.json")})),
            ),
        );
        parent
            .assembly
            .wires
            .retain(|w| !(w.from == "loop.run" && w.to == "subagent.execute"));
        parent.assembly.wires.extend([
            Wire::new("loop.run", "trust.review"),
            Wire::new("trust.forward", "subagent.execute"),
            Wire::new("trust.verdict", "loop.tools"),
            Wire::new("ui.answer", "trust.answer"),
        ]);
        let mut kernel = Kernel::start(
            &parent.assembly,
            &parent.registry,
            &mut parent.factories,
            KernelOptions {
                stream: Some("chat".into()),
                log_file: Some(root.path().join("chat.jsonl")),
                ..Default::default()
            },
        )
        .unwrap();
        let wake = kernel.take_wake_receiver().unwrap();
        kernel.injector("ui").emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"review and delegate"})),
        );
        kernel.run_until_quiescent().unwrap();
        let events = kernel.log().replay(1).unwrap();
        let question = events
            .iter()
            .find(|e| e.event_type == trust_policy::AUTH_REQUESTED)
            .unwrap()
            .clone();
        assert!(
            !events
                .iter()
                .any(|e| e.event_type == subagent::STREAM_REQUESTED
                    || e.event_type == ce::TOOL_EXEC_COMPLETED),
            "nothing may be accepted before authorization"
        );
        Self {
            root,
            candidate_path,
            parent: kernel,
            wake,
            question,
            reviewed,
            background,
        }
    }

    fn answer(&mut self, approve: bool) {
        // This request is the QUESTION event ID, not its held tool-call ID.
        self.parent.injector("ui").emit("answer", EventDraft::new(ce::EXTERNAL_INPUT, &[],
            json!({"channel":trust_policy::AUTH_CHANNEL,"request":self.question.id,"approve":approve})));
        self.parent.run_until_quiescent().unwrap();
    }

    fn accepted(&self) -> EventEnvelope {
        self.parent
            .log()
            .replay(1)
            .unwrap()
            .into_iter()
            .find(|e| e.event_type == subagent::STREAM_REQUESTED)
            .expect("delegation was accepted")
    }

    fn run_child(mut self) {
        let accepted = self.accepted();
        // The test-only late-opening adapter follows immutable audit ancestry.
        // It never resolves the candidate path again after acceptance.
        let forwarded = self
            .parent
            .log()
            .reader()
            .get(&accepted.causes[0])
            .unwrap()
            .unwrap();
        assert_eq!(forwarded.event_type, ce::TOOL_EXEC_STARTED);
        assert_eq!(forwarded.source, "trust");
        let frozen = forwarded.payload["arguments"]["candidate"].clone();
        assert_eq!(accepted.payload["expert"], revision(&frozen));
        assert_eq!(forwarded.payload["arguments"]["prompt"], "ONE_OFF_TASK");
        let child = child_template(self.root.path(), &frozen);
        let directory = self.root.path().to_owned();
        let mut children = StreamHost::new([(revision(&frozen), child)].into())
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
            if events.iter().any(|e| {
                if self.background {
                    e.event_type == ce::WAKE && e.payload["body"]["job"] == 1
                } else {
                    e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "delegate"
                }
            }) {
                break;
            }
            self.wake
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("causal host wake must arrive");
        }
        let parent = self.parent.log().replay(1).unwrap();
        let completions: Vec<_> = parent
            .iter()
            .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "delegate")
            .collect();
        assert_eq!(
            completions.len(),
            1,
            "one tool answer, never a second completion"
        );
        let report = if self.background {
            let wakes: Vec<_> = parent
                .iter()
                .filter(|e| e.event_type == ce::WAKE && e.payload["body"]["job"] == 1)
                .collect();
            assert_eq!(wakes.len(), 1);
            assert_eq!(
                wakes[0].payload["body"]["text"],
                self.reviewed["model"]["reply"]
            );
            wakes[0]
        } else {
            assert!(!parent.iter().any(|e| e.event_type == ce::WAKE));
            assert_eq!(
                completions[0].payload["result"]["text"],
                self.reviewed["model"]["reply"]
            );
            completions[0]
        };
        let child = from_ledger(self.root.path(), "chat-sub-1");
        let origin = report
            .origin
            .as_ref()
            .expect("report identifies child evidence");
        assert_eq!(origin.stream, "chat-sub-1");
        assert!(child.iter().any(|e| e.id == origin.event));
        let asks: Vec<_> = child
            .iter()
            .filter(|e| e.event_type == ce::MODEL_CALL_STARTED && e.source == "ctx")
            .collect();
        assert!(!asks.is_empty());
        let writable = self.reviewed["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("write"));
        let documents =
            lattice::contracts::document::documents_dir(&self.root.path().join("chat-sub-1.jsonl"));
        for ask in asks {
            let system =
                lattice::contracts::document::resolve(&ask.payload["system"], Some(&documents))
                    .unwrap();
            let tools =
                lattice::contracts::document::resolve(&ask.payload["tools"], Some(&documents))
                    .unwrap();
            assert!(system
                .as_str()
                .unwrap()
                .contains(self.reviewed["instructions"].as_str().unwrap()));
            let tools = tools.as_array().unwrap();
            assert_eq!(tools.iter().any(|t| t["name"] == "Write"), writable);
        }
        assert!(child
            .iter()
            .any(|e| e.event_type == ce::USER_MESSAGE && e.payload["text"] == "ONE_OFF_TASK"));
        assert_eq!(
            std::fs::read_to_string(self.root.path().join("probe.txt")).unwrap(),
            if writable { "changed" } else { "original" }
        );
        if !writable {
            assert!(
                child.iter().any(|e| e.event_type == ce::INTERRUPTED),
                "a forged write must settle without executing"
            );
        }
        self.parent.shutdown();
    }
}

fn child_template(root: &Path, frozen: &Value) -> StreamTemplate {
    let writable = frozen["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!("write"));
    let role = if writable { "worker" } else { "explorer" };
    let cfg = PresetConfig {
        adapter: "scripted".into(),
        model: frozen["model"]["id"].as_str().unwrap().into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(root.to_str().unwrap().into()),
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
            {"status":"ok","text":frozen["model"]["reply"]}
        ]})),
    };
    let (registry, factories, mut assembly) =
        expert_assembly(&cfg, EXPERTS.iter().find(|e| e.name == role).unwrap()).unwrap();
    assembly
        .instances
        .get_mut("ctx")
        .unwrap()
        .config
        .as_mut()
        .unwrap()["system"] = frozen["instructions"].clone();
    assembly.instances.get_mut("skills").unwrap().config =
        Some(json!({"dirs":[root.join("skills")]}));
    StreamTemplate {
        registry,
        factories,
        assembly,
    }
}

#[test]
fn changing_a_candidate_while_approval_waits_cannot_change_the_executed_revision() {
    let mut probe = ProbeRun::begin(candidate("A", false), true);
    save(&probe.candidate_path, &candidate("B", true));
    probe.answer(true);
    probe.run_child();
}

#[test]
fn changing_an_accepted_candidate_before_host_poll_does_not_change_execution() {
    let mut probe = ProbeRun::begin(candidate("A", false), false);
    probe.answer(true);
    let _ = probe.accepted();
    save(&probe.candidate_path, &candidate("B", true));
    probe.run_child();
}

#[test]
fn deleting_an_accepted_candidate_before_host_poll_keeps_the_snapshot_runnable() {
    let mut probe = ProbeRun::begin(candidate("A", false), true);
    probe.answer(true);
    let _ = probe.accepted();
    std::fs::remove_file(&probe.candidate_path).unwrap();
    probe.run_child();
}

#[test]
fn explicitly_approving_the_other_revision_changes_instructions_model_behavior_and_tools() {
    let mut probe = ProbeRun::begin(candidate("B", true), false);
    probe.answer(true);
    probe.run_child();
}

#[test]
fn refusal_produces_one_error_and_no_accepted_delegation() {
    let mut probe = ProbeRun::begin(candidate("A", false), true);
    probe.answer(false);
    let events = probe.parent.log().replay(1).unwrap();
    assert!(!events
        .iter()
        .any(|e| e.event_type == subagent::STREAM_REQUESTED));
    let endings: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .collect();
    assert_eq!(endings.len(), 1);
    assert_eq!(endings[0].payload["status"], "error");
    assert_eq!(
        std::fs::read_to_string(probe.root.path().join("probe.txt")).unwrap(),
        "original"
    );
    probe.parent.shutdown();
}
