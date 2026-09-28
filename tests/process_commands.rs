//! Offline process boundaries for commands that do not own a terminal UI.
#![cfg(unix)]

#[path = "process_commands/capture.rs"]
mod capture;
#[path = "process_commands/daemon.rs"]
mod daemon;
#[path = "process_commands/inspection.rs"]
mod inspection;
#[path = "process_commands/ledger.rs"]
mod ledger;

use std::path::Path;
use std::process::{Command, Output};

fn command(home: &Path) -> Command {
    let temporary = home.join("tmp");
    std::fs::create_dir_all(&temporary).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_lattice"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin")
        .env("HOME", home)
        .env("TMPDIR", temporary)
        .env("LATTICE_OVERLAY", "")
        .current_dir(home);
    command
}

fn succeeded(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "process failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).unwrap()
}
