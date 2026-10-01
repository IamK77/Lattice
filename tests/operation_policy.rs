//! Real gate wiring: prefix grants persist, compose conservatively, and revoke.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lattice::components::{
    interface_permissions as ip, operation_policy as op, shell_tools, silent_ui, trust_policy,
};
use lattice::{
    core_events as ce, AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};

struct FakeRun;
impl Component for FakeRun {
    fn handle(&mut self, _: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        if event.event_type == ce::TOOL_EXEC_STARTED {
            ctx.emit("outcome", EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id],
                json!({"call":event.payload["call"],"status":"ok","result":event.payload["arguments"]})));
        }
    }
}

fn start(path: Option<&Path>, config: Value) -> Kernel {
    start_with_trust(path, config, None)
}

fn start_with_trust(
    path: Option<&Path>,
    config: Value,
    admission: Option<(&Path, &str)>,
) -> Kernel {
    let mut ui = silent_ui::manifest();
    ui.outputs
        .push(PortDecl::new("call", &[ce::TOOL_EXEC_STARTED]));
    ui.outputs
        .push(PortDecl::new("hold", &[ce::TOOL_EXEC_STARTED]));
    let mut permissions = ip::manifest();
    permissions
        .inputs
        .push(PortDecl::new("hold", &[ce::TOOL_EXEC_STARTED]));
    let mut tools = shell_tools::manifest();
    if let Some((_, admits)) = admission {
        for tool in &mut tools.tools {
            if tool["name"] == "Run" {
                tool["effects"]["admits"] = json!(admits);
            }
        }
    }
    let registry = [
        (silent_ui::NAME.into(), ui),
        (shell_tools::NAME.into(), tools),
        (trust_policy::NAME.into(), trust_policy::manifest()),
        (op::NAME.into(), op::manifest()),
        (ip::NAME.into(), permissions),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.into(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(vec![]))))),
    );
    factories.insert(shell_tools::NAME.into(), Box::new(|_| Box::new(FakeRun)));
    factories.insert(
        op::NAME.into(),
        Box::new(|c| Box::new(op::OperationPolicy::from_config(c))),
    );
    factories.insert(
        ip::NAME.into(),
        Box::new(|c| Box::new(ip::InterfacePermissions::from_config(c))),
    );
    factories.insert(
        trust_policy::NAME.into(),
        Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
    );
    let mut assembly = AssemblyManifest {
        instances: [
            ("ui", silent_ui::NAME, None),
            ("tools", shell_tools::NAME, None),
            ("operations", op::NAME, Some(config)),
            ("permissions", ip::NAME, None),
        ]
        .into_iter()
        .map(|(id, component, config)| {
            (
                id.into(),
                ComponentInstance {
                    component: component.into(),
                    requires: vec![],
                    config,
                },
            )
        })
        .collect(),
        wires: vec![
            Wire::new("ui.call", "operations.review"),
            Wire::new("ui.hold", "permissions.hold"),
            Wire::new("operations.forward", "tools.execute"),
            Wire::new("ui.answer", "operations.answer"),
            Wire::new("ui.answer", "permissions.control"),
            Wire::new("ui.interrupt", "operations.control"),
        ],
    };
    if let Some((grants, _)) = admission {
        assembly.instances.insert(
            "trust".into(),
            ComponentInstance {
                component: trust_policy::NAME.into(),
                requires: vec![],
                config: Some(json!({"stance":"ask","grants":grants})),
            },
        );
        assembly
            .wires
            .retain(|wire| wire.from != "operations.forward");
        assembly.wires.extend([
            Wire::new("operations.forward", "trust.review"),
            Wire::new("trust.forward", "tools.execute"),
            Wire::new("operations.answered", "trust.answer"),
        ]);
    }
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            stream: Some("operation-test".into()),
            log_file: path.map(Path::to_path_buf),
            ..KernelOptions::default()
        },
    )
    .unwrap()
}

fn events(kernel: &Kernel) -> Vec<EventEnvelope> {
    kernel.log().replay(1).unwrap()
}
fn settle(kernel: &mut Kernel) {
    kernel.run_until_quiescent().unwrap();
    let errors: Vec<_> = events(kernel)
        .into_iter()
        .filter(|e| e.event_type == ce::ERROR)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}
