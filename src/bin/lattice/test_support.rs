//! Process isolation for tests that read launch configuration or construct hosts.
//! Changing HOME in a parallel test process can redirect somebody else's writes.

pub(crate) fn isolated(test: impl FnOnce()) {
    let name = std::thread::current()
        .name()
        .expect("called from a named test thread")
        .to_string();
    if std::env::var("LATTICE_PRIVATE_TEST").as_deref() == Ok(name.as_str()) {
        test();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    let temporary = root.path().join("tmp");
    for path in [&home, &workspace, &temporary] {
        std::fs::create_dir_all(path).unwrap();
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("TMPDIR", &temporary)
        .env("LATTICE_PRIVATE_TEST", &name)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated {name} failed ({status}):\n{stdout}\n{stderr}",
        status = output.status,
        stdout = String::from_utf8_lossy(&output.stdout),
        stderr = String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "the child must execute its test, not merely exit successfully"
    );
}
