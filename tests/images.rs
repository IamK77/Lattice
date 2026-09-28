//! An image, from the ledger to each wire.
//!
//! Attaching a picture crosses every layer at once, so it is pinned in one
//! place: the ledger line stays a REFERENCE, the bytes sit in the directory
//! beside it, and each dialect spells the block its own provider's way. That
//! last part is the reason dialects exist, and the reason this is one test file
//! rather than two.

use base64::Engine;
use serde_json::json;

use lattice::core_events as ce;
use lattice::core_events::core_event_decls;
use lattice::{EventDraft, EventLog};

/// The two wires spell an attachment differently, and that difference is
/// the whole reason a dialect exists. Same event, same bytes, two shapes.
#[test]
fn an_attached_image_reaches_each_wire_in_its_own_shape() {
    use lattice::contracts::document::store_bytes;
    let dir = tempfile::tempdir().unwrap();
    let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let stored = store_bytes(dir.path(), &png, "png").unwrap();

    let mut log = EventLog::in_memory(core_event_decls(), "main");
    let said = log
        .append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({
                    "text": "what is wrong here?",
                    "images": [{
                        "file": stored.file,
                        "mediaType": "image/png",
                        "bytes": stored.bytes,
                        "name": "shot.png",
                    }],
                }),
            ),
            "ui",
        )
        .unwrap();
    let parts = [json!({"event": said.id})];
    let expected = base64::engine::general_purpose::STANDARD.encode(png);

    // Anthropic: a typed image block carrying its own source
    let m =
        lattice::components::anthropic_model::materialize(&parts, &log.reader(), Some(dir.path()))
            .unwrap();
    let content = m[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert_eq!(content[0]["source"]["data"], expected);
    assert_eq!(
        content[1]["text"], "what is wrong here?",
        "the picture comes before the question about it"
    );

    // OpenAI: a data URI under image_url
    let m = lattice::components::openai_model::materialize(&parts, &log.reader(), Some(dir.path()))
        .unwrap();
    let content = m[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "image_url");
    assert_eq!(
        content[0]["image_url"]["url"],
        format!("data:image/png;base64,{expected}")
    );
    assert_eq!(content[1]["text"], "what is wrong here?");
}

/// The ledger keeps a reference, never the bytes: it is one JSON object per
/// line, read line by line, and an inlined image would put megabytes of
/// base64 on a line every reader has to walk past.
#[test]
fn the_ledger_line_holds_a_reference_and_not_the_picture() {
    use lattice::contracts::document::store_bytes;
    let dir = tempfile::tempdir().unwrap();
    let png = vec![0x42u8; 4096];
    let stored = store_bytes(dir.path(), &png, "png").unwrap();

    let mut log = EventLog::in_memory(core_event_decls(), "main");
    let said = log
        .append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text": "see this", "images": [
                    {"file": stored.file, "mediaType": "image/png"}]}),
            ),
            "ui",
        )
        .unwrap();
    let line = serde_json::to_string(&said).unwrap();
    assert!(
        line.len() < 400,
        "the line stays small: {} bytes",
        line.len()
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    assert!(!line.contains(&encoded[..64]), "no image data on the line");
}

/// An attachment that cannot be read must not take the sentence down with
/// it — the picture is gone either way, and a refused call helps nobody.
#[test]
fn an_unreadable_attachment_is_skipped_and_the_text_still_goes() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::in_memory(core_event_decls(), "main");
    let said = log
        .append(
            EventDraft::new(
                ce::USER_MESSAGE,
                &[],
                json!({"text": "still ask this", "images": [
                    {"file": "deadbeef.png", "mediaType": "image/png"}]}),
            ),
            "ui",
        )
        .unwrap();
    let parts = [json!({"event": said.id})];
    for m in [
        lattice::components::anthropic_model::materialize(&parts, &log.reader(), Some(dir.path()))
            .unwrap(),
        lattice::components::openai_model::materialize(&parts, &log.reader(), Some(dir.path()))
            .unwrap(),
    ] {
        let rendered = serde_json::to_string(&m).unwrap();
        assert!(rendered.contains("still ask this"), "{rendered}");
        assert!(!rendered.contains("image"), "{rendered}");
    }
}

#[test]
fn tool_screenshots_reach_all_dialects_without_splitting_parallel_answers() {
    let dir = tempfile::tempdir().unwrap();
    let mut pixels = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut pixels, 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0, 32, 64])
            .unwrap();
    }
    let image = lattice::contracts::document::store_bytes(dir.path(), &pixels, "png").unwrap();
    let result =
        json!({"latticeImages":[{"file":image.file,"bytes":image.bytes,"mediaType":"image/png"}]});
    let mut log = EventLog::in_memory(core_event_decls(), "main");
    let mut parts = Vec::new();
    for call in ["first", "second"] {
        let event = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_COMPLETED,
                    &[],
                    json!({"call":call,"status":"ok","result":result}),
                ),
                "tool",
            )
            .unwrap();
        parts.push(json!({"event":event.id}));
    }
    let reader = log.reader();
    let chat =
        lattice::components::openai_model::materialize(&parts, &reader, Some(dir.path())).unwrap();
    assert_eq!(chat.len(), 3);
    assert_eq!(chat[0]["role"], "tool");
    assert_eq!(chat[1]["role"], "tool");
    assert_eq!(chat[2]["role"], "user");
    assert_eq!(
        chat[2]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["type"] == "image_url")
            .count(),
        2
    );
    let anthropic =
        lattice::components::anthropic_model::materialize(&parts, &reader, Some(dir.path()))
            .unwrap();
    assert_eq!(anthropic.len(), 1);
    for answer in anthropic[0]["content"].as_array().unwrap() {
        assert_eq!(answer["content"][1]["type"], "image");
    }
    let responses = lattice::components::responses_media::restore_input(
        vec![json!({"type":"function_call_output","output":result.to_string()})],
        Some(dir.path()),
    )
    .unwrap();
    assert_eq!(responses[0]["output"][1]["type"], "input_image");
    std::fs::write(dir.path().join(&image.file), b"replaced").unwrap();
    assert!(
        lattice::components::openai_model::materialize(&parts, &reader, Some(dir.path())).is_err()
    );
    assert!(
        lattice::components::anthropic_model::materialize(&parts, &reader, Some(dir.path()))
            .is_err()
    );
    assert!(lattice::components::responses_media::restore_input(
        vec![json!({"type":"function_call_output","output":result.to_string()})],
        Some(dir.path())
    )
    .is_err());
}
