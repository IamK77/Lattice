//! Explicit native acceptance: no model endpoint, existing window, or network is used.
#![cfg(target_os = "macos")]

use lattice::components::desktop_cua::CuaDesktop;
use lattice::components::desktop_driver::{Action, Button, DesktopDriver};
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

struct OwnedApp(Child);
impl Drop for OwnedApp {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "Requires a graphical macOS session, installed desktop driver, and host Screen Recording/Accessibility permissions"]
fn owned_native_window_receives_literal_input_and_returns_pixels() {
    let cancel = CancellationToken::new();
    let mut driver = CuaDesktop::from_config(Some(&json!({})));
    driver
        .check_permissions(true, &cancel)
        .unwrap_or_else(|e| panic!("native prerequisites: {}", e.message));
    let root = tempfile::tempdir().unwrap();
    let contents = root.path().join("LatticeDesktopFixture.app/Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::write(contents.join("Info.plist"), r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>LatticeDesktopFixture</string><key>CFBundleIdentifier</key><string>org.lattice.desktop-fixture</string><key>CFBundleName</key><string>LatticeDesktopFixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
    let executable = contents.join("MacOS/LatticeDesktopFixture");
    let build = Command::new("/usr/bin/swiftc")
        .args(["-framework", "AppKit"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/desktop_fixture.swift"
        ))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let child = Command::new(executable)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut app = OwnedApp(child);
    let output = app.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(15)).unwrap(),
        "READY"
    );
    let targets = driver
        .targets(&cancel)
        .unwrap_or_else(|e| panic!("native target discovery: {}", e.message));
    let target = targets
        .iter()
        .find(|t| {
            t.title == "Lattice Desktop Verification" && t.application == "LatticeDesktopFixture"
        })
        .expect("owned fixture window must be discoverable");
    let before = driver
        .observe(&target.id, &cancel)
        .unwrap_or_else(|e| panic!("native capture: {}", e.message));
    driver
        .act(
            &target.id,
            &Action::Click {
                x: before.width / 2,
                y: before.height / 2,
                button: Button::Left,
                count: 1,
            },
            &cancel,
        )
        .unwrap_or_else(|e| panic!("native click: {}", e.message));
    let text = "Lattice $(not a shell) 中文";
    driver
        .act(&target.id, &Action::Type { text: text.into() }, &cancel)
        .unwrap_or_else(|e| panic!("native typing: {}", e.message));
    loop {
        if receive
            .recv_timeout(Duration::from_secs(15))
            .expect("native field did not acknowledge input")
            == format!("TEXT:{text}")
        {
            break;
        }
    }
    let after = driver
        .observe(&target.id, &cancel)
        .unwrap_or_else(|e| panic!("native post-action capture: {}", e.message));
    assert_eq!((before.width, before.height), (after.width, after.height));
    assert_ne!(before.png, after.png, "the observed field must change");
    driver.close();
}
