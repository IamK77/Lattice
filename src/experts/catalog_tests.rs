use super::activation::{Activation, ACTIVATE};
use super::catalog::{Catalog, Config};
use super::execution::Execution;
use super::tests::{definition, fixture, put};
use super::*;
use crate::preset::PresetConfig;
use serde_json::json;

pub(super) fn setup() -> (tempfile::TempDir, Catalog, Candidate, Activation) {
    let (directory, definitions) = fixture();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&definition()).unwrap(),
    );
    let candidate = definitions.read(Scope::Project, "reviewer").unwrap();
    let ledger = directory.path().join("approval.jsonl");
    let event: crate::EventEnvelope = serde_json::from_value(json!({
        "v":1,"id":"approved","seq":1,"stream":"synthetic","time":"synthetic",
        "type":crate::core_events::TOOL_EXEC_STARTED,"source":"trust","causes":[],
        "payload":{"tool":ACTIVATE,"call":"activate","arguments":{"operation":"activate",
            "target":candidate.identity,"definition":candidate.definition,"fileVersion":candidate.file_version}}
    })).unwrap();
    std::fs::write(
        &ledger,
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();
    let activation = Activation::approved(&candidate, &event, &ledger, "trust").unwrap();
    let model_catalog = directory.path().join("models.json");
    std::fs::write(&model_catalog, serde_json::to_vec(&json!({"models":{"configured-model":{
        "adapter":"scripted","model":"provider-A","baseUrl":"https://unused.invalid","apiKeyEnv":"UNUSED_SCRIPTED_KEY"
    }}})).unwrap()).unwrap();
    let defaults = PresetConfig {
        adapter: "scripted".into(),
        model: "default".into(),
        base_url: String::new(),
        key_env: String::new(),
        workspace: Some(directory.path().join("project").to_str().unwrap().into()),
        context_window: 64000,
        usage_input_field: "input_tokens".into(),
        profile: None,
        catalog_problems: vec![],
        system: String::new(),
        thinking: None,
        scripted: Some(json!({"script":[]})),
        overlay: None,
        assembly: None,
    };
    let catalog = Catalog::new(Config {
        workspace: directory.path().join("project"),
        home: directory.path().join("home"),
        model_catalog,
        gate: "trust".into(),
        defaults,
    })
    .unwrap();
    catalog.activations.commit(&activation, None).unwrap();
    (directory, catalog, candidate, activation)
}

#[test]
fn inspection_readiness_describes_the_same_candidate_even_after_the_file_changes() {
    let (_directory, catalog, original, activation) = setup();
    let mut changed = original.definition.clone();
    changed.instructions = "Unapproved B".into();
    put(
        &catalog.definitions,
        Scope::Project,
        &serde_json::to_vec(&changed).unwrap(),
    );
    let observed_b = catalog.candidate("project:reviewer").unwrap();
    put(
        &catalog.definitions,
        Scope::Project,
        &serde_json::to_vec(&original.definition).unwrap(),
    );
    assert!(catalog.resolve("project:reviewer", None).is_ok());
    let inspection = catalog.inspect_candidate(&observed_b, Some(&activation), None);
    assert_eq!(inspection["definition"]["instructions"], "Unapproved B");
    assert_eq!(inspection["ready"], false);
    assert_eq!(inspection["fileVersion"], observed_b.file_version);
}

#[test]
fn captured_catalog_identity_and_provider_configuration_are_distinct_and_frozen() {
    let (_directory, catalog, candidate, activation) = setup();
    // PATH is a non-secret fixture value. No provider is called by capture.
    let mut model = crate::models::Entry {
        id: candidate.definition.model.clone(),
        adapter: "openai".into(),
        model: "provider-model-A".into(),
        base_url: "https://endpoint-a.invalid".into(),
        key_env: "PATH".into(),
        profile: None,
    };
    assert!(model.key_present());
    let snapshot =
        Execution::capture(&candidate, &activation, &model, &catalog.config.defaults).unwrap();
    let main = snapshot.assembly.instances["model"]
        .config
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.model_id, model.id);
    assert_eq!(main["entryId"], model.id);
    assert_eq!(main["model"], "provider-model-A");
    assert_eq!(main["baseUrl"], "https://endpoint-a.invalid");
    assert_eq!(main["apiKeyEnv"], "PATH");
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/expert_execution.json")).unwrap();
    let assembly_schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/assembly_manifest.json")).unwrap();
    let component_schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/component_manifest.json")).unwrap();
    let registry = jsonschema::Registry::new()
        .add(
            "https://lattice.invalid/schemas/assembly_manifest.json",
            assembly_schema,
        )
        .unwrap()
        .add(
            "https://lattice.invalid/schemas/component_manifest.json",
            component_schema,
        )
        .unwrap()
        .prepare()
        .unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .with_base_uri("https://lattice.invalid/schemas/expert_execution.json")
        .build(&schema)
        .unwrap();
    let mut encoded = serde_json::to_value(&snapshot).unwrap();
    assert!(validator.is_valid(&encoded));
    encoded["v"] = json!(2);
    assert!(!validator.is_valid(&encoded));
    model.model = "provider-model-B".into();
    model.base_url = "https://endpoint-b.invalid".into();
    std::fs::remove_file(&catalog.config.model_catalog).unwrap();
    let transported: Execution =
        serde_json::from_value(serde_json::to_value(snapshot).unwrap()).unwrap();
    let template = transported.template().unwrap();
    let frozen = template.assembly.instances["model"]
        .config
        .as_ref()
        .unwrap();
    assert_eq!(frozen["model"], "provider-model-A");
    assert_eq!(frozen["baseUrl"], "https://endpoint-a.invalid");
    assert_eq!(
        template.assembly.instances["cmodel"]
            .config
            .as_ref()
            .unwrap()["model"],
        "provider-model-A"
    );
    assert!(!serde_json::to_string(&transported)
        .unwrap()
        .contains(&std::env::var("PATH").unwrap()));
}

