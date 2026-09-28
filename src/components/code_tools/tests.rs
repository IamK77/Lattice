use super::*;

fn fixture(dir: &Path, runtime: &tokio::runtime::Runtime, mode: &str) -> Peer {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/components/code_tools/fake_server.py");
    let command = vec![
        "python3".into(),
        script.to_str().unwrap().into(),
        dir.join("protocol trace.jsonl").to_str().unwrap().into(),
    ];
    let _entered = runtime.enter();
    Peer::spawn(
        &command,
        dir,
        &json!({"initializationOptions":{"mode":mode}}),
        dir,
    )
    .unwrap()
}

#[test]
fn an_invalid_rpc_version_is_rejected_before_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut peer = fixture(dir.path(), &runtime, "bad-version");
    let result = runtime.block_on(async {
        let result = peer.initialize().await;
        peer.stop().await;
        result
    });
    assert!(result.unwrap_err().contains("JSON-RPC 2.0"));
}

#[test]
fn malformed_or_excessive_outlines_are_not_silently_accepted() {
    let source = Source::fixture("x");
    let range = json!({"start":{"line":0,"character":0},"end":{"line":0,"character":1}});
    for response in [
        json!({"unexpected":true}),
        json!([{"name":"x","location":{"uri":"file:///another.rs","range":range}}]),
        json!([{"name":"x".repeat(4097),"range":range}]),
        json!([{"name":"x","range":range,"children":false}]),
    ] {
        assert!(symbols(&source, response, "symbols", &json!({})).is_err());
    }
}

#[test]
fn navigation_arguments_are_validated_before_any_server_is_needed() {
    assert_eq!(
        validate_arguments(&json!({"path":"a.rs"})).unwrap(),
        "symbols"
    );
    for args in [
        json!({}),
        json!({"path":"a.rs","action":"read"}),
        json!({"path":"a.rs","action":"definition","line":0,"column":1}),
        json!({"path":"a.rs","limit":101}),
        json!({"path":"a.rs","expectedVersion":false}),
    ] {
        assert!(validate_arguments(&args).is_err());
    }
}

#[test]
fn locations_sort_by_file_and_numeric_position_not_json_text() {
    let rows = location_rows(json!([
        {"uri":"file:///z","range":{"start":{"line":2,"character":0}}},
        {"uri":"file:///a","range":{"start":{"line":12,"character":0}}},
        {"uri":"file:///a","range":{"start":{"line":2,"character":0}}}
    ]));
    assert_eq!(rows[0]["uri"], "file:///a");
    assert_eq!(rows[0]["range"]["start"]["line"], 2);
    assert_eq!(rows[2]["uri"], "file:///z");
}

#[test]
fn escaped_symbol_content_keeps_a_nonempty_bounded_preview() {
    let source = Source::fixture(&"\0".repeat(48 * 1024));
    let response = json!([{"name":"escaped","kind":12,"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":48*1024}}}]);
    let result = symbols(&source, response, "read", &json!({"symbol":"escaped"})).unwrap();
    assert!(result.to_string().len() <= 65536);
    assert!(!result["content"].as_str().unwrap().is_empty());
    assert_eq!(result["truncated"], true);
}

#[test]
fn real_pipes_synchronize_changes_refuse_edits_and_read_exact_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rs");
    std::fs::write(&path, "fn sample() { /* 中文😀 */ }\n").unwrap();
    let source = Source::load(&path).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut peer = fixture(dir.path(), &runtime, "normal");
    runtime.block_on(async {
        let result = query(
            &mut peer,
            &source,
            "rust",
            "read",
            &json!({"symbol":"sample"}),
        )
        .await
        .unwrap();
        assert_eq!(result["content"], source.text);
        assert_eq!(result["fileVersion"], source.version);
        std::fs::write(&path, "fn sample() { /* updated */ }\n").unwrap();
        let updated = Source::load(&path).unwrap();
        let result = query(
            &mut peer,
            &updated,
            "rust",
            "read",
            &json!({"symbol":"sample"}),
        )
        .await
        .unwrap();
        assert_eq!(result["content"], updated.text);
        let result = query(
            &mut peer,
            &updated,
            "rust",
            "definition",
            &json!({"line":1,"column":4}),
        )
        .await
        .unwrap();
        assert_eq!(
            result["locations"][0]["observedFileVersion"],
            updated.version
        );
        assert_eq!(result["indexFreshness"], "not-guaranteed");
        assert_eq!(result["locations"][0]["text"], "f");
        assert_eq!(
            result["locations"][0]["read"],
            json!({"path":updated.path,"from":1,"fromByte":0,"expectedVersion":updated.version})
        );
        let other_path = dir.path().join("b.rs");
        std::fs::write(&other_path, "fn sample() {}\n").unwrap();
        let other = Source::load(&other_path).unwrap();
        query(&mut peer, &other, "rust", "symbols", &json!({}))
            .await
            .unwrap();
        assert_eq!(peer.documents.len(), 1);
        assert!(peer.documents.contains_key(&other.uri().unwrap()));
        peer.stop().await;
    });
    let trace = std::fs::read_to_string(dir.path().join("protocol trace.jsonl")).unwrap();
    let messages: Vec<Value> = trace
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["method"] == "initialize")
            .count(),
        1
    );
    let change = messages
        .iter()
        .find(|m| m["method"] == "textDocument/didChange")
        .unwrap();
    assert_eq!(change["params"]["textDocument"]["version"], 2);
    assert!(messages
        .iter()
        .any(|m| m["id"] == "edit-probe" && m["result"]["applied"] == false));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "fn sample() { /* updated */ }\n"
    );
}

