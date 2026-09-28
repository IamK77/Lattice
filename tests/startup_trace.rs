//! Exercise the actual TUI entry and first draw, offline, in a private terminal.
#![cfg(unix)]

#[path = "startup_trace/long_history.rs"]
mod long_history;
#[path = "startup_trace/preflight.rs"]
mod preflight;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use lattice::components::silent_ui::STARTUP_COST;
use lattice::EventEnvelope;
use notify::Watcher;

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn history(home: &std::path::Path) -> Vec<EventEnvelope> {
    let mut events = Vec::new();
    for entry in std::fs::read_dir(home.join(".lattice/ledgers")).unwrap() {
        let path = entry.unwrap().path();
        let files = if path.extension().is_some_and(|ext| ext == "jsonl") {
            vec![path]
        } else if path.extension().is_some_and(|ext| ext == "ledger")
            && path.join("catalog.json").exists()
        {
            let catalog: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path.join("catalog.json")).unwrap()).unwrap();
            catalog["segments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|segment| {
                    path.join(format!("{:020}.jsonl", segment["number"].as_u64().unwrap()))
                })
                .collect()
        } else {
            Vec::new()
        };
        for file in files {
            let text = std::fs::read_to_string(file).unwrap();
            events.extend(
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok()),
            );
        }
    }
    events
}

fn assert_history_sample(value: &serde_json::Value) {
    if let Some(stats) = value.get("stats") {
        assert!(stats["events"].as_u64().unwrap() > 0);
        assert_eq!(stats["inMemoryBodies"], 0);
        assert!(stats["cache"]["decodes"].is_u64());
    } else {
        assert!(
            value["error"].as_str().unwrap().contains("busy"),
            "unexpected observation failure: {value}"
        );
    }
}

