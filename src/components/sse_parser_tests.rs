use super::{SseError, SseParser};
use serde_json::{json, Value};

fn decode(wire: &[u8], width: usize) -> (Vec<Result<Value, SseError>>, Result<(), SseError>) {
    let mut parser = SseParser::default();
    let mut values = Vec::new();
    for chunk in wire.chunks(width) {
        values.extend(parser.push(chunk));
    }
    (values, parser.finish())
}

#[test]
fn valid_lines_and_utf8_are_independent_of_chunk_width() {
    let wire = concat!(
        ": keepalive\r\nevent: future\r\nid: 7\r\nretry: 1000\r\n\r\n",
        "data: {\"text\":\"你好，世界\"}\r\n\r\n",
        " data: [DONE] \n",
        "data: {\"type\":\"future-event\"}\n",
    );
    for width in 1..=wire.len() {
        assert_eq!(
            decode(wire.as_bytes(), width),
            (
                vec![
                    Ok(json!({"text":"你好，世界"})),
                    Ok(json!({"type":"future-event"}))
                ],
                Ok(())
            ),
            "chunk width {width}"
        );
    }
}

#[test]
fn a_bad_line_keeps_its_valid_prefix_but_never_accepts_later_values() {
    for bad in ["broken", "{", "", "[DONE]extra", "[done]"] {
        let wire = format!(
            "data: {{\"text\":\"kept\"}}\n\ndata: {bad}\n\ndata: {{\"tool\":\"unsafe\"}}\n\n"
        );
        for width in 1..=wire.len() {
            assert_eq!(
                decode(wire.as_bytes(), width),
                (
                    vec![Ok(json!({"text":"kept"})), Err(SseError::InvalidJson)],
                    Err(SseError::InvalidJson)
                ),
                "chunk width {width}, bad line {bad:?}"
            );
        }
    }
}

#[test]
fn invalid_utf8_is_explicit_in_data_or_metadata() {
    for wire in [
        b"data: {\"text\":\"\xff\"}\n".as_slice(),
        b": \xff\n",
        b"event: \xff\n",
    ] {
        for width in 1..=wire.len() {
            assert_eq!(
                decode(wire, width),
                (vec![Err(SseError::InvalidUtf8)], Err(SseError::InvalidUtf8))
            );
        }
    }
}

#[test]
fn eof_never_promotes_a_partial_or_unterminated_line() {
    for wire in [
        b"data: {".as_slice(),
        b"data: {}",
        b"data: [DONE]",
        b"data: \xe4\xbd",
        b": unfinished",
        b"\r",
    ] {
        for width in 1..=wire.len() {
            assert_eq!(decode(wire, width), (vec![], Err(SseError::UnfinishedLine)));
        }
    }
    assert_eq!(decode(b"", 1), (vec![], Ok(())));
    assert_eq!(decode(b": done\n\r\n", 1), (vec![], Ok(())));
}

#[test]
fn multiline_json_is_rejected_not_silently_dropped_or_joined() {
    let wire = b"data: {\n data: \"a\": 1}\n\n";
    assert_eq!(
        decode(wire, 1),
        (vec![Err(SseError::InvalidJson)], Err(SseError::InvalidJson))
    );
    // Two complete data lines retain the existing two-value behavior; this
    // parser does not introduce a new blank-line event boundary requirement.
    assert_eq!(
        decode(b"data: {}\ndata: {}\n", 1),
        (vec![Ok(json!({})), Ok(json!({}))], Ok(()))
    );
}

fn until_terminal(wire: &[u8], width: usize) -> Result<Value, SseError> {
    let mut parser = SseParser::default();
    for chunk in wire.chunks(width) {
        for value in parser.push(chunk) {
            let value = value?;
            if value["type"] == "response.completed" {
                return Ok(value);
            }
        }
    }
    panic!("fixture must contain a terminal event or a framing error");
}

#[test]
fn terminal_vs_damage_order_does_not_depend_on_network_chunks() {
    let terminal = "data: {\"type\":\"response.completed\"}\n\n";
    let after = format!("{terminal}data: broken\n\ndata: {{");
    let before = format!("data: broken\n\n{terminal}");
    for width in 1..=after.len() {
        assert_eq!(
            until_terminal(after.as_bytes(), width),
            Ok(json!({"type":"response.completed"}))
        );
        assert_eq!(
            until_terminal(before.as_bytes(), width),
            Err(SseError::InvalidJson)
        );
    }
}

#[test]
fn push_does_not_parse_past_the_consumed_value() {
    let mut parser = SseParser::default();
    assert_eq!(
        parser.push(b"data: {}\ndata: broken\n").next(),
        Some(Ok(json!({})))
    );
    assert!(
        parser.error.is_none(),
        "later bytes have not been interpreted yet"
    );
    assert_eq!(parser.push(b"").next(), Some(Err(SseError::InvalidJson)));
    assert_eq!(parser.push(b"data: {}\n").next(), None);
    assert_eq!(parser.finish(), Err(SseError::InvalidJson));
}

#[test]
fn large_valid_data_is_not_rejected_by_an_invented_frame_cap() {
    let value = json!({"image": "x".repeat(2 * 1024 * 1024)});
    let wire = format!("data: {value}\n\n");
    assert_eq!(
        decode(wire.as_bytes(), 16 * 1024),
        (vec![Ok(value)], Ok(()))
    );
}