#[test]
fn missing_or_broken_authorization_evidence_never_becomes_ready() {
    let (_directory, catalog, candidate, activation) = setup();
    std::fs::write(&activation.authorization.ledger, "broken").unwrap();
    assert!(catalog.resolve("project:reviewer", None).is_err());
    std::fs::remove_file(&activation.authorization.ledger).unwrap();
    assert!(catalog.resolve("project:reviewer", None).is_err());
    assert_eq!(
        catalog
            .activations
            .read(&candidate.identity)
            .unwrap()
            .unwrap(),
        activation
    );
}

#[test]
fn short_names_never_silently_override_a_builtin_or_another_scope() {
    let (_directory, catalog, candidate, _activation) = setup();
    assert_eq!(
        catalog.qualify("reviewer", false).unwrap(),
        "project:reviewer"
    );
    assert!(catalog.qualify("reviewer", true).is_err());
    put(
        &catalog.definitions,
        Scope::Personal,
        &serde_json::to_vec(&candidate.definition).unwrap(),
    );
    assert!(catalog.qualify("reviewer", false).is_err());
    assert_eq!(
        catalog.qualify("personal:reviewer", true).unwrap(),
        "personal:reviewer"
    );
    assert!(catalog.candidate("builtin:reviewer").is_err());
}

#[test]
fn inspected_content_is_returned_once_and_preserves_all_mutation_preconditions() {
    let (_directory, catalog, candidate, _) = setup();
    let mut definition = candidate.definition.clone();
    definition.instructions = "Review each changed boundary carefully. ".repeat(256);
    std::fs::write(
        catalog
            .definitions
            .path(candidate.identity.scope, &candidate.identity.id)
            .unwrap(),
        serde_json::to_vec(&definition).unwrap(),
    )
    .unwrap();
    let details = catalog.inspect("project:reviewer", None).unwrap();
    let encoded = serde_json::to_string(&details).unwrap();
    assert_eq!(encoded.matches(&definition.instructions).count(), 1);
    assert_eq!(
        details["toolRoot"],
        json!(catalog.config.defaults.workspace)
    );
    let mut old = details.clone();
    for (operation, field) in [
        ("put", "putArguments"),
        ("activate", "activateArguments"),
        ("delete", "deleteArguments"),
    ] {
        let args = super::catalog::mutation_arguments(&details, operation).unwrap();
        assert_eq!(args["target"], details["target"]);
        assert_eq!(args["fileVersion"], details["fileVersion"]);
        assert_eq!(args["expectedActivation"], details["activation"]);
        assert_eq!(
            args.get("definition"),
            if operation == "delete" {
                None
            } else {
                Some(&details["definition"])
            }
        );
        assert!(details.get(field).is_none());
        old[field] = args;
    }
    let old_len = serde_json::to_vec(&old).unwrap().len();
    println!(
        "Inspection bytes: compact={}, repeated={old_len}",
        encoded.len()
    );
    assert!(
        encoded.len() * 2 < old_len,
        "large definitions must not be duplicated in action templates"
    );
    let builtin = catalog.inspect("builtin:explorer", None).unwrap();
    assert!(super::catalog::mutation_arguments(&builtin, "delete").is_err());
    let mut incomplete = details.clone();
    incomplete.as_object_mut().unwrap().remove("activation");
    assert!(super::catalog::mutation_arguments(&incomplete, "put").is_err());
}

#[test]
fn personal_storage_does_not_change_the_reported_file_tool_root() {
    let (_directory, mut catalog, candidate, _) = setup();
    put(
        &catalog.definitions,
        Scope::Personal,
        &serde_json::to_vec(&candidate.definition).unwrap(),
    );
    let details = catalog.inspect("personal:reviewer", None).unwrap();
    assert_eq!(
        details["target"]["root"],
        json!(catalog.config.home.canonicalize().unwrap())
    );
    assert_eq!(
        details["toolRoot"],
        json!(catalog.config.defaults.workspace)
    );
    assert_ne!(details["target"]["root"], details["toolRoot"]);
    catalog.config.defaults.workspace = None;
    assert!(catalog.inspect("personal:reviewer", None).unwrap()["toolRoot"].is_null());
    assert!(catalog.management_listing(None).unwrap()["toolRoot"].is_null());
    let absent = catalog.inspect("personal:missing", None).unwrap();
    assert!(super::catalog::mutation_arguments(&absent, "put").is_ok());
    assert!(super::catalog::mutation_arguments(&absent, "activate").is_err());
    assert!(super::catalog::mutation_arguments(&absent, "delete").is_err());
}

#[test]
fn invalid_activation_is_rejected_before_a_human_question_is_needed() {
    let (_directory, catalog, _candidate, _activation) = setup();
    let mut args = super::catalog::mutation_arguments(
        &catalog.inspect("project:reviewer", None).unwrap(),
        "activate",
    )
    .unwrap();
    args["reason"] = json!("Confirm this version");
    assert!(catalog
        .review(&args)
        .unwrap()
        .contains("Read and report evidence."));
    args["definition"]["instructions"] = json!("Replaced after inspection");
    assert!(catalog.review(&args).is_err());
}
