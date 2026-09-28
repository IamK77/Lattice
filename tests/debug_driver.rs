//! Process-level compatibility of the headless terminal drivers. Every process
//! gets a private HOME, temporary directory and workspace, with no credentials.
#[path = "process_commands/capture.rs"]
mod capture;

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        for name in ["home", "tmp", "work"] {
            std::fs::create_dir(fixture.path(name)).unwrap();
        }
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
    fn run(&self, args: &[&str], input: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lattice"));
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("tmp"))
            .current_dir(self.path("work"));
        capture::output(command, input.as_bytes(), Duration::from_secs(60)).unwrap()
    }
    fn success(&self, args: &[&str], input: &str) -> Output {
        let output = self.run(args, input);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}
fn frames(output: &Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn rows(frame: &Value) -> String {
    frame["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn scripted_actions_use_real_modal_edit_submit_and_reopen_paths() {
    let fixture = Fixture::new();
    let brain = fixture.path("brain.json");
    std::fs::write(
        &brain,
        json!({"script":[{"status":"ok","text":"UNIQUE-OFFLINE-REPLY"}]}).to_string(),
    )
    .unwrap();
    let output=fixture.success(&["debug-tui","--json","--brain",brain.to_str().unwrap()],
        "type /model\nkey enter\ntype MUST-NOT-LEAK\nframe\nkey esc\npaste alpha\\nbeta\nframe\nresize 36x14\nframe\nkey ctrl-c\ntype fixture-question\nkey enter\nwait\nframe\n");
    let shots = frames(&output);
    assert_eq!(shots.len(), 4);
    assert!(!rows(&shots[0]).contains("MUST-NOT-LEAK"));
    assert!(
        !rows(&shots[1]).contains("MUST-NOT-LEAK"),
        "closing the modal must reveal an untouched draft"
    );
    assert!(rows(&shots[1]).contains("alpha"));
    assert!(rows(&shots[1]).contains("beta"));
    assert_eq!(
        (shots[2]["width"].as_u64(), shots[2]["height"].as_u64()),
        (Some(36), Some(14))
    );
    assert!(rows(&shots[3]).contains("UNIQUE-OFFLINE-REPLY"));
    let ledger = fixture.path("tmp/lattice-debug-tui/stream.jsonl");
    let events: Vec<Value> = std::fs::read_to_string(&ledger)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let messages: Vec<_> = events
        .iter()
        .filter(|e| {
            e["type"] == lattice::core_events::USER_MESSAGE
                && e["causes"].as_array().unwrap().is_empty()
        })
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["payload"]["text"], "fixture-question");
    let reopened = fixture.success(&["debug-tui", "--continue", "--json"], "frame\n");
    assert!(rows(&frames(&reopened)[0]).contains("UNIQUE-OFFLINE-REPLY"));
    let snapshot = fixture.success(
        &[
            "debug-frame",
            ledger.to_str().unwrap(),
            "--width",
            "60",
            "--height",
            "24",
        ],
        "",
    );
    let snapshot: Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    assert_eq!(snapshot["width"], 60);
    assert_eq!(snapshot["height"], 24);
    assert!(rows(&snapshot).contains("UNIQUE-OFFLINE-REPLY"));
    let empty = fixture.success(&["debug-frame", ledger.to_str().unwrap(), "--at", "0"], "");
    assert!(!rows(&serde_json::from_slice(&empty.stdout).unwrap()).contains("fixture-question"));
    let html = fixture.path("saved.html");
    let output = fixture.success(
        &[
            "debug-frame",
            ledger.to_str().unwrap(),
            "--html",
            html.to_str().unwrap(),
        ],
        "",
    );
    assert!(output.stdout.is_empty());
    assert!(std::fs::read_to_string(html)
        .unwrap()
        .contains("UNIQUE-OFFLINE-REPLY"));
}

#[test]
fn image_override_and_html_precedence_remain_debug_only() {
    let fixture = Fixture::new();
    let image = fixture.path("image.png");
    // The attachment path recognizes the same PNG signature as its unit fixture.
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    let actions = format!("paste {}\nframe\n", image.display());
    let without = fixture.success(&["debug-tui", "--json"], &actions);
    assert!(!rows(&frames(&without)[0]).contains("image.png"));
    let with = fixture.success(&["debug-tui", "--json", "--images"], &actions);
    assert!(rows(&frames(&with)[0]).contains("image.png"));
    let prefix = fixture.path("shot");
    let html = fixture.success(
        &["debug-tui", "--json", "--html", prefix.to_str().unwrap()],
        "type HTML-MARKER\nframe\nframe\n",
    );
    assert!(html.stdout.is_empty());
    for i in 1..=2 {
        assert!(
            std::fs::read_to_string(fixture.path(&format!("shot-{i}.html")))
                .unwrap()
                .contains("HTML-MARKER")
        );
    }
    assert!(!fixture.path("home/.lattice/installed.json").exists());
}

#[test]
fn invalid_debug_commands_keep_their_diagnostics_and_exit_codes() {
    let fixture = Fixture::new();
    for (args, input, problem) in [
        (vec!["debug-frame"], "", "usage: lattice debug-frame"),
        (
            vec!["debug-tui"],
            "key wiggle\n",
            "line 1: unknown key 'wiggle'",
        ),
        (
            vec!["debug-tui"],
            "wiggle\n",
            "line 1: unknown action 'wiggle'",
        ),
    ] {
        let output = fixture.run(&args, input);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains(problem));
    }
}
