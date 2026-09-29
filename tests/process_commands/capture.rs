//! Bounded capture for self-contained test children. Completion is driven by
//! process exit and pipe EOF; deadlines only detect failure. No new dependency.

use std::io;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;

fn spawn(command: Command) -> io::Result<Child> {
    tokio::process::Command::from(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

async fn collect(child: &mut Child, input: &[u8], limit: Duration) -> io::Result<Output> {
    let pid = child.id().expect("fresh child");
    let mut stdin = child.stdin.take().expect("spawn captures stdin");
    let mut stdout = child.stdout.take().expect("spawn captures stdout");
    let mut stderr = child.stderr.take().expect("spawn captures stderr");
    let completion = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let write = async {
            stdin.write_all(input).await?;
            drop(stdin);
            Ok::<_, io::Error>(())
        };
        let (status, _, _, _) = tokio::try_join!(
            child.wait(),
            write,
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
        )?;
        Ok(Output {
            status,
            stdout: out,
            stderr: err,
        })
    };
    let error = match tokio::time::timeout(limit, completion).await {
        Ok(Ok(output)) => return Ok(output),
        Ok(Err(error)) => error,
        Err(_) => io::Error::new(
            io::ErrorKind::TimedOut,
            format!("child {pid} exceeded its capture deadline"),
        ),
    };
    // Tokio's kill awaits wait: returning an error means termination AND reaping,
    // not just a signal sent to a process which might still be running.
    child.kill().await?;
    Err(io::Error::new(
        error.kind(),
        format!("{error}; child {pid} terminated and reaped"),
    ))
}

pub(super) fn output(command: Command, input: &[u8], limit: Duration) -> io::Result<Output> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async { collect(&mut spawn(command)?, input, limit).await })
}

#[test]
fn timeout_and_write_errors_terminate_and_reap_children() {
    const FIXTURE: &str = "LATTICE_CAPTURE_FIXTURE";
    let mode = std::env::var(FIXTURE).ok();
    if matches!(mode.as_deref(), Some("blocked" | "closed-input")) {
        if mode.as_deref() == Some("closed-input") {
            // Only the re-executed fixture closes its own piped descriptor.
            assert_eq!(unsafe { libc::close(libc::STDIN_FILENO) }, 0);
        }
        use std::io::Write;
        std::io::stdout().write_all(b"CAPTURE_READY\n").unwrap();
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
    let home = tempfile::tempdir().unwrap();
    let name = std::thread::current().name().unwrap().to_owned();
    let command = |mode: &str| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &name, "--nocapture"])
            .env_clear()
            .env("HOME", home.path())
            .env("TMPDIR", home.path())
            .env("PATH", "/usr/bin:/bin")
            .env(FIXTURE, mode)
            .current_dir(home.path());
        command
    };
    if mode.is_none() {
        // Concurrent sibling spawns may briefly inherit a pipe reader before
        // exec, even with CLOEXEC. A small write can then succeed after the
        // fixture closes fd 0. Create the tested pipes in a dedicated process
        // whose only spawns are the sequential fixtures below.
        let result = output(command("driver"), b"", Duration::from_secs(120)).unwrap();
        assert!(
            result.status.success(),
            "isolated capture checks failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
        );
        return;
    }
    assert_eq!(mode.as_deref(), Some("driver"));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (mode, input, limit, expected) in [
                ("blocked", &b""[..], Duration::ZERO, io::ErrorKind::TimedOut),
                (
                    "closed-input",
                    &b"input"[..],
                    Duration::from_secs(30),
                    io::ErrorKind::BrokenPipe,
                ),
            ] {
                let mut child = spawn(command(mode)).unwrap();
                // The child has reached its deliberate stall (and, in the second
                // case, closed stdin) before starting the failure check.
                let ready = tokio::time::timeout(Duration::from_secs(30), async {
                    let stdout = child.stdout.as_mut().unwrap();
                    let mut seen = Vec::new();
                    while !seen.ends_with(b"CAPTURE_READY\n") {
                        seen.push(stdout.read_u8().await?);
                    }
                    Ok::<_, io::Error>(())
                })
                .await;
                if !matches!(ready, Ok(Ok(()))) {
                    child.kill().await.unwrap();
                    panic!("fixture did not become ready: {ready:?}");
                }
                let result = collect(&mut child, input, limit).await;
                let reaped = child.id().is_none();
                if !reaped {
                    // Keep the test itself safe even when capture cleanup is poisoned.
                    child.kill().await.unwrap();
                }
                assert!(reaped, "capture returned before reaping its child");
                let error = result.unwrap_err();
                assert_eq!(error.kind(), expected);
                assert!(error.to_string().contains("terminated and reaped"));
                assert!(!child.try_wait().unwrap().unwrap().success());
            }
        });
}
