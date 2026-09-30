//! Real terminal coordinator, real management and authorization, no network or user files.
use serde_json::Value;
use std::process::Command;

#[test]
fn panel_copy_save_activate_and_delete_use_real_authorization_without_model_turns() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let temporary = directory.path().join("tmp");
    std::fs::create_dir_all(home.join(".lattice")).unwrap();
    std::fs::create_dir_all(&temporary).unwrap();
    let models = home.join(".lattice/models.json");
    std::fs::write(&models, r#"{"models":{"preview":{"adapter":"scripted","model":"preview","baseUrl":"https://unused.invalid","apiKeyEnv":"UNUSED_PREVIEW_KEY"}}}"#).unwrap();
    let script = directory.path().join("actions");
    let actions = format!("{}\nkey d\nwait\nframe\nkey esc\nwait\nframe\nkey d\nwait\nkey up\nkey enter\nwait\nframe\n", include_str!("fixtures/expert-panel.actions"));
    std::fs::write(&script, actions).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lattice"))
        .args(["debug-tui", "--width", "100", "--height", "40"])
        .arg(&script)
        .env("HOME", &home)
        .env("TMPDIR", &temporary)
        .env("LATTICE_MODELS", &models)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let screen = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "State: pending",
        "State: ready",
        "Confirm expert deletion",
        "user refused expert deletion",
        "Deleted.",
    ] {
        assert!(screen.contains(expected), "missing {expected}: {screen}");
    }
    let workspace = temporary.join("lattice-debug-tui");
    assert!(!workspace
        .join(".lattice/expert-definitions/review-copy.json")
        .exists());
    let events: Vec<Value> = std::fs::read_to_string(workspace.join("stream.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!events
        .iter()
        .any(|event| event["type"] == lattice::core_events::MODEL_CALL_STARTED));
    assert_eq!(events.iter().filter(|event| event["type"] == lattice::components::expert_definitions::AUTH_REQUESTED).count(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == lattice::components::trust_policy::AUTH_REQUESTED)
            .count(),
        2
    );
    let state_files: Vec<_> = std::fs::read_dir(home.join(".lattice/expert-activations"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    assert_eq!(state_files.len(), 1);
    let state: Value =
        serde_json::from_slice(&std::fs::read(state_files[0].path()).unwrap()).unwrap();
    assert_eq!(state["state"], "deleted");
}
