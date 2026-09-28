use super::*;

fn script(directory: &std::path::Path, body: &str) -> String {
    let path = directory.join("child.py");
    std::fs::write(&path, body).unwrap();
    format!("python3 {}", path.display())
}

#[test]
fn failed_bridge_cleanup_reaps_its_child() {
    let directory = tempfile::tempdir().unwrap();
    let entry = script(directory.path(), "import sys\nsys.stdin.readline()\n");
    let (central, _messages) = mpsc::channel();
    let (wake, _wakes) = mpsc::channel();
    let child = materialise("fixture", &entry, &None, "test", &central, &wake, &[]).unwrap();
    let pid = child.pid;
    let groups = Mutex::new([("fixture".into(), pid)].into());
    child.reap("fixture", &groups);
    assert!(groups.lock().unwrap().is_empty());
    // The fixture exits after hello. On the old path this call also cleans up
    // its zombie before failing the assertion; on the fixed path it must say
    // there is no child left to reap. No sleep is needed to arrange exit.
    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
    let error = std::io::Error::last_os_error();
    assert_eq!(
        waited, -1,
        "the bridge must reap its child, not leave that duty to its caller"
    );
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
}

#[test]
fn normal_bridge_exit_removes_its_process_group_registration() {
    let directory = tempfile::tempdir().unwrap();
    let entry = script(
        directory.path(),
        r#"
import json
import sys
for line in sys.stdin:
    if "stop" in json.loads(line):
        break
"#,
    );
    let (central, _messages) = mpsc::channel();
    let (wake, _wakes) = mpsc::channel();
    let groups = Arc::new(Mutex::new(HashMap::new()));
    let (mailbox, bridge) = spawn_process_bridge(
        BridgeSeat {
            instance: "fixture".into(),
            entry,
            config: None,
            stream: "test".into(),
            lazy: false,
            env_deny: vec![],
        },
        central,
        wake,
        groups.clone(),
    )
    .unwrap();
    assert_eq!(groups.lock().unwrap().len(), 1);
    drop(mailbox);
    bridge.join().unwrap();
    assert!(
        groups.lock().unwrap().is_empty(),
        "a reaped child must not leave a process id for later kernel cleanup to signal"
    );
}

#[test]
fn registered_signals_finish_under_the_lock_before_reaping_can_proceed() {
    for selected in [Some("first"), None] {
        let groups = Mutex::new([("first".into(), 11), ("second".into(), 22)].into());
        let mut signalled = Vec::new();
        signal_registered_groups(&groups, selected, |pid| {
            assert!(
                matches!(groups.try_lock(), Err(std::sync::TryLockError::WouldBlock)),
                "the registration lock must still protect the pid while signalling"
            );
            signalled.push(pid);
        });
        signalled.sort();
        if selected.is_some() {
            assert_eq!(signalled, vec![11]);
            assert_eq!(*groups.lock().unwrap(), [("second".into(), 22)].into());
        } else {
            assert_eq!(signalled, vec![11, 22]);
            assert!(groups.lock().unwrap().is_empty());
        }
    }
}

struct ChildGuard(Option<std::process::Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

// Stderr is a separate duplex control socket, not either bridge pipe. READY
// proves the selected pipe is closed while the process is still alive. The
// control byte is a rescue exit if a deliberately broken cleanup gets stuck.
fn paused_child(closed_fd: i32) -> (ChildGuard, std::os::unix::net::UnixStream) {
    use std::io::BufRead;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let (control, child_control) = UnixStream::pair().unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let child = Command::new("python3")
        .args([
            "-c",
            &format!("import os\nos.close({closed_fd})\nos.write(2, b'ready\\n')\nos.read(2, 1)\n"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(OwnedFd::from(child_control)))
        .process_group(0)
        .spawn()
        .unwrap();
    let guard = ChildGuard(Some(child));
    let mut ready = String::new();
    std::io::BufReader::new(control.try_clone().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    (guard, control)
}

fn assert_reaped(pid: i32) {
    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    let error = std::io::Error::last_os_error();
    assert_eq!(waited, -1, "cleanup must finish waiting for its child");
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
}

#[test]
fn failed_hello_cleans_up_a_live_unregistered_child() {
    let (mut guard, _control) = paused_child(0);
    let child = guard.0.as_mut().unwrap();
    let pid = child.id() as i32;
    let mut stdin = child.stdin.take().unwrap();
    let error = send_bridge_hello(child, &mut stdin, &json!({"hello": {}})).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    assert_reaped(pid);
}

#[test]
fn cleanup_terminates_a_live_child_with_either_bridge_pipe_closed() {
    use std::io::Write;
    for closed_fd in [0, 1] {
        let (mut guard, mut control) = paused_child(closed_fd);
        let mut child = guard.0.take().unwrap();
        let pid = child.id() as i32;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let (_processed, processed) = mpsc::channel();
        let bridge = BridgeChild {
            stdin,
            processed,
            reader,
            child,
            pid,
        };
        let groups = Arc::new(Mutex::new([("fixture".into(), pid)].into()));
        let registered = groups.clone();
        let (done, completed) = mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            bridge.reap("fixture", &registered);
            let _ = done.send(());
        });
        let finished = completed.recv_timeout(Duration::from_secs(5)).is_ok();
        if !finished {
            let _ = control.write_all(b"q");
        }
        cleanup.join().unwrap();
        assert!(
            finished,
            "cleanup must not depend on a later kernel kill or pipe closure"
        );
        assert!(groups.lock().unwrap().is_empty());
        assert_reaped(pid);
    }
}

#[test]
fn an_old_bridge_does_not_remove_a_replacement_registration() {
    let directory = tempfile::tempdir().unwrap();
    let entry = script(directory.path(), "import sys\nsys.stdin.readline()\n");
    let (central, _messages) = mpsc::channel();
    let (wake, _wakes) = mpsc::channel();
    let child = materialise("fixture", &entry, &None, "test", &central, &wake, &[]).unwrap();
    let pid = child.pid;
    // Both ids belong to unreaped test children; even a broken implementation
    // must never be allowed to signal an unrelated process during this test.
    let successor = materialise("fixture", &entry, &None, "test", &central, &wake, &[]).unwrap();
    let replacement = successor.pid;
    let groups = Mutex::new([("fixture".into(), replacement)].into());
    child.reap("fixture", &groups);
    let preserved = groups.lock().unwrap().get("fixture") == Some(&replacement);
    successor.reap("fixture", &groups);
    assert_reaped(pid);
    assert_reaped(replacement);
    assert!(
        preserved,
        "retiring an old child must leave its replacement registered"
    );
}
