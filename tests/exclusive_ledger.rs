use lattice::core_events as ce;
use lattice::{AssemblyManifest, EventDraft, EventLog, Kernel, KernelOptions, Session};
use serde_json::json;
use std::path::{Path, PathBuf};

#[path = "common/ledger_snapshot.rs"]
mod snapshot;

fn create(
    path: Option<PathBuf>,
    stream: &str,
) -> Result<Kernel, lattice::kernel::host::KernelError> {
    Kernel::create_new(
        &AssemblyManifest {
            instances: Default::default(),
            wires: vec![],
        },
        &Default::default(),
        &mut Default::default(),
        KernelOptions {
            log_file: path,
            stream: Some(stream.into()),
            ..Default::default()
        },
    )
}

fn seed(path: &Path, with_event: bool) {
    let mut log =
        EventLog::open(ce::core_event_decls(), "same-stream", Some(path.to_owned())).unwrap();
    if with_event {
        log.append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"untouched"})),
            "fixture",
        )
        .unwrap();
    }
}

#[test]
fn exclusive_creation_rejects_every_occupied_leaf_without_changing_it() {
    let root = tempfile::tempdir().unwrap();
    let mut occupied = vec![];
    for (name, with_event) in [("history.ledger", true), ("zero.ledger", false)] {
        let path = root.path().join(name);
        seed(&path, with_event);
        occupied.push(path);
    }
    let active_path = root.path().join("active.ledger");
    seed(&active_path, true);
    let _active = EventLog::open(
        ce::core_event_decls(),
        "same-stream",
        Some(active_path.clone()),
    )
    .unwrap();
    occupied.push(active_path);
    let empty = root.path().join("empty.ledger");
    std::fs::create_dir(&empty).unwrap();
    occupied.push(empty);
    let broken = root.path().join("broken.ledger");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("catalog.json"), b"not a catalog").unwrap();
    std::fs::create_dir(broken.join("documents")).unwrap();
    std::fs::write(broken.join("documents/sentinel"), b"keep evidence").unwrap();
    occupied.push(broken);
    let file = root.path().join("file.ledger");
    std::fs::write(&file, b"not a directory").unwrap();
    occupied.push(file);
    #[cfg(unix)]
    for (name, target) in [
        ("directory-link.ledger", "history.ledger"),
        ("file-link.ledger", "file.ledger"),
        ("dangling.ledger", "missing-target"),
    ] {
        let path = root.path().join(name);
        std::os::unix::fs::symlink(target, &path).unwrap();
        occupied.push(path);
    }
    let before = snapshot::tree(root.path());
    for path in occupied {
        // Same identity is deliberate: a mistaken ordinary open must not be
        // hidden by the unrelated stream-mismatch guard.
        let result = create(Some(path.clone()), "same-stream");
        let error = match result {
            Ok(kernel) => {
                kernel.shutdown();
                None
            }
            Err(error) => Some(error.to_string()),
        };
        assert!(
            error.is_some(),
            "occupied path was adopted: {}",
            path.display()
        );
        assert!(error.unwrap().contains(&path.display().to_string()));
        assert_eq!(snapshot::tree(root.path()), before);
    }
}

#[test]
fn exclusive_creation_rejects_unsupported_storage_without_creating_it() {
    let root = tempfile::tempdir().unwrap();
    for path in [
        None,
        Some(PathBuf::new()),
        Some(root.path().join("flat.jsonl")),
        Some(root.path().join("plain")),
    ] {
        let result = create(path.clone(), "fresh");
        let error = match result {
            Ok(kernel) => {
                kernel.shutdown();
                None
            }
            Err(error) => Some(error.to_string()),
        };
        assert!(error.unwrap().contains("nonempty .ledger path"));
        if let Some(path) = path {
            if !path.as_os_str().is_empty() {
                assert!(!path.exists());
            }
        }
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn invalid_assembly_does_not_allocate_a_new_ledger() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("invalid-assembly.ledger");
    let assembly = AssemblyManifest {
        instances: [(
            "missing".into(),
            lattice::ComponentInstance {
                component: "unregistered".into(),
                requires: vec![],
                config: None,
            },
        )]
        .into(),
        wires: vec![],
    };
    let result = Kernel::create_new(
        &assembly,
        &Default::default(),
        &mut Default::default(),
        KernelOptions {
            log_file: Some(path.clone()),
            ..Default::default()
        },
    );
    let rejected = match result {
        Err(lattice::kernel::host::KernelError::Inspection(_)) => true,
        Err(_) => false,
        Ok(kernel) => {
            kernel.shutdown();
            false
        }
    };
    assert!(rejected);
    assert!(!path.exists());
}

#[test]
fn invalid_event_schema_does_not_allocate_a_new_ledger() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("invalid.ledger");
    let mut declaration = lattice::EventTypeDecl::new("fixture.invalid", "invalid schema fixture");
    declaration.schema = Some(json!({"type":"not-a-schema-type"}));
    let result = EventLog::create_new(vec![declaration], "fresh", path.clone());
    assert!(result.is_err());
    assert!(!path.exists());
}

