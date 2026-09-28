//! Model profiles are pure data — schema validation IS their inspection.
//! Every reference profile shipped in profiles/ must pass the canon schema,
//! so a drifting schema or a sloppy profile reddens here, not in a user's
//! assembly.

use serde_json::Value;

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
