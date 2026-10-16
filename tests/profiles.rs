//! Model profiles are pure data — schema validation IS their inspection.
//! Every reference profile shipped in profiles/ must pass the canon schema,
//! so a drifting schema or a sloppy profile reddens here, not in a user's
//! assembly.

use serde_json::{json, Value};

#[test]
fn profile_and_catalog_schemas_accept_only_string_supplements() {
    for (schema, catalog) in [
        (include_str!("../schemas/model_profile.json"), false),
        (include_str!("../schemas/model_catalog.json"), true),
    ] {
        let schema: Value = serde_json::from_str(schema).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for (field, valid) in [
            (None, true),
            (Some(json!("")), true),
            (Some(json!(" \nSYNTHETIC {model}\n ")), true),
            (Some(json!(null)), false),
            (Some(json!(false)), false),
            (Some(json!(17)), false),
            (Some(json!([])), false),
            (Some(json!({})), false),
        ] {
            let mut profile = if catalog {
                json!({})
            } else {
                json!({"model":"synthetic"})
            };
            if let Some(field) = field {
                profile["systemPromptSupplement"] = field;
            }
            let document = if catalog {
                json!({"models":{"synthetic-entry":{"adapter":"openai", "model":"synthetic",
                    "baseUrl":"http://127.0.0.1:1", "apiKeyEnv":"SYNTHETIC_KEY", "profile":profile}}})
            } else {
                profile
            };
            assert_eq!(
                validator.is_valid(&document),
                valid,
                "catalog={catalog}: {document}"
            );
        }
    }
}

#[test]
fn every_reference_profile_passes_the_canon_schema() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/model_profile.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let profile: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            profile.get("systemPromptSupplement").is_none(),
            "{} must not ship authored model-specific prompt text",
            path.display()
        );
        let problems: Vec<String> = validator
            .iter_errors(&profile)
            .map(|e| e.to_string())
            .collect();
        assert!(
            problems.is_empty(),
            "{} fails its inspection: {problems:?}",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "the reference profiles must exist and be checked"
    );
}
