use super::command;
use lattice::{core_events as ce, ClientMessage, EventEnvelope, ServerMessage};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Client {
    write: UnixStream,
    read: BufReader<UnixStream>,
}
impl Client {
    fn connect(path: &std::path::Path) -> Self {
        let write = UnixStream::connect(path).unwrap();
        write
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let read = BufReader::new(write.try_clone().unwrap());
        Self { write, read }
    }

    fn send(&mut self, message: ClientMessage) {
        writeln!(self.write, "{}", serde_json::to_string(&message).unwrap()).unwrap();
    }

    fn until(&mut self, mut done: impl FnMut(&ServerMessage) -> bool) -> Vec<ServerMessage> {
        let mut messages = Vec::new();
        loop {
            let mut line = String::new();
            assert!(
                self.read.read_line(&mut line).unwrap() > 0,
                "daemon disconnected before answering"
            );
            let message = serde_json::from_str(&line).unwrap();
            let finished = done(&message);
            messages.push(message);
            if finished {
                return messages;
            }
        }
    }
}

#[test]
fn private_daemon_keeps_host_metadata_name_filter_redaction_and_graceful_stop() {
    let home = tempfile::tempdir().unwrap();
    let settings = home.path().join(".lattice");
    std::fs::create_dir_all(&settings).unwrap();
    let secret = "fixture-only-daemon-key-not-a-real-credential";
    std::fs::write(
        settings.join("models.json"),
        json!({"models":{"unused":{
            "adapter":"openai", "model":"unused", "baseUrl":"https://not-called.invalid",
            "apiKeyEnv":"LATTICE_DAEMON_FIXTURE_KEY"
        }}})
        .to_string(),
    )
    .unwrap();
    let socket = home.path().join("private.sock");
    let mut child = Running(
        command(home.path())
            .args(["serve", "ignored"])
            .env("LATTICE_SCRIPTED", "1")
            .env("LATTICE_DAEMON_FIXTURE_KEY", secret)
            .env("LATTICE_SOCKET", &socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let (line_tx, line_rx) = mpsc::channel();
    let out_reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line_tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let err_reader = std::thread::spawn(move || {
        let mut text = String::new();
        stderr.read_to_string(&mut text).unwrap();
        text
    });
    // The banner follows bind; no filesystem polling or startup sleep is needed.
    assert_eq!(
        line_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "lattice daemon · scripted @ "
    );
    assert_eq!(
        line_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        format!("socket: {}", socket.display())
    );
    assert_eq!(
        line_rx.recv_timeout(Duration::from_secs(30)).unwrap(),
        "connect a client (clients/ink, or any language); Ctrl-C to stop"
    );
    let mut client = Client::connect(&socket);
    let valid = ["valid-Name_7".to_string(), "a".repeat(128)];
    let invalid = [
        "".to_string(),
        "../escaped".to_string(),
        "非文件名".to_string(),
        "b".repeat(129),
    ];
    for name in valid.iter().chain(invalid.iter()) {
        client.send(ClientMessage::Attach {
            stream: name.clone(),
            template: None,
            derive_from: None,
            capabilities: vec!["authorize".into()],
        });
        let attached = client.until(
            |message| matches!(message, ServerMessage::Attached {stream, ..} if stream == name),
        );
        let ServerMessage::Attached { replay, .. } = attached.last().unwrap() else {
            unreachable!()
        };
        let opened = replay
            .iter()
            .find(|event| event.event_type == ce::STREAM_OPENED)
            .unwrap();
        assert_eq!(opened.payload["host"], "daemon");
        assert_eq!(opened.payload["model"], "scripted");
        assert_eq!(opened.payload["adapter"], "scripted");
        client.send(ClientMessage::SendText {
            stream: name.clone(),
            text: secret.into(),
        });
        // Opening the interface can settle before this input is consumed.
        // Only a quiescent notification after this stream's reply ends our turn.
        let mut replied = false;
        let messages = client.until(|message| {
            if matches!(message, ServerMessage::Appended {stream, event}
                if stream == name && event.event_type == ce::OUTPUT_REPLY)
            {
                replied = true;
            }
            replied && matches!(message, ServerMessage::Quiescent {stream} if stream == name)
        });
        let events: Vec<&EventEnvelope> = messages
            .iter()
            .filter_map(|message| match message {
                ServerMessage::Appended { stream, event } if stream == name => Some(event.as_ref()),
                _ => None,
            })
            .collect();
        let user = events
            .iter()
            .find(|event| event.event_type == ce::USER_MESSAGE)
            .unwrap();
        assert!(!user.payload.to_string().contains(secret));
        assert!(user.payload["text"]
            .as_str()
            .unwrap()
            .contains("[redacted]"));
        assert!(events
            .iter()
            .any(|event| event.event_type == ce::OUTPUT_REPLY
                && event.payload["text"] == "scripted reply 1"));
    }
    let mut saved: Vec<_> = std::fs::read_dir(settings.join("streams"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    saved.sort();
    let mut expected: Vec<_> = valid.iter().map(|name| format!("{name}.ledger")).collect();
    expected.sort();
    assert_eq!(
        saved, expected,
        "invalid stream names must remain memory-only"
    );
    assert!(!settings.join("escaped.ledger").exists());

    // Complete round trips precede shutdown; keep the client attached while all
    // streams stop. Pipe EOF is the completion signal, not a fixed delay.
    assert_eq!(
        unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let mut remaining = Vec::new();
    loop {
        match line_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => remaining.push(line),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("daemon did not stop after SIGINT"),
        }
    }
    assert!(child.0.wait().unwrap().success());
    out_reader.join().unwrap();
    let errors = err_reader.join().unwrap();
    assert_eq!(remaining, ["", "stopped"]);
    assert_eq!(
        errors.matches("kept in memory only").count(),
        invalid.len(),
        "{errors}"
    );
    assert!(!errors.contains(secret));
    assert!(!socket.exists());
}
