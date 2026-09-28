//! Process-level dispatch checks that do not create a session or read a catalog.
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_lattice"))
        .args(args)
        .env("HOME", home.path())
        .current_dir(home.path())
        .output()
        .unwrap()
}

#[test]
fn version_help_and_unknown_commands_keep_their_process_results() {
    for flag in ["--version", "-V", "version"] {
        let output = run(&[flag, "ignored"]);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n", lattice::VERSION)
        );
    }
    let help = run(&["--help"]);
    assert!(help.status.success());
    assert!(help.stdout.is_empty());
    let help_text = String::from_utf8(help.stderr).unwrap();
    assert!(help_text.contains("USAGE:"));
    for flag in ["-h", "help"] {
        let output = run(&[flag, "ignored"]);
        assert!(output.status.success());
        assert_eq!(output.stdout, help.stdout);
        assert_eq!(String::from_utf8(output.stderr).unwrap(), help_text);
    }
    for command in ["not-a-command", "-r", "recover", "run", "assemble"] {
        let output = run(&[command]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stdout, help.stdout);
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("lattice: unknown command '{command}'\n\n{help_text}")
        );
    }
}

#[test]
fn maintenance_arity_errors_do_not_enter_the_filesystem_operation() {
    for (flag, usage) in [
        (
            "--migrate-ledger",
            "usage: lattice --migrate-ledger OFFLINE_LEGACY.jsonl",
        ),
        (
            "--verify-ledger",
            "usage: lattice --verify-ledger SEGMENTED_LEDGER_DIRECTORY",
        ),
    ] {
        for args in [vec![flag], vec![flag, "one", "two"]] {
            let output = run(&args);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains("InvalidInput"), "{error}");
            assert!(error.contains(usage), "{error}");
        }
    }
}
