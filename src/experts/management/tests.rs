use super::*;
use crate::experts::catalog_tests::setup;
use crate::experts::Scope;

fn change(catalog: &Catalog, id: &str) -> AuditRef {
    AuditRef {
        ledger: catalog.config.home.join("synthetic.ledger"),
        stream: "synthetic".into(),
        event: id.into(),
    }
}

fn put_arguments(details: &Value, definition: &Definition) -> Value {
    json!({"operation":"put","target":details["target"],"fileVersion":details["fileVersion"],
        "expectedActivation":details["activation"],"definition":definition,"reason":"Save reviewed instructions"})
}

#[test]
fn saving_a_changed_definition_withdraws_availability_and_returns_activation_arguments() {
    let (_directory, catalog, original, _) = setup();
    let details = catalog.inspect("project:reviewer", None).unwrap();
    assert_eq!(details["ready"], true);
    let mut proposed = original.definition.clone();
    proposed.instructions.push_str(" Check cancellation too.");
    let request = put_arguments(&details, &proposed);
    let review = catalog.review_mutation(&request).unwrap();
    assert!(review.contains(&original.definition.instructions));
    assert!(review.contains(&proposed.instructions));
    let result = catalog
        .apply_mutation(&request, change(&catalog, "save-1"))
        .unwrap();
    assert_eq!(result["saved"], true);
    assert_eq!(result["details"]["definition"], json!(proposed));
    assert_eq!(result["details"]["state"], "pending");
    let listing = catalog.management_listing(None).unwrap();
    let row = listing["experts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "project:reviewer")
        .unwrap();
    assert_eq!(row["state"], "pending");
    assert_eq!(row["displayName"], proposed.name);
    assert_eq!(result["details"]["activation"]["v"], 2);
    assert!(catalog.resolve("project:reviewer", None).is_err());
    assert!(catalog
        .apply_mutation(&request, change(&catalog, "stale-save"))
        .is_err());
    assert_eq!(
        catalog.candidate("project:reviewer").unwrap().definition,
        proposed
    );
}

#[test]
fn create_expects_absence_and_never_copies_an_activation() {
    let (_directory, catalog, original, _) = setup();
    let absent = catalog.inspect("project:copy", None).unwrap();
    assert_eq!(absent["state"], "absent");
    let mut copied = original.definition;
    copied.id = "copy".into();
    let request = put_arguments(&absent, &copied);
    let result = catalog
        .apply_mutation(&request, change(&catalog, "create"))
        .unwrap();
    assert_eq!(result["ready"], false);
    assert!(catalog
        .apply_mutation(&request, change(&catalog, "competing-create"))
        .is_err());
    assert_eq!(
        catalog.candidate("project:copy").unwrap().definition,
        copied
    );
    assert!(catalog
        .activations
        .read(&catalog.identity("project:copy").unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn deletion_and_identical_recreation_cannot_resurrect_the_old_activation() {
    let (_directory, catalog, original, active) = setup();
    let details = catalog.inspect("project:reviewer", None).unwrap();
    let mut delete = crate::experts::catalog::mutation_arguments(&details, "delete").unwrap();
    delete["reason"] = json!("Remove the reusable definition");
    assert_eq!(
        catalog
            .apply_mutation(&delete, change(&catalog, "delete"))
            .unwrap()["deleted"],
        true
    );
    let absent = catalog.inspect("project:reviewer", None).unwrap();
    assert_eq!(absent["activation"]["state"], "deleted");
    assert!(catalog.activations.commit(&active, None).is_err());
    let request = put_arguments(&absent, &original.definition);
    let result = catalog
        .apply_mutation(&request, change(&catalog, "recreate"))
        .unwrap();
    assert_eq!(result["details"]["state"], "pending");
    assert_ne!(result["details"]["activation"], details["activation"]);
    assert!(catalog.resolve("project:reviewer", None).is_err());
    assert!(catalog
        .apply_mutation(&delete, change(&catalog, "late-delete"))
        .is_err());
}

#[test]
fn interrupted_two_file_change_keeps_the_exact_partial_state_without_replay() {
    let (_directory, catalog, original, _) = setup();
    let details = catalog.inspect("project:reviewer", None).unwrap();
    let mut proposed = original.definition.clone();
    proposed.instructions.push_str(" New revision.");
    let error = catalog
        .apply_mutation_with(
            &put_arguments(&details, &proposed),
            change(&catalog, "interrupted-save"),
            || Err("injected storage failure".into()),
        )
        .unwrap_err();
    assert!(error.activation_changed);
    assert!(!error.definition_changed);
    assert_eq!(
        catalog.candidate("project:reviewer").unwrap().definition,
        original.definition
    );
    let reopened = Catalog::new(catalog.config.clone()).unwrap();
    assert_eq!(
        reopened.inspect("project:reviewer", None).unwrap()["state"],
        "pending"
    );
    assert_eq!(
        reopened.candidate("project:reviewer").unwrap().definition,
        original.definition
    );
    assert!(reopened.resolve("project:reviewer", None).is_err());
}

#[test]
fn an_external_change_after_state_commit_is_not_overwritten() {
    let (_directory, catalog, original, _) = setup();
    let details = catalog.inspect("project:reviewer", None).unwrap();
    let mut proposed = original.definition.clone();
    proposed.instructions.push_str(" Proposed.");
    let mut external = original.definition;
    external.instructions.push_str(" External.");
    let bytes = serde_json::to_vec(&external).unwrap();
    let path = catalog
        .definitions
        .path(Scope::Project, "reviewer")
        .unwrap();
    let error = catalog
        .apply_mutation_with(
            &put_arguments(&details, &proposed),
            change(&catalog, "race"),
            || {
                std::fs::write(&path, &bytes).unwrap();
                Ok(())
            },
        )
        .unwrap_err();
    assert!(error.activation_changed);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn malformed_existing_definitions_and_missing_expectations_are_not_empty_slots() {
    let (_directory, catalog, original, _) = setup();
    let details = catalog.inspect("project:reviewer", None).unwrap();
    let path = catalog
        .definitions
        .path(Scope::Project, "reviewer")
        .unwrap();
    std::fs::write(&path, b"broken JSON").unwrap();
    let mut request = put_arguments(&details, &original.definition);
    request["fileVersion"] = Value::Null;
    assert!(catalog
        .apply_mutation(&request, change(&catalog, "bad"))
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"broken JSON");
    request
        .as_object_mut()
        .unwrap()
        .remove("expectedActivation");
    assert!(Mutation::parse(&request).is_err());
}

#[test]
fn builtins_export_their_actual_instructions_and_capability_groups_for_copying() {
    let (_directory, catalog, _, _) = setup();
    for expert in crate::preset::EXPERTS {
        let details = catalog
            .inspect(&format!("builtin:{}", expert.name), None)
            .unwrap();
        let mut template = details["copyTemplate"].clone();
        assert_eq!(template["instructions"], expert.prompt);
        template["model"] = json!("configured-model");
        let definition: Definition = serde_json::from_value(template).unwrap();
        definition.validate().unwrap();
        assert_eq!(
            definition
                .tool_instances()
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            expert.tools.iter().copied().collect()
        );
    }
    assert!(catalog.identity("builtin:explorer").is_err());
}

#[test]
fn simultaneous_saves_have_one_winner_and_preserve_its_content_and_state() {
    let (_directory, catalog, original, _) = setup();
    let details = catalog.inspect("project:reviewer", None).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let writer = Catalog::new(catalog.config.clone()).unwrap();
            let mut proposed = original.definition.clone();
            proposed.instructions = name.into();
            let request = put_arguments(&details, &proposed);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (name, writer.apply_mutation(&request, change(&writer, name)))
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    let winner = results.iter().find(|(_, result)| result.is_ok()).unwrap().0;
    assert_eq!(
        catalog
            .candidate("project:reviewer")
            .unwrap()
            .definition
            .instructions,
        winner
    );
    let state = catalog
        .activations
        .state(&original.identity)
        .unwrap()
        .unwrap();
    let Record::Inactive(state) = state else {
        panic!("a saved revision must be pending")
    };
    assert_eq!(state.change.event, winner);
}
