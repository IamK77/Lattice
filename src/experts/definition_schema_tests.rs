use super::*;
use serde_json::json;

#[test]
fn definition_schema_and_runtime_agree_on_supported_data() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/expert_definition.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let original = serde_json::to_value(super::tests::definition()).unwrap();
    assert!(validator.is_valid(&original));
    for (field, value) in [
        ("approved", json!(true)),
        ("apiKey", json!("not-allowed")),
        ("v", json!(2)),
        ("id", json!("../outside")),
        ("instructions", json!(" \n\t")),
        ("capabilities", json!(["read", "read"])),
        ("capabilities", json!(["unknown"])),
    ] {
        let mut bad = original.clone();
        bad[field] = value;
        assert!(!validator.is_valid(&bad), "schema accepted {field}");
        assert!(
            Definition::parse(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "runtime accepted {field}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_definition_is_not_followed() {
    let (directory, definitions) = super::tests::fixture();
    let external = directory.path().join("external.json");
    std::fs::write(
        &external,
        serde_json::to_vec(&super::tests::definition()).unwrap(),
    )
    .unwrap();
    let path = definitions.path(Scope::Project, "reviewer").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(external, path).unwrap();
    assert!(definitions.read(Scope::Project, "reviewer").is_err());
}