fn latest(kernel: &Kernel, kind: &str) -> EventEnvelope {
    events(kernel)
        .into_iter()
        .rev()
        .find(|e| e.event_type == kind)
        .unwrap()
}
fn call(kernel: &mut Kernel, id: &str, script: &str) {
    kernel.injector("ui").emit(
        "call",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":id,"tool":"Run","arguments":{"command":script}}),
        ),
    );
    settle(kernel);
}
fn answer(kernel: &mut Kernel, question: &EventEnvelope, approve: bool, scope: &str) {
    kernel.injector("ui").emit("answer", EventDraft::new(ce::EXTERNAL_INPUT, &[],
        json!({"channel":trust_policy::AUTH_CHANNEL,"request":question.id,"approve":approve,"scope":scope})));
    settle(kernel);
}
fn executed(kernel: &Kernel) -> Vec<String> {
    events(kernel)
        .iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "tools")
        .map(|e| e.payload["call"].as_str().unwrap().into())
        .collect()
}
fn grant_push(kernel: &mut Kernel) {
    call(kernel, "first", "git push origin main");
    assert!(executed(kernel).is_empty());
    let question = latest(kernel, op::AUTH_REQUESTED);
    assert_eq!(
        question.payload["grants"][0]["prefix"],
        json!(["git", "push", "origin"])
    );
    answer(kernel, &question, true, "flow");
    assert_eq!(executed(kernel), ["first"]);
}

#[test]
fn persistent_prefix_is_narrow_and_compound_requests_execute_only_after_full_approval() {
    let mut kernel = start(None, json!({"stance":"ask"}));
    grant_push(&mut kernel);
    call(&mut kernel, "same-remote", "git push origin dev");
    assert_eq!(executed(&kernel), ["first", "same-remote"]);
    call(
        &mut kernel,
        "compound",
        "git push origin main && git commit -m update",
    );
    assert_eq!(
        executed(&kernel),
        ["first", "same-remote"],
        "no partial execution"
    );
    let question = latest(&kernel, op::AUTH_REQUESTED);
    answer(&mut kernel, &question, true, "once");
    let outcome = latest(&kernel, ce::TOOL_EXEC_COMPLETED);
    assert_eq!(
        outcome.payload["result"]["command"],
        "git push origin main && git commit -m update"
    );
    call(&mut kernel, "other-remote", "git push upstream main");
    assert_eq!(
        latest(&kernel, op::AUTH_REQUESTED).payload["summary"],
        "Execute this shell operation: git push upstream main"
    );
    assert_eq!(executed(&kernel), ["first", "same-remote", "compound"]);
}

#[test]
fn flow_grants_survive_reopening_and_revocation_survives_another_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("flow.jsonl");
    let grant;
    {
        let mut kernel = start(Some(&path), json!({"stance":"ask"}));
        grant_push(&mut kernel);
        grant = op::read_grants(&kernel.log().reader(), "operations")
            .unwrap()
            .grants
            .keys()
            .next()
            .unwrap()
            .clone();
    }
    {
        let mut kernel = start(Some(&path), json!({"stance":"ask"}));
        call(&mut kernel, "resumed", "git push origin dev");
        assert_eq!(executed(&kernel), ["first", "resumed"]);
        kernel.injector("ui").emit(
            "answer",
            EventDraft::new(
                ce::EXTERNAL_INPUT,
                &[],
                json!({"channel":op::CHANNEL,"action":"revoke","grant":grant}),
            ),
        );
        settle(&mut kernel);
        assert!(op::read_grants(&kernel.log().reader(), "operations")
            .unwrap()
            .grants
            .is_empty());
    }
    let mut kernel = start(Some(&path), json!({"stance":"ask"}));
    call(&mut kernel, "after-revocation", "git push origin main");
    assert_eq!(executed(&kernel), ["first", "resumed"]);
    assert!(op::read_grants(&kernel.log().reader(), "operations")
        .unwrap()
        .grants
        .is_empty());
}

