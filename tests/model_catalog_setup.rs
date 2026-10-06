use lattice::models;
use serde_json::json;

#[test]
fn setup_never_replaces_a_malformed_models_member() {
    for original in [r#"{"models":[],"keep":"untouched"}"#, "[]", "null"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        std::fs::write(&path, original).unwrap();
        assert!(models::add_to(&path, "new", json!({"model":"synthetic"})).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}
