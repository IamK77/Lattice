//! The recovery executable path must remain independent of the normal assembly.
use std::process::Command;

#[test]
fn damaged_ledger_is_read_without_credentials_configuration_or_repairs() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let ledger = directory.path().join("damaged.jsonl");
    let original = b"\x1b[2J{broken}\n{\"unfinished\":\"\xf0\x9f";
    std::fs::write(&ledger, original).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lattice"))
        .env("HOME", &home)
        .env(
            "LATTICE_ASSEMBLY",
            directory.path().join("does-not-exist.json"),
        )
        .env("LATTICE_ADAPTER", "recovery-must-not-construct-an-adapter")
        .env("LATTICE_API_KEY_ENV", "RECOVERY_TEST_MISSING_KEY")
        .env_remove("RECOVERY_TEST_MISSING_KEY")
        .args([
            "--recover",
            ledger.to_str().unwrap(),
            "--offset",
            "0",
            "--bytes",
            "8",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.contains(&0x1b),
        "file controls must not execute in the terminal"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["mode"], "read-only recovery");
    assert_eq!(report["window"]["nextOffset"], 8);
    assert_eq!(report["window"]["observedFileBytes"], original.len());
    assert_eq!(std::fs::read(&ledger).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "no normal runtime state should be created"
    );
    assert_eq!(
        std::fs::read_dir(directory.path()).unwrap().count(),
        2,
        "no ledger sidecars should be created"
    );
}