#[test]
fn duplicate_answers_and_answers_after_cancellation_do_not_execute_again() {
    let mut kernel = start(None, json!({"stance":"ask"}));
    call(&mut kernel, "once", "git status");
    let question = latest(&kernel, op::AUTH_REQUESTED);
    answer(&mut kernel, &question, true, "once");
    answer(&mut kernel, &question, true, "flow");
    assert_eq!(executed(&kernel), ["once"]);
    assert!(op::read_grants(&kernel.log().reader(), "operations")
        .unwrap()
        .grants
        .is_empty());
    call(&mut kernel, "cancelled", "git commit -m stop");
    let question = latest(&kernel, op::AUTH_REQUESTED);
    kernel.injector("ui").emit(
        "interrupt",
        EventDraft::new(ce::INTERRUPTED, &[], json!({"by":"user"})),
    );
    settle(&mut kernel);
    answer(&mut kernel, &question, true, "flow");
    assert_eq!(executed(&kernel), ["once"]);
    assert!(kernel
        .log()
        .reader()
        .has_outcome(question.payload["request"].as_str().unwrap())
        .unwrap());
    assert!(op::read_grants(&kernel.log().reader(), "operations")
        .unwrap()
        .grants
        .is_empty());
}

#[test]
fn bare_shell_and_unqualified_push_grants_do_not_broaden_later_requests() {
    for (first, later) in [
        ("bash", "echo $HOME"),
        ("env", "env bash -c 'echo later'"),
        ("git push", "git push upstream main"),
    ] {
        let mut kernel = start(None, json!({"stance":"ask"}));
        call(&mut kernel, "first", first);
        let question = latest(&kernel, op::AUTH_REQUESTED);
        answer(&mut kernel, &question, true, "flow");
        call(&mut kernel, "later", later);
        assert_eq!(
            executed(&kernel),
            ["first"],
            "granting {first:?} must not authorize {later:?}"
        );
    }
}

#[test]
fn a_delayed_copy_of_stopped_work_does_not_execute_but_new_work_can() {
    let mut kernel = start(
        None,
        json!({"rules":[{"pattern":["git"],"decision":"allow"}]}),
    );
    kernel.injector("ui").emit(
        "hold",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call":"held","tool":"Run","arguments":{"command":"git status"}}),
        ),
    );
    settle(&mut kernel);
    let held = latest(&kernel, ce::TOOL_EXEC_STARTED);
    kernel.injector("ui").emit(
        "interrupt",
        EventDraft::new(ce::INTERRUPTED, &[], json!({"by":"user"})),
    );
    settle(&mut kernel);
    kernel.injector("ui").emit(
        "call",
        EventDraft::new(ce::TOOL_EXEC_STARTED, &[&held.id], held.payload.clone()),
    );
    settle(&mut kernel);
    assert!(
        executed(&kernel).is_empty(),
        "a stop also covers requests delayed upstream"
    );
    let reviewed = latest(&kernel, ce::TOOL_EXEC_STARTED);
    assert!(kernel.log().reader().has_outcome(&reviewed.id).unwrap());
    assert!(ce::hanging_chain_heads(&events(&kernel), ce::TOOL_EXEC_STARTED).is_empty());
    call(&mut kernel, "fresh", "git status");
    assert_eq!(executed(&kernel), ["fresh"]);
}

#[test]
fn explicit_once_and_flow_answers_never_write_permanent_trust() {
    for scope in ["once", "flow"] {
        let temp = tempfile::tempdir().unwrap();
        let grants = temp.path().join("trust.json");
        let mut kernel = start_with_trust(None, json!({"shellTools":[]}), Some((&grants, "code")));
        call(&mut kernel, "first", "git status");
        let question = latest(&kernel, trust_policy::AUTH_REQUESTED);
        answer(&mut kernel, &question, true, scope);
        assert_eq!(executed(&kernel), ["first"]);
        assert!(!grants.exists(), "{scope} must not create permanent trust");
        call(&mut kernel, "second", "git status");
        if scope == "once" {
            assert_eq!(executed(&kernel), ["first"]);
            assert_ne!(
                latest(&kernel, trust_policy::AUTH_REQUESTED).id,
                question.id
            );
        } else {
            assert_eq!(executed(&kernel), ["first", "second"]);
        }
    }
}