#[test]
fn exclusive_writer_lease_survives_session_shutdown_until_final_log_is_dropped() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.ledger");
    let at = path.clone();
    let session = Session::spawn("unused", move |_| create(Some(at), "fresh-stream")).unwrap();
    let reader = session.log_reader();
    let during =
        EventLog::open(ce::core_event_decls(), "fresh-stream", Some(path.clone())).is_err();
    session.request_shutdown();
    let mut closed = session.finish_shutdown().unwrap();
    let after_shutdown =
        EventLog::open(ce::core_event_decls(), "fresh-stream", Some(path.clone())).is_err();
    closed
        .log
        .append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"final observation"})),
            "fixture",
        )
        .unwrap();
    let stream = closed.log.stream().to_owned();
    let lingering = closed.kernel.lingering.clone();
    drop(closed);
    let reopened =
        EventLog::open(ce::core_event_decls(), "fresh-stream", Some(path.clone())).unwrap();
    let events = reopened.replay(1).unwrap();
    drop(reopened);
    let rejected = create(Some(path), "fresh-stream");
    let fresh_rejected = match rejected {
        Ok(kernel) => {
            kernel.shutdown();
            false
        }
        Err(_) => true,
    };
    assert!(
        during && after_shutdown,
        "the same writer lease must span shutdown"
    );
    assert!(lingering.is_empty());
    assert_eq!(stream, "fresh-stream");
    assert_eq!(reader.stream(), stream);
    assert!(events
        .iter()
        .any(|event| event.payload["text"] == "final observation"));
    assert!(
        fresh_rejected,
        "release permits reopen, never a second exclusive creation"
    );
}

#[test]
fn ordinary_open_retains_memory_flat_and_existing_segmented_behavior() {
    let root = tempfile::tempdir().unwrap();
    for path in [
        None,
        Some(root.path().join("old.jsonl")),
        Some(root.path().join("old.ledger")),
    ] {
        let mut log = EventLog::open(ce::core_event_decls(), "old", path.clone()).unwrap();
        log.append(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":"history"})),
            "fixture",
        )
        .unwrap();
        drop(log);
        if path.is_some() {
            let log = EventLog::open(ce::core_event_decls(), "old", path).unwrap();
            assert_eq!(log.len(), 1);
        }
    }
}

// Separate OS processes compete at the actual create_dir boundary. Pipe
// messages, not sleeps or candidate-name randomness, coordinate their lifetime.
#[test]
fn exclusive_create_child() {
    use std::io::{BufRead, Write};
    let Some(path) = std::env::var_os("LATTICE_EXCLUSIVE_CREATE_TEST") else {
        return;
    };
    let mut input = std::io::stdin().lock();
    println!("CHILD_READY");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let kernel = create(Some(path.into()), "competing-stream");
    println!("CHILD_{}", if kernel.is_ok() { "WIN" } else { "LOSE" });
    std::io::stdout().flush().unwrap();
    line.clear();
    input.read_line(&mut line).unwrap();
    if let Ok(kernel) = kernel {
        kernel.shutdown();
    }
}

#[test]
fn two_processes_have_one_exclusive_creator_and_no_later_adoption() {
    use std::io::{BufRead, Write};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;
    struct ChildGuard {
        child: Child,
        reader: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("race.ledger");
    let mut children = vec![];
    for _ in 0..2 {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "exclusive_create_child", "--nocapture"])
            .env("LATTICE_EXCLUSIVE_CREATE_TEST", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((_, message)) = line.split_once("CHILD_") {
                    let _ = tx.send(message.to_owned());
                }
            }
        });
        children.push((
            ChildGuard {
                child,
                reader: Some(reader),
            },
            rx,
        ));
    }
    for (_, rx) in &children {
        assert_eq!(rx.recv_timeout(Duration::from_secs(15)).unwrap(), "READY");
    }
    for (child, _) in &mut children {
        writeln!(child.child.stdin.as_mut().unwrap(), "go").unwrap();
    }
    let mut outcomes = vec![];
    for (_, rx) in &children {
        outcomes.push(rx.recv_timeout(Duration::from_secs(15)).unwrap());
    }
    for (child, _) in &mut children {
        writeln!(child.child.stdin.as_mut().unwrap(), "stop").unwrap();
    }
    let mut exits = vec![];
    for (child, _) in &mut children {
        exits.push(child.child.wait().unwrap());
    }
    drop(children);
    outcomes.sort();
    assert_eq!(outcomes, ["LOSE", "WIN"]);
    assert!(exits.iter().all(|status| status.success()));
    let result = create(Some(path), "competing-stream");
    let rejected = match result {
        Ok(kernel) => {
            kernel.shutdown();
            false
        }
        Err(_) => true,
    };
    assert!(rejected);
}
