use super::*;

#[test]
fn repairing_only_a_credential_preserves_every_other_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let original = json!({"unknown":{"keep":true},"models":{"one":{"model":"synthetic","apiKey":"old-fake-key","apiKeyEnv":"OLD","profile":{"custom":1},"note":"keep"},"broken":[1,2]}});
    std::fs::write(&path, original.to_string()).unwrap();
    let mut snapshot = Snapshot::read(&path).unwrap();
    snapshot.credential("one", "apiKeyEnv", "NEW").unwrap();
    snapshot.save().unwrap();
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut expected = original;
    expected["models"]["one"]
        .as_object_mut()
        .unwrap()
        .remove("apiKey");
    expected["models"]["one"]["apiKeyEnv"] = json!("NEW");
    assert_eq!(value, expected);
}

#[test]
fn stale_missing_or_existing_snapshots_never_overwrite_another_writer() {
    for exists in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        if exists {
            std::fs::write(&path, r#"{"models":{}}"#).unwrap();
        }
        let mut first = Snapshot::read(&path).unwrap();
        let mut stale = Snapshot::read(&path).unwrap();
        first.insert("first", json!({})).unwrap();
        stale.insert("stale", json!({})).unwrap();
        first.save().unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(stale.save().unwrap_err().contains("changed"));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[cfg(unix)]
#[test]
fn old_fixed_staging_name_is_never_touched_and_new_file_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    let old_stage = path.with_extension("json.writing");
    std::fs::write(&old_stage, "do not touch").unwrap();
    let mut snapshot = Snapshot::read(&path).unwrap();
    snapshot
        .insert("one", json!({"apiKey":"synthetic-secret"}))
        .unwrap();
    snapshot.save().unwrap();
    assert_eq!(std::fs::read_to_string(old_stage).unwrap(), "do not touch");
    verify_private(&std::fs::File::open(path).unwrap()).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[cfg(unix)]
#[test]
fn private_mode_is_verified_before_credentials_can_be_written() {
    use std::os::unix::fs::PermissionsExt;
    let file = tempfile::NamedTempFile::new().unwrap();
    verify_private(file.as_file()).unwrap();
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(verify_private(file.as_file()).is_err());
    assert!(std::fs::read(file.path()).unwrap().is_empty());
}

#[test]
fn malformed_json_diagnostics_do_not_echo_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.json");
    std::fs::write(&path, "{\"apiKey\": SECRET_SENTINEL}").unwrap();
    let Err(error) = Snapshot::read(&path) else {
        panic!("must reject malformed catalog")
    };
    assert!(!error.contains("SECRET_SENTINEL"));
}

#[test]
fn damaged_preferences_are_not_destroyed_while_saving_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preferences.json");
    for original in ["broken", "[]", "null"] {
        std::fs::write(&path, original).unwrap();
        assert!(crate::preferences::set_in(&path, "model", json!("one")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}