#[test]
fn a_flow_admission_grant_does_not_cover_changed_effects_after_reopening() {
    let temp = tempfile::tempdir().unwrap();
    let grants = temp.path().join("trust.json");
    let ledger = temp.path().join("flow.jsonl");
    {
        let mut kernel = start_with_trust(
            Some(&ledger),
            json!({"shellTools":[]}),
            Some((&grants, "code")),
        );
        call(&mut kernel, "first", "git status");
        let question = latest(&kernel, trust_policy::AUTH_REQUESTED);
        answer(&mut kernel, &question, true, "flow");
        assert_eq!(executed(&kernel), ["first"]);
    }
    let mut kernel = start_with_trust(
        Some(&ledger),
        json!({"shellTools":[]}),
        Some((&grants, "instructions")),
    );
    call(&mut kernel, "changed", "git status");
    assert_eq!(executed(&kernel), ["first"]);
    assert_eq!(
        latest(&kernel, trust_policy::AUTH_REQUESTED).payload["admits"],
        "instructions"
    );
    assert!(!grants.exists());
}

#[test]
fn malformed_scope_cannot_fall_back_to_permanent_trust_or_consume_the_question() {
    let temp = tempfile::tempdir().unwrap();
    let grants = temp.path().join("trust.json");
    let mut kernel = start_with_trust(None, json!({"shellTools":[]}), Some((&grants, "code")));
    call(&mut kernel, "first", "git status");
    let question = latest(&kernel, trust_policy::AUTH_REQUESTED);
    for scope in [json!("oncc"), Value::Null, json!(true)] {
        kernel.injector("ui").emit("answer", EventDraft::new(ce::EXTERNAL_INPUT, &[],
            json!({"channel":trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true,"scope":scope})));
        kernel.run_until_quiescent().unwrap();
        assert!(executed(&kernel).is_empty());
        assert!(!grants.exists());
    }
    assert_eq!(
        events(&kernel)
            .iter()
            .filter(|e| e.event_type == op::DECISION && e.payload.get("answer").is_some())
            .count(),
        3
    );
    kernel.injector("ui").emit("answer", EventDraft::new(ce::EXTERNAL_INPUT, &[],
        json!({"channel":trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true,"scope":"once"})));
    kernel.run_until_quiescent().unwrap();
    assert_eq!(executed(&kernel), ["first"]);
    assert!(!grants.exists());
}

#[test]
fn a_legacy_answer_still_uses_the_existing_permanent_trust_choice() {
    let temp = tempfile::tempdir().unwrap();
    let grants = temp.path().join("trust.json");
    let mut kernel = start_with_trust(None, json!({"shellTools":[]}), Some((&grants, "code")));
    call(&mut kernel, "first", "git status");
    let question = latest(&kernel, trust_policy::AUTH_REQUESTED);
    kernel.injector("ui").emit(
        "answer",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[],
            json!({"channel":trust_policy::AUTH_CHANNEL,"request":question.id,"approve":true}),
        ),
    );
    settle(&mut kernel);
    assert!(grants.exists());
    call(&mut kernel, "second", "git status");
    assert_eq!(executed(&kernel), ["first", "second"]);
}

#[test]
fn forbidden_rules_and_malformed_rules_fail_closed() {
    for config in [
        json!({"stance":"ask","rules":[{"pattern":["git"],"decision":"forbidden"}]}),
        json!({"stance":"ask","rules":[{"pattern":[],"decision":"allow"}]}),
        json!({"stance":"ask","rules":"not a rule list"}),
        json!({"stance":"ask","shellTools":"Run"}),
        json!({"stance":"ask","shellTools":[false]}),
    ] {
        let mut kernel = start(None, config);
        call(&mut kernel, "denied", "git push origin main");
        assert!(executed(&kernel).is_empty());
        assert_eq!(
            latest(&kernel, ce::TOOL_EXEC_COMPLETED).payload["status"],
            "error"
        );
        assert!(!events(&kernel)
            .iter()
            .any(|e| e.event_type == op::AUTH_REQUESTED));
    }
}
