//! Exercise the real review, admission and deletion-confirmation route.
use super::catalog::Catalog;
use super::catalog_tests::setup;
use super::management::{DELETE, SAVE};
use crate::components::{expert_definitions as provider, expert_ui, trust_policy as trust};
use crate::{
    core_events as ce, AssemblyManifest, Component, ComponentInstance, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, Wire,
};
use serde_json::{json, Value};
use std::collections::HashMap;

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _: &str, _: &EventEnvelope, _: &mut Ctx) {}
}
fn kernel(catalog: &Catalog, path: &std::path::Path) -> Kernel {
    let mut driver = provider::manifest();
    driver.name = "driver".into();
    driver.entry = "builtin:driver".into();
    driver.tools.clear();
    driver.events.clear();
    driver.implements.clear();
    driver.inputs = vec![PortDecl::new(
        "questions",
        &[trust::AUTH_REQUESTED, provider::AUTH_REQUESTED],
    )];
    driver.outputs = vec![
        PortDecl::new("calls", &[ce::TOOL_EXEC_STARTED]),
        PortDecl::new("answers", &[ce::EXTERNAL_INPUT]),
        PortDecl::new("cancel", &[ce::INTERRUPTED]),
    ];
    let registry = [
        ("driver".into(), driver),
        (expert_ui::NAME.into(), expert_ui::manifest()),
        (provider::NAME.into(), provider::manifest()),
        (provider::review::NAME.into(), provider::review::manifest()),
        (trust::NAME.into(), trust::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".into(), Box::new(|_| Box::new(Driver)));
    factories.insert(
        expert_ui::NAME.into(),
        Box::new(|_| Box::new(expert_ui::ExpertUi::default())),
    );
    factories.insert(
        provider::NAME.into(),
        Box::new(|c| Box::new(provider::ExpertDefinitions::from_config(c))),
    );
    factories.insert(
        provider::review::NAME.into(),
        Box::new(|c| Box::new(provider::review::ExpertReview::from_config(c))),
    );
    factories.insert(
        trust::NAME.into(),
        Box::new(|c| Box::new(trust::TrustPolicy::from_config(c))),
    );
    let config = serde_json::to_value(&catalog.config).unwrap();
    Kernel::start(
        &AssemblyManifest {
            instances: [
                ("driver".into(), ComponentInstance::new("driver", None)),
                (
                    "expert-ui".into(),
                    ComponentInstance::new(expert_ui::NAME, None),
                ),
                (
                    "definitions".into(),
                    ComponentInstance::new(provider::NAME, Some(config.clone())),
                ),
                (
                    "review".into(),
                    ComponentInstance::new(provider::review::NAME, Some(config)),
                ),
                (
                    "trust".into(),
                    ComponentInstance::new(
                        trust::NAME,
                        Some(json!({"stance":"ask","grants":path.with_extension("grants.json")})),
                    ),
                ),
            ]
            .into(),
            wires: vec![
                Wire::new("driver.calls", "review.review"),
                Wire::new("driver.answers", "expert-ui.input"),
                Wire::new("expert-ui.run", "review.review"),
                Wire::new("review.verdict", "expert-ui.completed"),
                Wire::new("trust.verdict", "expert-ui.completed"),
                Wire::new("definitions.outcome", "expert-ui.completed"),
                Wire::new("definitions.interrupted", "expert-ui.completed"),
                Wire::new("review.forward", "trust.review"),
                Wire::new("trust.forward", "definitions.execute"),
                Wire::new("driver.answers", "trust.answer"),
                Wire::new("driver.answers", "definitions.answer"),
                Wire::new("driver.cancel", "definitions.control"),
                Wire::new("trust.request", "driver.questions"),
                Wire::new("definitions.request", "driver.questions"),
            ],
        },
        &registry,
        &mut factories,
        KernelOptions {
            log_file: Some(path.into()),
            ..Default::default()
        },
    )
    .unwrap()
}
fn call(kernel: &mut Kernel, tool: &str, arguments: Value) {
    kernel.injector("driver").emit("calls", EventDraft::new(ce::TOOL_EXEC_STARTED, &[], json!({"call":format!("call-{}", kernel.log().len()),"tool":tool,"arguments":arguments})));
    kernel.run_until_quiescent().unwrap();
}
fn question(kernel: &Kernel, kind: &str) -> EventEnvelope {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rev()
        .find(|event| event.event_type == kind)
        .expect("human confirmation")
}
fn answer(kernel: &mut Kernel, question: &EventEnvelope, approve: bool) {
    kernel.injector("driver").emit(
        "answers",
        EventDraft::new(
            ce::EXTERNAL_INPUT,
            &[&question.id],
            json!({"channel":trust::AUTH_CHANNEL,"request":question.id,"approve":approve}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
}
fn outcomes(kernel: &Kernel) -> Vec<EventEnvelope> {
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ce::TOOL_EXEC_COMPLETED)
        .collect()
}
#[test]
fn overlapping_roots_cannot_mutate_a_personal_expert_through_a_project_alias() {
    use super::catalog::mutation_arguments;
    let (dir, original, candidate, _) = setup();
    let mut config = original.config.clone();
    config.workspace = config.home.clone();
    config.defaults.workspace = Some(config.home.to_string_lossy().into_owned());
    let catalog = Catalog::new(config).unwrap();
    super::tests::put(
        &catalog.definitions,
        super::Scope::Personal,
        &serde_json::to_vec(&candidate.definition).unwrap(),
    );
    let personal = catalog.inspect("personal:reviewer", None).unwrap();
    let mut kernel = kernel(&catalog, &dir.path().join("overlap.jsonl"));
    let mut activate = mutation_arguments(&personal, "activate").unwrap();
    activate["reason"] = json!("Activate the personal definition");
    call(&mut kernel, super::activation::ACTIVATE, activate);
    let q = question(&kernel, trust::AUTH_REQUESTED);
    answer(&mut kernel, &q, true);
    let activated = outcomes(&kernel).last().unwrap().payload["result"]["details"].clone();
    assert_eq!(activated["ready"], true);
    let mut project_delete = mutation_arguments(&activated, "delete").unwrap();
    project_delete["target"]["scope"] = json!("project");
    project_delete["expectedActivation"] = Value::Null;
    project_delete["reason"] = json!("Try deleting the same physical file through another scope");
    call(&mut kernel, DELETE, project_delete);
    let results = outcomes(&kernel);
    assert_eq!(
        results.len(),
        2,
        "the colliding scope must be rejected before deletion confirmation"
    );
    assert_eq!(results[1].payload["status"], "error");
    assert!(catalog.resolve("personal:reviewer", None).is_ok());
    assert!(catalog.inspect("project:reviewer", None).is_err());
    assert_eq!(catalog.names().unwrap(), ["personal:reviewer"]);
    assert_eq!(
        catalog.management_listing(None).unwrap()["projectAvailable"],
        false
    );

    let mut delete = mutation_arguments(&activated, "delete").unwrap();
    delete["reason"] = json!("Delete through the valid personal identity");
    call(&mut kernel, DELETE, delete);
    let q = question(&kernel, provider::AUTH_REQUESTED);
    answer(&mut kernel, &q, true);
    assert_eq!(
        outcomes(&kernel).last().unwrap().payload["result"]["deleted"],
        true
    );
    let absent = catalog.inspect("personal:reviewer", None).unwrap();
    let mut recreate = mutation_arguments(&absent, "put").unwrap();
    recreate["definition"] = personal["definition"].clone();
    recreate["reason"] = json!("Recreate identical content without reactivation");
    call(&mut kernel, SAVE, recreate);
    let q = question(&kernel, trust::AUTH_REQUESTED);
    answer(&mut kernel, &q, true);
    assert_eq!(
        outcomes(&kernel).last().unwrap().payload["result"]["details"]["state"],
        "pending"
    );
    assert!(catalog.resolve("personal:reviewer", None).is_err());
}

#[test]
fn managed_results_chain_save_activate_and_delete_without_reinspection() {
    use super::catalog::mutation_arguments;
    let (dir, catalog, _, _) = setup();
    let mut kernel = kernel(&catalog, &dir.path().join("chain.jsonl"));
    call(
        &mut kernel,
        provider::INSPECT,
        json!({"expert":"project:reviewer"}),
    );
    let details = outcomes(&kernel).last().unwrap().payload["result"].clone();
    let mut save = mutation_arguments(&details, "put").unwrap();
    save["definition"]["instructions"] = json!("Review the newly selected boundary.");
    save["reason"] = json!("Save the selected revision");
    call(&mut kernel, SAVE, save);
    let q = question(&kernel, trust::AUTH_REQUESTED);
    answer(&mut kernel, &q, true);
    let saved = outcomes(&kernel).last().unwrap().payload["result"]["details"].clone();
    assert_eq!(saved["state"], "pending");
    let mut activate = mutation_arguments(&saved, "activate").unwrap();
    activate["reason"] = json!("Activate the saved revision");
    call(&mut kernel, super::activation::ACTIVATE, activate);
    let q = question(&kernel, trust::AUTH_REQUESTED);
    answer(&mut kernel, &q, true);
    let activated = outcomes(&kernel).last().unwrap().payload["result"].clone();
    assert_eq!(activated["activated"], true);
    assert_eq!(activated["details"]["ready"], true);
    assert_ne!(activated["details"]["activation"], saved["activation"]);
    assert_eq!(activated["details"]["fileVersion"], saved["fileVersion"]);
    let mut delete = mutation_arguments(&activated["details"], "delete").unwrap();
    delete["reason"] = json!("Remove the inspected revision");
    call(&mut kernel, DELETE, delete);
    let q = question(&kernel, provider::AUTH_REQUESTED);
    answer(&mut kernel, &q, true);
    let results = outcomes(&kernel);
    assert_eq!(
        results.len(),
        4,
        "one inspection followed by three mutations"
    );
    assert!(results.iter().all(|event| event.payload["status"] == "ok"));
    assert_eq!(results.last().unwrap().payload["result"]["deleted"], true);
    assert!(catalog.candidate("project:reviewer").is_err());
}

#[test]
fn managed_save_waits_for_reviewed_admission_and_returns_pending_revision() {
    let (dir, catalog, _, _) = setup();
    let mut kernel = kernel(&catalog, &dir.path().join("management.jsonl"));
    let mut arguments = super::catalog::mutation_arguments(
        &catalog.inspect("project:reviewer", None).unwrap(),
        "put",
    )
    .unwrap();
    arguments["reason"] = json!("Update the review instructions");
    arguments["definition"]["instructions"] = json!("Review changed boundaries only.");
    call(&mut kernel, SAVE, arguments);
    let question = question(&kernel, trust::AUTH_REQUESTED);
    assert!(question.payload["summary"]
        .as_str()
        .unwrap()
        .contains("Review changed boundaries only."));
    assert!(outcomes(&kernel).is_empty());
    assert!(catalog.resolve("project:reviewer", None).is_ok());
    answer(&mut kernel, &question, true);
    let outcomes = outcomes(&kernel);
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].payload["status"], "ok", "{:?}", outcomes[0]);
    assert_eq!(outcomes[0].payload["result"]["ready"], false);
    assert!(catalog.resolve("project:reviewer", None).is_err());
}
#[test]
fn deletion_refusal_and_late_approval_never_remove_the_definition() {
    let (dir, catalog, candidate, _) = setup();
    let mut kernel = kernel(&catalog, &dir.path().join("management.jsonl"));
    let mut arguments = super::catalog::mutation_arguments(
        &catalog.inspect("project:reviewer", None).unwrap(),
        "delete",
    )
    .unwrap();
    arguments["reason"] = json!("Remove unused reviewer");
    call(&mut kernel, DELETE, arguments);
    let question = question(&kernel, provider::AUTH_REQUESTED);
    assert!(outcomes(&kernel).is_empty());
    assert_eq!(question.payload["confirmation"], "expert-delete");
    assert!(question.payload.get("admits").is_none());
    answer(&mut kernel, &question, false);
    answer(&mut kernel, &question, true);
    assert_eq!(outcomes(&kernel).len(), 1);
    assert_eq!(
        catalog.candidate("project:reviewer").unwrap().file_version,
        candidate.file_version
    );
    assert!(catalog.resolve("project:reviewer", None).is_ok());
}
#[test]
fn deletion_rechecks_the_file_after_confirmation_and_accepts_only_the_current_revision() {
    let (dir, catalog, candidate, _) = setup();
    let mut kernel = kernel(&catalog, &dir.path().join("management.jsonl"));
    let mut arguments = super::catalog::mutation_arguments(
        &catalog.inspect("project:reviewer", None).unwrap(),
        "delete",
    )
    .unwrap();
    arguments["reason"] = json!("Remove unused reviewer");
    call(&mut kernel, DELETE, arguments);
    let first = question(&kernel, provider::AUTH_REQUESTED);
    let path = catalog
        .definitions
        .path(candidate.identity.scope, &candidate.identity.id)
        .unwrap();
    let changed = format!("{}\n", std::fs::read_to_string(&path).unwrap());
    std::fs::write(&path, &changed).unwrap();
    answer(&mut kernel, &first, true);
    assert_eq!(outcomes(&kernel)[0].payload["status"], "error");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
    let mut arguments = super::catalog::mutation_arguments(
        &catalog.inspect("project:reviewer", None).unwrap(),
        "delete",
    )
    .unwrap();
    arguments["reason"] = json!("Remove the freshly inspected revision");
    call(&mut kernel, DELETE, arguments);
    let second = question(&kernel, provider::AUTH_REQUESTED);
    answer(&mut kernel, &second, true);
    answer(&mut kernel, &second, true);
    assert_eq!(outcomes(&kernel).len(), 2);
    assert_eq!(outcomes(&kernel)[1].payload["status"], "ok");
    assert!(!path.exists());
}

#[test]
fn frontend_bridge_uses_the_real_gate_without_creating_a_model_turn() {
    let (dir, catalog, _, _) = setup();
    let mut kernel = kernel(&catalog, &dir.path().join("management.jsonl"));
    let mut arguments = super::catalog::mutation_arguments(
        &catalog.inspect("project:reviewer", None).unwrap(),
        "put",
    )
    .unwrap();
    arguments["reason"] = json!("Save from the expert panel");
    arguments["definition"]["instructions"] = json!("Review the chosen boundary.");
    kernel.injector("driver").emit("answers", EventDraft::new(ce::EXTERNAL_INPUT, &[], json!({
        "channel":expert_ui::CHANNEL,"request":"panel-request","operation":"save","arguments":arguments,
        "effects":{"reads":[],"writes":[],"network":[],"executes":false}
    })));
    kernel.run_until_quiescent().unwrap();
    let question = question(&kernel, trust::AUTH_REQUESTED);
    assert!(outcomes(&kernel).is_empty());
    answer(&mut kernel, &question, true);
    let events = kernel.log().replay(1).unwrap();
    let results: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == expert_ui::RESULT)
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].payload["request"], "panel-request");
    assert_eq!(results[0].payload["status"], "ok");
    assert_eq!(outcomes(&kernel).len(), 1);
    assert!(!events.iter().any(|event| matches!(
        event.event_type.as_str(),
        ce::MODEL_CALL_STARTED | ce::USER_MESSAGE | ce::WAKE | ce::OUTPUT_REPLY
    )));
    let request = events
        .iter()
        .find(|event| event.event_type == ce::TOOL_EXEC_STARTED && event.source == "expert-ui")
        .unwrap();
    assert_eq!(request.payload["purpose"], expert_ui::PURPOSE);
    assert_eq!(request.causes.len(), 1);
    let origin = kernel
        .log()
        .reader()
        .get(&request.causes[0])
        .unwrap()
        .unwrap();
    assert_eq!(origin.event_type, ce::EXTERNAL_INPUT);
    assert!(origin.causes.is_empty());
    assert!(crate::components::call_purpose::auxiliary(
        &kernel.log().reader(),
        &outcomes(&kernel)[0]
    )
    .unwrap());
}