#[test]
fn changed_during_query_is_not_given_a_fresh_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.rs");
    std::fs::write(&path, "fn sample() {}\n").unwrap();
    let source = Source::load(&path).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut peer = fixture(dir.path(), &runtime, "mutate");
    runtime.block_on(async {
        let error = query(&mut peer, &source, "rust", "symbols", &json!({}))
            .await
            .unwrap_err();
        assert!(error.contains("source changed"), "{error}");
        peer.stop().await;
    });
}

#[test]
fn outline_paging_is_bounded_even_for_maximum_offsets() {
    let source = Source::fixture("x");
    let rows = json!([{"name":"x","kind":12,"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]);
    let result = symbols(&source, rows, "symbols", &json!({"offset":u64::MAX})).unwrap();
    assert_eq!(result["symbols"], json!([]));
    assert_eq!(result["more"], false);
}

#[test]
fn approved_languages_have_explicit_ids_not_text_search_fallbacks() {
    for (file, id) in [
        ("a.ts", "typescript"),
        ("a.js", "javascript"),
        ("a.py", "python"),
        ("a.java", "java"),
        ("a.cs", "csharp"),
        ("a.go", "go"),
        ("a.rs", "rust"),
        ("a.c", "c"),
        ("a.cpp", "cpp"),
        ("a.php", "php"),
        ("a.rb", "ruby"),
        ("a.swift", "swift"),
        ("a.kt", "kotlin"),
    ] {
        assert_eq!(language(Path::new(file)), Some(id));
    }
    assert_eq!(language(Path::new("unknown.xyz")), None);
    assert!(manifest().capabilities.unwrap().executes);
}

#[test]
fn serves_language_server_requests_between_tool_calls() {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/components/code_tools/fake_server.py");
    let command = vec![
        "python3".into(),
        script.to_str().unwrap().into(),
        dir.path().join("trace").to_str().unwrap().into(),
    ];
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut peer = {
        let _entered = runtime.enter();
        Peer::spawn(&command, dir.path(), &json!({"initializationOptions":{"mode":"idle","port":listener.local_addr().unwrap().port()}}),dir.path()).unwrap()
    };
    runtime.block_on(peer.initialize()).unwrap();
    // initialize only completes after the fixture connected. No polling or
    // sleep coordinates this check; the socket acknowledges an idle reply.
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut acknowledgement = [0; 4];
    let received = socket.read_exact(&mut acknowledgement);
    runtime.block_on(peer.stop());
    received.unwrap();
    assert_eq!(&acknowledgement, b"done");
}

#[test]
#[cfg(unix)]
fn stopping_an_exited_leader_also_closes_its_descendants() {
    use std::io::{BufRead, Read};
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/components/code_tools/fake_server.py");
    let command = vec![
        "python3".into(),
        script.to_str().unwrap().into(),
        dir.path().join("trace").to_str().unwrap().into(),
    ];
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut peer = {
        let _entered = runtime.enter();
        Peer::spawn(&command, dir.path(), &json!({"initializationOptions":{"mode":"exit-with-child","port":listener.local_addr().unwrap().port()}}), dir.path()).unwrap()
    };
    runtime.block_on(peer.initialize()).unwrap();
    let (child, _) = listener.accept().unwrap();
    let (mut leader, _) = listener.accept().unwrap();
    child
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    leader
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut child = std::io::BufReader::new(child);
    let mut pid = String::new();
    child.read_line(&mut pid).unwrap();
    let pid: i32 = pid.trim().parse().unwrap();
    assert!(pid > 0);
    let mut byte = [0];
    assert_eq!(leader.read(&mut byte).unwrap(), 0);
    runtime.block_on(peer.stop());
    let closed = child.read(&mut byte);
    if closed.is_err() {
        // A deliberately broken cleanup must not leave our fixture orphaned.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    assert_eq!(closed.unwrap(), 0);
}

#[test]
#[ignore = "requires an installed rust-analyzer; run the built test binary without cargo"]
fn installed_rust_analyzer_reads_a_real_function() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loose.rs");
    let body = "fn sample() { let _ = \"😀\"; }";
    std::fs::write(&path, format!("{body}\n")).unwrap();
    let source = Source::load(&path).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut peer = {
        let _entered = runtime.enter();
        Peer::spawn(&["rust-analyzer".into()], dir.path(), &json!({"initializationOptions":{
            "cargo":{"buildScripts":{"enable":false}},"checkOnSave":false,"procMacro":{"enable":false}
        }}), dir.path()).unwrap()
    };
    let result = runtime.block_on(async {
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            query(
                &mut peer,
                &source,
                "rust",
                "read",
                &json!({"symbol":"sample"}),
            ),
        )
        .await;
        peer.stop().await;
        result
    });
    let log = std::fs::read_to_string(&peer.log).unwrap_or_default();
    let result = result
        .expect("language server deadline")
        .unwrap_or_else(|error| panic!("{error}; {log}"));
    assert_eq!(result["content"], body);
    assert_eq!(result["fileVersion"], source.version);
}
