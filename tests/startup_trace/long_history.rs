//! Manual main-TUI startup acceptance against a private copy of a real ledger.
//! No credentials, overlays, restored side tabs, or user prompts are supplied.

use super::*;
use serde_json::Value;
use std::io::BufRead;
use std::path::Path;

fn tail(root: &Path, after: u64) -> Vec<EventEnvelope> {
    let catalog: Value =
        serde_json::from_reader(File::open(root.join("catalog.json")).unwrap()).unwrap();
    let mut events = Vec::new();
    for segment in catalog["segments"].as_array().unwrap() {
        if segment["seal"]["through"]
            .as_u64()
            .is_some_and(|end| end <= after)
        {
            continue;
        }
        let file = root.join(format!("{:020}.jsonl", segment["number"].as_u64().unwrap()));
        let mut file = std::io::BufReader::new(File::open(file).unwrap());
        let mut line = Vec::new();
        loop {
            line.clear();
            file.read_until(b'\n', &mut line).unwrap();
            // A partial final line is retried after the next notification.
            if line.last() != Some(&b'\n') {
                break;
            }
            let event: EventEnvelope = serde_json::from_slice(&line).unwrap();
            if event.seq > after {
                events.push(event);
            }
        }
    }
    events
}

struct PrivateHome(Option<tempfile::TempDir>);

impl PrivateHome {
    fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}

impl Drop for PrivateHome {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "failed startup fixture retained at {}",
                self.0.take().unwrap().keep().display()
            );
        }
    }
}

