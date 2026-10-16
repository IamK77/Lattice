use super::*;

#[test]
fn request_error_survives_resize_failure_without_retry_or_configuration_side_effects() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (started, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
        }
        assert!(request.starts_with(b"GET /models "));
        started.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(10)).unwrap();
        let body = "SYNTHETIC_PRIVATE_RESPONSE_BODY";
        write!(
            stream,
            "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        listener
    });
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::start(home.path());
    terminal.wait("Choose how to configure the model");
    terminal.send(b"\x1b[B\r");
    terminal.wait("API format");
    terminal.send(b"\r");
    terminal.wait("API base URL");
    terminal.send(format!("http://{address}\r").as_bytes());
    terminal.wait("How should Lattice obtain the key?");
    terminal.send(b"\r");
    terminal.wait("API key");
    terminal.send(b"SYNTHETIC_RESIZE_KEY\r");
    terminal.wait_prompt("Choose a model");
    terminal.send(b"\r");
    terminal.wait("Fetching models");
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let size = libc::winsize {
        ws_row: 10,
        ws_col: 30,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe { libc::ioctl(terminal.master.as_raw_fd(), libc::TIOCSWINSZ, &size) },
        0
    );
    terminal.parser.screen_mut().set_size(10, 30);
    release.send(()).unwrap();
    // Wait for the terminal failure first: the old implementation does exit,
    // but silently loses the queued network error on that path.
    terminal.wait("Setup needs a terminal of at least 40 columns and 14 rows");
    assert!(!terminal.child.wait().unwrap().success());
    terminal.assert_restored();
    let output = String::from_utf8_lossy(&terminal.bytes);
    let restored = output
        .rfind("\u{1b}[?1049l")
        .expect("the primary screen must be restored");
    assert!(
        output[restored..].contains("HTTP 401"),
        "the request diagnostic was lost: {output}"
    );
    assert!(!output.contains("SYNTHETIC_RESIZE_KEY"));
    assert!(!output.contains("SYNTHETIC_PRIVATE_RESPONSE_BODY"));
    assert!(!terminal.parser.screen().alternate_screen());
    assert!(!home.path().join(".lattice/models.json").exists());
    assert!(!home.path().join(".lattice/ledgers").exists());
    let records: Vec<_> = std::fs::read_dir(home.path().join(".lattice/setup-tests"))
        .unwrap()
        .collect();
    assert_eq!(
        records.len(),
        1,
        "fallback must not perform another request"
    );
    let journal = std::fs::read_to_string(records[0].as_ref().unwrap().path()).unwrap();
    let entries: Vec<serde_json::Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["phase"], "requested");
    assert_eq!(entries[1]["phase"], "completed");
    assert_eq!(entries[1]["ok"], false);
    assert!(entries[1]["detail"].as_str().unwrap().contains("HTTP 401"));
    assert!(!journal.contains("SYNTHETIC_RESIZE_KEY"));
    assert!(!journal.contains("SYNTHETIC_PRIVATE_RESPONSE_BODY"));
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}
