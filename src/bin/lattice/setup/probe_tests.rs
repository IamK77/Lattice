use super::*;
use std::io::Read;

#[test]
fn probe_journals_never_append_to_an_existing_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("setup-tests.jsonl");
    let original = r#"{"models":{"keep":{}}}"#;
    std::fs::write(&config, original).unwrap();
    let wrong_location = HttpTest::new(config.clone());
    assert!(wrong_location.new_record().is_err());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
    let probe = HttpTest::new(dir.path().join("setup-tests"));
    let mut first = probe.new_record().unwrap();
    HttpTest::record(&mut first, &json!({"phase":"requested"})).unwrap();
    let _second = probe.new_record().unwrap();
    assert_eq!(
        std::fs::read_dir(dir.path().join("setup-tests"))
            .unwrap()
            .count(),
        2
    );
    assert_eq!(std::fs::read_to_string(config).unwrap(), original);
}

fn entry(base: String) -> Entry {
    Entry {
        id: "probe".into(),
        adapter: "openai".into(),
        model: "synthetic".into(),
        base_url: base,
        key_env: "LATTICE_SYNTHETIC_PROBE_KEY".into(),
        profile: None,
    }
}

#[test]
fn each_protocol_has_an_explicit_small_request_and_strict_success_shape() {
    let mut e = entry("https://example.invalid/v1".into());
    let (url, body) = request(&e).unwrap();
    assert_eq!(url, "https://example.invalid/v1/chat/completions");
    assert_eq!(body["max_tokens"], 32);
    assert!(body.get("tools").is_none());
    assert!(!valid_response("openai", &json!({"choices":[]})));
    assert!(valid_response(
        "openai",
        &json!({"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}]})
    ));
    assert!(!valid_response(
        "openai",
        &json!({"choices":[{"message":{"content":"O"},"finish_reason":"length"}]})
    ));
    e.adapter = "responses".into();
    assert_eq!(request(&e).unwrap().1["store"], false);
    assert!(!valid_response(
        "responses",
        &json!({"status":"incomplete","output":[]})
    ));
    e.adapter = "anthropic".into();
    e.base_url = "https://example.invalid".into();
    assert_eq!(
        request(&e).unwrap().0,
        "https://example.invalid/v1/messages"
    );
}

#[test]
fn failed_http_test_sends_once_and_journals_no_credential_or_response_body() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = stream.read(&mut buffer).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..header_end]);
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        assert!(String::from_utf8_lossy(&bytes).contains("Bearer SYNTHETIC_PROBE_SECRET"));
        let body = "upstream might echo SYNTHETIC_PROBE_SECRET";
        write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("setup-tests.jsonl");
    std::env::set_var("LATTICE_SYNTHETIC_PROBE_KEY", "SYNTHETIC_PROBE_SECRET");
    let mut probe = HttpTest::new(path.clone());
    let error = probe.test(&entry(format!("http://{address}"))).unwrap_err();
    assert!(error.contains("503"));
    let listener = server.join().unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let files: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    let journal = std::fs::read_to_string(&files[0]).unwrap();
    assert!(!journal.contains("SYNTHETIC_PROBE_SECRET"));
    assert!(!journal.contains("upstream might echo"));
    let records: Vec<Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["attempt"], records[1]["attempt"]);
    assert_eq!(records[1]["ok"], false);
}