#[test]
#[ignore = "requires LATTICE_RECOVERY_FIXTURE; clones a large ledger and runs two offline TUI starts"]
fn offline_saved_ledger_first_frame() {
    let source = std::path::PathBuf::from(
        std::env::var_os("LATTICE_RECOVERY_FIXTURE").expect("set LATTICE_RECOVERY_FIXTURE"),
    );
    let home = PrivateHome(Some(tempfile::tempdir().unwrap()));
    let ledgers = home.path().join(".lattice/ledgers");
    std::fs::create_dir_all(&ledgers).unwrap();
    let root = ledgers.join("fixture.ledger");
    let flag = if cfg!(target_os = "macos") {
        "-cR"
    } else {
        "-R"
    };
    assert!(Command::new("cp")
        .arg(flag)
        .arg(&source)
        .arg(&root)
        .status()
        .unwrap()
        .success());
    // Side conversations are independent runtimes, outside this main-startup
    // measurement. Keep their manifest in the copy but do not auto-open them.
    let tabs = root.join("documents/btw/tabs.json");
    if tabs.exists() {
        std::fs::rename(&tabs, tabs.with_extension("disabled-for-startup-test")).unwrap();
    }
    let reader = lattice::LogReader::segmented_snapshot(&root).unwrap();
    let original_end = reader.snapshot_end();
    // Named lookup is scoped to the recorded project, even under a private HOME.
    let cwd = reader
        .scan_back_types(
            &["core.stream.opened", "core.stream.resumed"],
            |event, _| {
                Ok(event.payload["cwd"]
                    .as_str()
                    .filter(|cwd| !cwd.is_empty())
                    .map(std::path::PathBuf::from))
            },
        )
        .unwrap()
        .expect("fixture needs a recorded project directory");
    assert!(cwd.is_dir(), "recorded project must exist for named lookup");
    drop(reader);

    enum Notice {
        Ledger,
        Output,
        Closed,
    }
    let (changed_tx, changed_rx) = mpsc::channel();
    let file_tx = changed_tx.clone();
    // One subscription spans both starts. Terminal activity also wakes the
    // observer; neither path polls the ledger on a timer.
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_err()
            || event.as_ref().is_ok_and(|event| {
                event
                    .paths
                    .iter()
                    .any(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            })
        {
            let _ = file_tx.send(Notice::Ledger);
        }
    })
    .unwrap();
    watcher
        .watch(&root, notify::RecursiveMode::NonRecursive)
        .unwrap();
    for launch in 1..=2 {
        let reader = lattice::LogReader::segmented_snapshot(&root).unwrap();
        let before = reader.snapshot_end();
        drop(reader);
        while changed_rx.try_recv().is_ok() {}
        let closed_tx = changed_tx.clone();

        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 40,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::addr_of_mut!(size),
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        // Optional reviewed predecessor exercises a real executable upgrade,
        // not just two starts of the same binary. It runs in the same isolation.
        let binary = if launch == 1 {
            std::env::var_os("LATTICE_RECOVERY_BASELINE_BIN")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_lattice").into())
        } else {
            env!("CARGO_BIN_EXE_lattice").into()
        };
        let mut command = Command::new(binary);
        command
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("LATTICE_SCRIPTED", "1")
            .env("LATTICE_WORKSPACE", home.path())
            .env("LATTICE_OVERLAY", "")
            .current_dir(&cwd)
            .args(["--resume", "fixture"])
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                #[allow(clippy::unnecessary_cast)]
                let request = libc::TIOCSCTTY as _;
                if libc::setsid() < 0 || libc::ioctl(0, request, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = Running(command.spawn().unwrap());
        drop(command);
        let mut output = master.try_clone().unwrap();
        let (output_tx, output_rx) = mpsc::channel();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output_copy = transcript.clone();
        let output_thread = std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            loop {
                match output.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut bytes = output_copy.lock().unwrap();
                        bytes.extend_from_slice(&buffer[..n]);
                        if bytes.len() > 65536 {
                            let remove = bytes.len() - 65536;
                            bytes.drain(..remove);
                        }
                        let _ = closed_tx.send(Notice::Output);
                    }
                }
            }
            let _ = output_tx.send(output_copy.lock().unwrap().clone());
            let _ = closed_tx.send(Notice::Closed);
        });
        eprintln!(
            "startup launch {launch}, pid {}, fixture {}",
            child.0.id(),
            root.display()
        );
        let deadline =
            std::time::Instant::now() + Duration::from_secs(if launch == 1 { 180 } else { 60 });
        let startup = loop {
            let events = tail(&root, before);
            assert!(
                !events.iter().any(|e| e.event_type == "core.control.error"),
                "startup error in ledger"
            );
            if let Some(event) = events
                .into_iter()
                .find(|event| event.event_type == STARTUP_COST)
            {
                break event;
            }
            match changed_rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_else(|error| {
                    let text = String::from_utf8_lossy(&transcript.lock().unwrap()).into_owned();
                    panic!(
                        "startup deadline exceeded: {error}; status {:?}; terminal {text}",
                        child.0.try_wait()
                    )
                }) {
                Notice::Ledger | Notice::Output => {}
                Notice::Closed => panic!(
                    "TUI exited before first frame: {}",
                    String::from_utf8_lossy(&output_rx.recv().unwrap())
                ),
            }
        };
        eprintln!(
            "real TUI launch {launch}: totalMs {}, phases {}, kernel {}",
            startup.payload["frontend"]["totalMs"],
            startup.payload["frontend"]["phasesMs"],
            startup.payload["kernel"]["phasesMs"]
        );
        if launch == 2 {
            assert!(
                startup.payload["replayMemory"]["stages"]["bookkeeping"]["operations"]
                    .as_u64()
                    .unwrap()
                    < 100,
                "warm UI must only fold the new suffix"
            );
        }
        master.write_all(b"\x04").unwrap();
        let bytes = output_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("TUI did not exit after Ctrl-D");
        assert!(!bytes.is_empty());
        assert!(
            child.0.wait().unwrap().success(),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        output_thread.join().unwrap();
    }
    assert!(
        !tail(&root, original_end).iter().any(|event| matches!(
            event.event_type.as_str(),
            "core.tool.exec_started" | "core.model.call_started"
        )),
        "startup must not execute old tools or call a model"
    );
}
