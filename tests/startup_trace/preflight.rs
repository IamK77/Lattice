//! Failure order at the real process boundary, before any terminal takeover.
//! Every launch has a private home and an empty environment; no model is called.

use std::path::Path;
use std::process::{Command, Output};

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lattice"));
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("LATTICE_OVERLAY", "")
        .current_dir(home);
    command
}

fn broken_catalog(home: &Path) {
    std::fs::create_dir_all(home.join(".lattice")).unwrap();
    std::fs::write(home.join(".lattice/models.json"), "not valid JSON").unwrap();
}

fn refusal(output: Output) -> String {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        !error.contains('\u{1b}'),
        "preflight must not enter terminal modes"
    );
    error
}

#[test]
fn missing_configuration_never_creates_a_workspace_or_ledger() {
    for args in [vec![], vec!["serve"]] {
        let home = tempfile::tempdir().unwrap();
        broken_catalog(home.path());
        let workspace = home.path().join("new/workspace");
        let error = refusal(
            command(home.path())
                .args(args)
                .env("LATTICE_ADAPTER", "openai")
                .env("LATTICE_MODEL", "fixture")
                .env("LATTICE_API_KEY_ENV", "LATTICE_TEST_UNSET_KEY")
                .env("LATTICE_WORKSPACE", &workspace)
                .output()
                .unwrap(),
        );
        assert!(
            error.contains("no model this installation can reach")
                || error.contains("model configuration is incomplete"),
            "{error}"
        );
        assert!(!error.contains("warning:"), "{error}");
        assert!(!error.contains("no conversation called"), "{error}");
        assert!(
            !workspace.exists(),
            "key refusal must precede workspace creation"
        );
        assert!(!home.path().join(".lattice/ledgers").exists());
        assert!(!home.path().join(".lattice/daemon.sock").exists());
    }
}

#[test]
fn catalog_warning_precedes_workspace_failure_in_both_hosts() {
    for args in [vec![], vec!["serve"]] {
        let home = tempfile::tempdir().unwrap();
        broken_catalog(home.path());
        let workspace = home.path().join("not-a-directory");
        std::fs::write(&workspace, "keep").unwrap();
        let error = refusal(
            command(home.path())
                .args(args)
                // Unlike LATTICE_SCRIPTED, this still reads the model catalog.
                .env("LATTICE_ADAPTER", "scripted")
                .env("LATTICE_WORKSPACE", &workspace)
                .output()
                .unwrap(),
        );
        assert!(error.starts_with("warning:"), "{error}");
        assert!(error.contains("Error:"), "{error}");
        assert!(!error.contains("no conversation called"), "{error}");
        assert_eq!(std::fs::read_to_string(workspace).unwrap(), "keep");
        assert!(!home.path().join(".lattice/ledgers").exists());
        assert!(!home.path().join(".lattice/daemon.sock").exists());
    }
}

#[test]
fn missing_conversation_is_reported_before_setup_or_workspace_creation() {
    let home = tempfile::tempdir().unwrap();
    broken_catalog(home.path());
    let workspace = home.path().join("new/workspace");
    let error = refusal(
        command(home.path())
            .args(["--resume", "missing"])
            .env("LATTICE_ADAPTER", "scripted")
            .env("LATTICE_WORKSPACE", &workspace)
            .output()
            .unwrap(),
    );
    assert!(!error.contains("warning:"), "{error}");
    assert!(error.contains("no conversation called"), "{error}");
    assert!(
        !workspace.exists(),
        "invalid history selection must not prepare a workspace"
    );
    assert!(!home.path().join(".lattice/ledgers").exists());
}

#[test]
fn an_empty_key_is_not_a_usable_credential_in_either_host() {
    for args in [vec![], vec!["serve"]] {
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("workspace");
        let error = refusal(
            command(home.path())
                .args(args)
                .env("LATTICE_ADAPTER", "openai")
                .env("LATTICE_MODEL", "fixture")
                .env("LATTICE_API_KEY_ENV", "LATTICE_TEST_EMPTY_KEY")
                .env("LATTICE_TEST_EMPTY_KEY", "")
                .env("LATTICE_WORKSPACE", &workspace)
                .output()
                .unwrap(),
        );
        assert!(
            error.contains("no model this installation can reach")
                || error.contains("model configuration is incomplete"),
            "{error}"
        );
        assert!(!workspace.exists());
        assert!(!home.path().join(".lattice/ledgers").exists());
    }
}

#[test]
fn prompt_inspection_reports_catalog_problems_without_launch_preparation() {
    let home = tempfile::tempdir().unwrap();
    broken_catalog(home.path());
    let workspace = home.path().join("not-created");
    let output = command(home.path())
        .arg("prompt")
        .env("LATTICE_ADAPTER", "scripted")
        .env("LATTICE_WORKSPACE", &workspace)
        .output()
        .unwrap();
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{error}");
    assert!(error.starts_with("warning:"), "{error}");
    assert!(!output.stdout.is_empty());
    assert!(
        !workspace.exists(),
        "prompt inspection must not prepare the workspace"
    );
    assert!(!home.path().join(".lattice/ledgers").exists());
}