#[test]
fn fresh_and_resumed_tui_record_one_complete_startup_each() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".lattice/ledgers")).unwrap();
    let (changed_tx, changed_rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = changed_tx.send(event);
    })
    .unwrap();
    watcher
        .watch(dir.path(), notify::RecursiveMode::Recursive)
        .unwrap();

    for launch in 1..=2 {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 40,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // The child gets its own controlling terminal, never the test runner's.
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_lattice"));
        command
            .env_clear()
            .env("HOME", dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("LATTICE_ADAPTER", "scripted")
            .env("LATTICE_OVERLAY", "")
            .current_dir(dir.path())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        if launch == 2 {
            command.arg("-c");
        }
        unsafe {
            command.pre_exec(|| {
                // The ioctl request type differs between Unix targets.
                #[allow(clippy::unnecessary_cast)]
                let request = libc::TIOCSCTTY as _;
                if libc::setsid() < 0 || libc::ioctl(0, request, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = Running(command.spawn().unwrap());
        // Command owns the parent's slave handles until dropped.
        drop(command);
        let mut output = master.try_clone().unwrap();
        let (closed_tx, closed_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match output.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                }
            }
            let _ = closed_tx.send(bytes);
        });

        // File notifications, not sleeps: the successful draw must produce a
        // complete ledger event. The timeout only bounds a broken implementation.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let events = history(dir.path());
            if events
                .iter()
                .filter(|e| e.event_type == STARTUP_COST)
                .count()
                == launch
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "startup deadline exceeded"
            );
            changed_rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("startup did not reach the ledger")
                .unwrap();
        }
        let baseline = history(dir.path())
            .iter()
            .filter(|e| e.event_type == lattice::components::silent_ui::RENDER_COST)
            .count();
        master.write_all(b"memory trace probe\r").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if history(dir.path())
                .iter()
                .filter(|e| e.event_type == lattice::components::silent_ui::RENDER_COST)
                .count()
                > baseline
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "turn memory trace deadline exceeded"
            );
            changed_rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("turn observation did not reach the ledger")
                .unwrap();
        }
        master.write_all(b"\x04").unwrap();
        let bytes = closed_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("TUI did not exit after Ctrl-D");
        assert!(!bytes.is_empty(), "the real terminal received output");
        let text = String::from_utf8_lossy(&bytes);
        let active = text
            .split_once("\x1b[?1049h")
            .expect("entered alternate screen")
            .1
            .split_once("\x1b[?1049l")
            .expect("left alternate screen")
            .0;
        assert!(
            !active.contains("slow recovery for"),
            "raw recovery output escaped into the screen: {active}"
        );
        if launch == 1 {
            assert!(
                active.contains("Diagnostics saved to"),
                "the renderer must show where diagnostics went"
            );
        }
        assert!(
            child.0.wait().unwrap().success(),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        reader.join().unwrap();
    }

    let mut diagnostics = String::new();
    for entry in std::fs::read_dir(dir.path().join(".lattice/ledgers")).unwrap() {
        let ledger = entry.unwrap().path();
        if !ledger
            .extension()
            .is_some_and(|ext| ext == "jsonl" || ext == "ledger")
        {
            continue;
        }
        let documents = lattice::contracts::document::documents_dir(&ledger);
        // The ledger directory also contains the history index, not a stream.
        if !documents.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(documents).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("terminal-diagnostics-")
            {
                diagnostics.push_str(&std::fs::read_to_string(path).unwrap());
            }
        }
    }
    assert!(diagnostics.contains("slow recovery for terminal state"));
    let events = history(dir.path());
    let inputs: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == lattice::core_events::USER_MESSAGE && e.causes.is_empty())
        .map(|e| e.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(inputs, ["memory trace probe", "memory trace probe"]);
    let notes: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == STARTUP_COST)
        .collect();
    assert_eq!(notes.len(), 2);
    for (index, event) in notes.iter().enumerate() {
        let note: lattice::session::StartupNote =
            serde_json::from_value(event.payload.clone()).unwrap();
        assert_eq!(event.source, "ui");
        assert_eq!(note.resumed, index == 1);
        assert_eq!(note.history_events > 0, index == 1);
        assert!(chrono::DateTime::parse_from_rfc3339(&note.started_at).is_ok());
        assert!(chrono::DateTime::parse_from_rfc3339(&note.first_frame_at).is_ok());
        for phase in [
            "config",
            "ledger_select",
            "prepare",
            "session_start",
            "terminal_setup",
            "history_snapshot",
            "ui_rebuild",
            "pre_draw",
            "first_draw",
        ] {
            assert!(
                note.frontend.phases_ms.contains_key(phase),
                "missing {phase}"
            );
        }
        for timing in [&note.frontend, &note.kernel] {
            assert_eq!(timing.memory.first().unwrap().phase, "start");
            for phase in timing
                .phases_ms
                .keys()
                .filter(|phase| phase.as_str() != "memory_sampling")
            {
                assert!(
                    timing.memory.iter().any(|point| &point.phase == phase),
                    "missing memory boundary {phase}"
                );
            }
            for point in &timing.memory {
                assert_ne!(
                    point.memory.pid,
                    std::process::id(),
                    "sample the child, not the test runner"
                );
                #[cfg(target_os = "macos")]
                assert!(point.memory.footprint_bytes.unwrap() > 0);
            }
        }
        assert_history_sample(&note.history);
        assert!(note.ui_counts["calls"].is_u64());
        assert!(note
            .replay_memory
            .stages
            .contains_key(&lattice::memory::Stage::BetweenBatches));
        if note.resumed {
            // State recovery folds only the suffix after a valid checkpoint;
            // paged cards are restored separately, not rebuilt by this fold.
            let folded = note
                .replay_memory
                .stages
                .get(&lattice::memory::Stage::Bookkeeping)
                .map_or(0, |cost| cost.operations);
            assert!(folded <= note.history_events as u64);
            for stage in [
                lattice::memory::Stage::Size,
                lattice::memory::Stage::Usage,
                lattice::memory::Stage::Background,
            ] {
                let cost = note.replay_memory.stages.get(&stage);
                assert_eq!(cost.map_or(0, |cost| cost.operations), folded);
                if folded > 0 {
                    assert!(cost.unwrap().samples > 0);
                }
            }
        }
        let sum: f64 = note.frontend.phases_ms.values().sum();
        assert!((sum - note.frontend.total_ms).abs() < 1e-6);
        assert!(note.kernel.total_ms <= note.frontend.phases_ms["session_start"]);
    }
    let rendered: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == lattice::components::silent_ui::RENDER_COST)
        .collect();
    assert!(rendered.len() >= 2);
    for event in rendered {
        assert_ne!(
            event.payload["memory"]["pid"].as_u64().unwrap(),
            u64::from(std::process::id())
        );
        assert_history_sample(&event.payload["history"]);
    }
    let input_costs: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == lattice::components::silent_ui::INPUT_COST)
        .collect();
    assert_eq!(
        input_costs.len(),
        2,
        "one receipt per explicit Enter, including after resume"
    );
    for event in input_costs {
        let note: lattice::input_latency::Note =
            serde_json::from_value(event.payload.clone()).unwrap();
        assert_eq!(event.causes, std::slice::from_ref(&note.event));
        let input = events.iter().find(|e| e.id == note.event).unwrap();
        assert_eq!(input.event_type, lattice::core_events::USER_MESSAGE);
        assert!(input.causes.is_empty());
        assert!(note.notified_ms <= note.ui_received_ms);
        assert!(note.ui_received_ms <= note.absorbed_ms);
        assert!(note.absorbed_ms <= note.first_frame_ms);
        assert!(note.draw_ms <= note.first_frame_ms);
        assert_eq!(note.dropped_observations, 0);
        assert!(!event.payload.to_string().contains("memory trace probe"));
    }
    let closes: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == lattice::components::silent_ui::SHUTDOWN_COST)
        .collect();
    assert_eq!(
        closes.len(),
        2,
        "Each private TUI exit must finish its observation"
    );
    assert_eq!(events.last().unwrap().id, closes.last().unwrap().id);
    let index = std::fs::read_to_string(dir.path().join(".lattice/ledgers/index.jsonl")).unwrap();
    let indexed: serde_json::Value = serde_json::from_str(index.lines().last().unwrap()).unwrap();
    let last = events.last().unwrap();
    assert_eq!(indexed["events"], serde_json::json!(last.seq));
    assert_eq!(indexed["last"], serde_json::json!(last.time));
    let ledger = dir
        .path()
        .join(".lattice/ledgers")
        .join(indexed["file"].as_str().unwrap());
    assert_eq!(
        indexed["bytes"],
        serde_json::json!(std::fs::metadata(ledger).unwrap().len())
    );
    for event in closes {
        assert_eq!(event.source, "ui");
        let timing: lattice::startup::Timings =
            serde_json::from_value(event.payload["frontend"].clone()).unwrap();
        for phase in [
            "request_stop",
            "terminal_restore",
            "children_finish",
            "main_finish",
            "index_summary",
            "finalize",
        ] {
            assert!(
                timing.phases_ms.contains_key(phase),
                "Missing shutdown phase {phase}"
            );
        }
        assert!((timing.phases_ms.values().sum::<f64>() - timing.total_ms).abs() < 1e-6);
        assert_eq!(event.payload["errors"], serde_json::json!([]));
        assert_eq!(event.payload["kernel"]["lingering"], serde_json::json!([]));
        assert_eq!(event.payload["children"]["errors"], serde_json::json!([]));
        eprintln!("shutdown observation: {}", event.payload);
    }
    // Launch itself must not ask the model. Only the explicit offline probe
    // after the first frame may do so (including on the resumed launch).
    for note in &notes {
        let previous_close = events
            .iter()
            .filter(|e| {
                e.event_type == lattice::components::silent_ui::SHUTDOWN_COST && e.seq < note.seq
            })
            .map(|e| e.seq)
            .max()
            .unwrap_or(0);
        assert!(!events.iter().any(|e| e.seq > previous_close
            && e.seq < note.seq
            && e.event_type == lattice::core_events::MODEL_CALL_STARTED));
    }
    assert!(events
        .iter()
        .any(|e| e.event_type == lattice::core_events::MODEL_CALL_COMPLETED));
    assert!(!events
        .iter()
        .any(|e| e.event_type == lattice::core_events::ERROR));
}
