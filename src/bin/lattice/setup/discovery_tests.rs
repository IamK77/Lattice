use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
};

#[test]
fn metadata_preserves_unknown_and_distinguishes_input_from_total_context() {
    let bare = parse_page(&json!({"data":[{"id":"synthetic"}]}), false).unwrap();
    assert_eq!(bare[0].profile, json!({}));
    assert_eq!(bare[0].input_limit, None);
    let anthropic = parse_page(&json!({"data":[{"id":"synthetic","max_input_tokens":1000,"max_tokens":200,"capabilities":{"image_input":{"supported":true},"effort":{"supported":true,"low":{"supported":true},"high":{"supported":false},"max":{"supported":true}}}}]}), true).unwrap();
    assert_eq!(anthropic[0].input_limit, Some(1000));
    assert_eq!(
        anthropic[0].profile,
        json!({"maxOutputTokens":200,"acceptsImages":true,"effort":["low","max"]})
    );
    let deepseek = parse_page(&json!({"data":[{"id":"synthetic","context_window":4096,"max_output_tokens":1024,"input_modalities":["text","image"],"effort":{"supported_levels":["low","high","max"]}}]}), false).unwrap();
    assert_eq!(
        deepseek[0].profile,
        json!({"contextWindow":4096,"maxOutputTokens":1024,"acceptsImages":true,"effort":["low","high","max"]})
    );
    let unknown = parse_page(
        &json!({"data":[{"id":"synthetic","max_tokens":0,"capabilities":null}]}),
        true,
    )
    .unwrap();
    assert_eq!(unknown[0].profile, json!({}));
}

#[test]
fn invalid_lists_do_not_become_empty_successes() {
    for value in [
        json!({"error":"no"}),
        json!({"data":{}}),
        json!({"data":[{}]}),
        json!({"data":[{"id":"bad\nidentifier"}]}),
    ] {
        assert!(parse_page(&value, false).is_err());
    }
    assert!(parse_page(&json!({"data":[]}), false).unwrap().is_empty());
}

#[test]
fn discovery_routes_follow_service_not_just_generation_format() {
    for (adapter, base, expected, anthropic) in [
        (
            "responses",
            "https://api.openai.com/v1",
            "https://api.openai.com/v1/models",
            false,
        ),
        (
            "anthropic",
            "https://api.anthropic.com",
            "https://api.anthropic.com/v1/models",
            true,
        ),
        (
            "anthropic",
            "https://api.deepseek.com/anthropic",
            "https://api.deepseek.com/models",
            false,
        ),
        (
            "anthropic",
            "https://example.invalid/proxy",
            "https://example.invalid/proxy/v1/models",
            true,
        ),
    ] {
        let (url, kind) = endpoint(&json!({"adapter":adapter,"baseUrl":base})).unwrap();
        assert_eq!(url.as_str(), expected);
        assert_eq!(kind, anthropic);
    }
}

fn serve(responses: Vec<String>) -> (String, mpsc::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut requests = vec![];
        for response in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = vec![];
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                if socket.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                bytes.push(byte[0]);
            }
            if bytes.is_empty() {
                break;
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let _ = socket.write_all(response.as_bytes());
        }
        tx.send(requests).unwrap();
    });
    (format!("http://{address}"), rx)
}
fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}
fn run(base: &str, adapter: &str) -> Result<Vec<Model>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(fetch(
        &json!({"adapter":adapter,"baseUrl":base}),
        "SYNTHETIC_DISCOVERY_KEY",
    ));
    // Unblock a fixture waiting for a page the implementation failed to request.
    let address = base.trim_start_matches("http://");
    let _ = std::net::TcpStream::connect(address);
    result
}

#[test]
fn anthropic_discovery_follows_bounded_pagination_and_uses_correct_auth() {
    let (base, requests) = serve(vec![
        response(
            "200 OK",
            "",
            r#"{"data":[{"id":"z"}],"has_more":true,"last_id":"z"}"#,
        ),
        response(
            "200 OK",
            "",
            r#"{"data":[{"id":"a"},{"id":"z"}],"has_more":false}"#,
        ),
    ]);
    let models = run(&base, "anthropic").unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["a", "z"]
    );
    let requests = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /v1/models?limit=1000 "));
    assert!(requests[1].starts_with("GET /v1/models?limit=1000&after_id=z "));
    assert_eq!(
        header(&requests[0], "x-api-key"),
        Some("SYNTHETIC_DISCOVERY_KEY")
    );
    assert_eq!(
        header(&requests[0], "anthropic-version"),
        Some("2023-06-01")
    );
    assert_eq!(header(&requests[0], "authorization"), None);
}

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

#[test]
fn errors_and_redirects_are_not_retried_or_echoed() {
    for (status, headers, body) in [
        ("503 Unavailable", "", "SYNTHETIC_DISCOVERY_KEY"),
        ("302 Found", "Location: http://127.0.0.1:1/stolen\r\n", ""),
        ("200 OK", "", "SYNTHETIC_DISCOVERY_KEY"),
    ] {
        let (base, requests) = serve(vec![response(status, headers, body)]);
        let error = run(&base, "openai").unwrap_err();
        assert!(!error.contains("SYNTHETIC_DISCOVERY_KEY"));
        let requests = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /models "));
        assert_eq!(
            header(&requests[0], "authorization"),
            Some("Bearer SYNTHETIC_DISCOVERY_KEY")
        );
        if status.starts_with("302") {
            assert!(error.contains("302"));
        }
    }
}

#[test]
fn response_size_and_nonadvancing_cursors_are_rejected() {
    let body = " ".repeat(1_048_577);
    let (base, requests) = serve(vec![response("200 OK", "", &body)]);
    assert!(run(&base, "openai").unwrap_err().contains("size limit"));
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(5)).unwrap().len(),
        1
    );
    let page = response(
        "200 OK",
        "",
        r#"{"data":[{"id":"same"}],"has_more":true,"last_id":"same"}"#,
    );
    let (base, requests) = serve(vec![page.clone(), page]);
    assert!(run(&base, "anthropic")
        .unwrap_err()
        .contains("did not advance"));
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(5)).unwrap().len(),
        2
    );
}
