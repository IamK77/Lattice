use super::*;
use lattice::components::responses_model;

fn sse(values: Vec<Value>) -> String {
    values
        .into_iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect()
}

fn check_streaming_usage(name: &str) {
    ensure_key();
    for purpose in [None, Some("test.progress")] {
        let (head, tail) = if name == openai_model::NAME {
            (
                sse(vec![json!({"choices":[{"delta":{"content":"abcdefgh"}}]})]),
                sse(vec![
                    json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
                    json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":2}}),
                ]),
            )
        } else if name == anthropic_model::NAME {
            (
                sse(vec![
                    json!({"type":"message_start","message":{"usage":{"input_tokens":12,"cache_read_input_tokens":3}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"abcdefgh"}}),
                ]),
                sse(vec![
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                    json!({"type":"message_stop"}),
                ]),
            )
        } else {
            (
                sse(vec![
                    json!({"type":"response.output_text.delta","delta":"abcdefgh"}),
                ]),
                sse(vec![
                    json!({"type":"response.completed","response":{"status":"completed",
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"abcdefgh"}]}],
                "usage":{"input_tokens":12,"output_tokens":2}}}),
                ]),
            )
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (received, wait_for_chunk) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_http_request(&mut stream);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{head}", head.len() + tail.len()).unwrap();
            stream.flush().unwrap();
            // The terminal notification must be observed before the server sends
            // completion. No timer/sleep decides whether streaming is working.
            wait_for_chunk
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            stream.write_all(tail.as_bytes()).unwrap();
        });
        let notes = Arc::new(Mutex::new(Vec::new()));
        let candidate = transport_candidates()
            .into_iter()
            .find(|c| c.manifest.name == name)
            .unwrap();
        let result = run_call_recorded(
            candidate,
            &url,
            None,
            None,
            purpose,
            KernelOptions::default(),
            Some(NoticeProbe {
                notes: Arc::clone(&notes),
                chunk_received: received,
            }),
        );
        server.join().unwrap();
        assert_eq!(result["status"], "ok", "{name}: {result}");
        let notes = notes.lock().unwrap();
        assert!(notes.iter().any(|note| note["chunk"] == "abcdefgh"));
        let usage: Vec<_> = notes.iter().filter_map(|note| note.get("usage")).collect();
        assert_eq!(
            usage.last().copied(),
            Some(&result["usage"]),
            "{name}: {notes:?}"
        );
        if name == anthropic_model::NAME {
            assert_eq!(usage.len(), 2);
            assert_eq!(usage[0]["input_tokens"], 12);
            assert_eq!(usage[1]["cache_read_input_tokens"], 3);
            assert_eq!(usage[1]["output_tokens"], 2);
        }
    }
}

#[test]
fn streaming_usage_openai_is_tagged_and_transient() {
    check_streaming_usage(openai_model::NAME);
}
#[test]
fn streaming_usage_anthropic_merges_snapshots_and_preserves_purpose() {
    check_streaming_usage(anthropic_model::NAME);
}
#[test]
fn streaming_usage_responses_is_tagged_and_transient() {
    check_streaming_usage(responses_model::NAME);
}
