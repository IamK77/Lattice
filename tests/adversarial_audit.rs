//! Retained reproductions from the adversarial audit.
#![cfg(unix)]

use lattice::daemon::Daemon;
use lattice::{core_events as ce, EventDraft, EventLog, StreamHost};
use serde_json::json;
use std::os::unix::net::{UnixListener, UnixStream};

#[test]
fn daemon_start_must_not_delete_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("important.txt");
    std::fs::write(&path, b"only copy").unwrap();
    let result = Daemon::serve(&path, || StreamHost::new(Default::default()));
    let refused = result.is_err();
    if let Ok(daemon) = result {
        daemon.stop();
    }
    assert!(
        refused,
        "an existing regular file must not be replaced by a socket"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"only copy");
}

#[test]
fn daemon_start_must_not_replace_a_live_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("active.sock");
    let owner = UnixListener::bind(&path).unwrap();
    let result = Daemon::serve(&path, || StreamHost::new(Default::default()));
    let refused = result.is_err();
    if let Ok(daemon) = result {
        daemon.stop();
    }
    assert!(refused, "a live socket must retain its original owner");
    assert!(UnixStream::connect(&path).is_ok());
    drop(owner);
}

#[test]
fn recovery_preserves_a_complete_event_without_its_final_newline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let mut log = EventLog::open(ce::core_event_decls(), "main", Some(path.clone())).unwrap();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "first"})),
        "ui",
    )
    .unwrap();
    drop(log);
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    std::fs::write(&path, bytes).unwrap();
    let mut log = EventLog::open(ce::core_event_decls(), "main", Some(path.clone())).unwrap();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "second"})),
        "ui",
    )
    .unwrap();
    drop(log);
    let reopened = EventLog::open(ce::core_event_decls(), "main", Some(path)).unwrap();
    assert_eq!(
        reopened.replay(1).unwrap().len(),
        2,
        "both completed events must survive reopening"
    );
}

#[test]
fn recovery_discards_an_incomplete_utf8_tail() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let mut log = EventLog::open(ce::core_event_decls(), "main", Some(path.clone())).unwrap();
    log.append(
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "first"})),
        "ui",
    )
    .unwrap();
    drop(log);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"{\"text\":\"\xe4\xbd").unwrap();
    drop(file);
    let reopened = EventLog::open(ce::core_event_decls(), "main", Some(path));
    assert!(
        reopened.is_ok(),
        "a torn UTF-8 suffix must not make the good prefix unreadable"
    );
    assert_eq!(reopened.unwrap().replay(1).unwrap().len(), 1);
}

#[test]
fn rejected_install_names_never_overwrite_source_files() {
    let dir = tempfile::tempdir().unwrap();
    let workshop = dir.path().join("workshop");
    std::fs::create_dir(&workshop).unwrap();
    let outside = dir.path().join("victim.py");
    let inside = workshop.join("existing.py");
    std::fs::write(&outside, "outside original").unwrap();
    std::fs::write(&inside, "inside original").unwrap();
    let assembly = lattice::AssemblyManifest {
        instances: Default::default(),
        wires: vec![],
    };
    let mut kernel = lattice::Kernel::start(
        &assembly,
        &Default::default(),
        &mut Default::default(),
        Default::default(),
    )
    .unwrap();
    for name in [
        dir.path().join("victim").display().to_string(),
        "../victim".into(),
        "existing".into(),
    ] {
        let req = lattice::workshop::PendingInstall {
            call: "test".into(),
            cause: "unused".into(),
            args: json!({"instance": name, "source": "invalid python source"}),
        };
        let result = lattice::workshop::build_and_install(
            &mut kernel,
            &req,
            &workshop,
            "loop",
            None,
            |_, _| false,
        );
        assert!(!matches!(
            result,
            Ok(lattice::workshop::BuildOutcome::Installed(_))
        ));
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "outside original"
        );
        assert_eq!(std::fs::read_to_string(&inside).unwrap(), "inside original");
    }
}
