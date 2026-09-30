use super::*;
use serde_json::json;

pub(super) fn definition() -> Definition {
    Definition::parse(
        &serde_json::to_vec(&json!({
            "v":1,"id":"reviewer","name":"Reviewer","description":"Reviews source",
            "instructions":"Read and report evidence.","model":"configured-model",
            "capabilities":["read"]
        }))
        .unwrap(),
    )
    .unwrap()
}

pub(super) fn fixture() -> (tempfile::TempDir, Definitions) {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("project");
    let home = directory.path().join("home");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&home).unwrap();
    let definitions = Definitions::new(&workspace, &home).unwrap();
    (directory, definitions)
}

pub(super) fn put(definitions: &Definitions, scope: Scope, bytes: &[u8]) {
    let path = definitions.path(scope, "reviewer").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn definitions_are_strict_data_not_executable_or_self_approving_configuration() {
    let good = serde_json::to_value(definition()).unwrap();
    for (field, value) in [
        ("approved", json!(true)),
        ("apiKey", json!("synthetic-key")),
        ("entry", json!("arbitrary-command")),
        ("wires", json!([])),
        ("scope", json!("personal")),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert!(
            Definition::parse(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "accepted {field}"
        );
    }
    for (field, value) in [
        ("v", json!(2)),
        ("instructions", json!(" ")),
        ("model", json!("")),
        ("capabilities", json!(["read", "read"])),
        ("capabilities", json!(["unknown"])),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert!(
            Definition::parse(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "accepted {field}"
        );
    }
}

#[test]
fn paths_are_scoped_and_identifiers_cannot_choose_paths() {
    let (_directory, definitions) = fixture();
    for id in [
        "",
        "../outside",
        "/tmp/x",
        "a/b",
        "a\\b",
        ".hidden",
        "A",
        "a:b",
    ] {
        assert!(
            definitions.path(Scope::Project, id).is_err(),
            "accepted {id}"
        );
    }
    assert!(definitions.path(Scope::Project, &"a".repeat(64)).is_ok());
    assert!(definitions.path(Scope::Project, &"a".repeat(65)).is_err());
    let project = definitions.identity(Scope::Project, "reviewer").unwrap();
    let personal = definitions.identity(Scope::Personal, "reviewer").unwrap();
    assert_eq!(project.name(), "project:reviewer");
    assert_eq!(personal.name(), "personal:reviewer");
    assert_ne!(project, personal);
}

#[test]
fn formatting_preserves_semantic_revision_but_changes_the_file_conflict_token() {
    let (_directory, definitions) = fixture();
    let original = definition();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&original).unwrap(),
    );
    let first = definitions.read(Scope::Project, "reviewer").unwrap();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec_pretty(&original).unwrap(),
    );
    let second = definitions.read(Scope::Project, "reviewer").unwrap();
    assert_eq!(first.definition.revision(), second.definition.revision());
    assert_ne!(first.file_version, second.file_version);
    let mut changed = original.clone();
    changed.instructions.push_str(" Then write changes.");
    assert_ne!(original.revision(), changed.revision());
    changed = original.clone();
    changed.model = "another-model".into();
    assert_ne!(original.revision(), changed.revision());
    changed = original.clone();
    changed.capabilities.push(Capability::Write);
    assert_ne!(original.revision(), changed.revision());
}

#[test]
fn broken_or_misnamed_definitions_are_not_empty_definitions() {
    let (_directory, definitions) = fixture();
    for bytes in [b"{".as_slice(), b"null".as_slice()] {
        put(&definitions, Scope::Project, bytes);
        assert!(definitions.read(Scope::Project, "reviewer").is_err());
        assert_eq!(
            std::fs::read(definitions.path(Scope::Project, "reviewer").unwrap()).unwrap(),
            bytes
        );
    }
    let mut wrong = definition();
    wrong.id = "different".into();
    put(
        &definitions,
        Scope::Project,
        &serde_json::to_vec(&wrong).unwrap(),
    );
    assert!(definitions.read(Scope::Project, "reviewer").is_err());
}

#[test]
fn definition_size_limit_is_checked_without_truncating_or_rewriting_the_file() {
    let (_directory, definitions) = fixture();
    let bytes = vec![b' '; 1024 * 1024 + 1];
    put(&definitions, Scope::Project, &bytes);
    assert!(definitions
        .read(Scope::Project, "reviewer")
        .unwrap_err()
        .contains("exceeds"));
    assert_eq!(
        std::fs::metadata(definitions.path(Scope::Project, "reviewer").unwrap())
            .unwrap()
            .len(),
        bytes.len() as u64
    );
}

#[test]
fn capability_groups_expand_to_real_separate_provider_instances() {
    let mut custom = definition();
    assert_eq!(custom.tool_instances(), ["fs", "search"]);
    custom.capabilities = vec![
        Capability::Write,
        Capability::Web,
        Capability::Commands,
        Capability::SkillInstall,
    ];
    assert_eq!(
        custom.tool_instances(),
        ["fs-write", "net", "search-web", "shell", "skill-installer"]
    );
}
