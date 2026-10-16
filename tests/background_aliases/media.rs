use super::*;

#[test]
fn background_reference_annotation_preserves_tool_images_on_all_three_wires() {
    use base64::Engine;
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
    let result = json!({
        "job":"long-call-id", "background":true,
        "latticeImages":[{"file":image.file,"bytes":image.bytes,"mediaType":"image/png"}]
    });
    let mut log = EventLog::in_memory(ce::core_event_decls(), "owner-stream");
    let request = append(
        &mut log,
        ce::TOOL_EXEC_STARTED,
        &[],
        json!({
            "call":"start","tool":"Run","arguments":{"command":"fixture","background":true}
        }),
    );
    let receipt = append(
        &mut log,
        ce::TOOL_EXEC_COMPLETED,
        &[&request.id],
        json!({
            "call":"start","status":"ok","result":result
        }),
    );
    let reader = log.reader();
    let projection = Projection::recover(&reader, reader.snapshot_end()).unwrap();
    let identity = identity_for_event(&reader, &receipt).unwrap().unwrap();
    let reference = format!(
        "Run {}; stream=\"owner-stream\"",
        projection.alias(&identity).unwrap()
    );
    let expected = base64::engine::general_purpose::STANDARD.encode(&pixels);
    let parts = [json!({"event":receipt.id})];
    let chat = openai_model::materialize(&parts, &reader, Some(dir.path())).unwrap();
    assert_eq!(
        chat.len(),
        2,
        "the annotated receipt must still deliver its pixels"
    );
    let rendered: Value = serde_json::from_str(chat[0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(rendered["_backgroundReference"], reference);
    assert_eq!(rendered["job"], "long-call-id");
    assert_eq!(rendered["latticeImages"], result["latticeImages"]);
    assert_eq!(
        chat[1]["content"][1]["image_url"]["url"],
        format!("data:image/png;base64,{expected}")
    );

    let anthropic = anthropic_model::materialize(&parts, &reader, Some(dir.path())).unwrap();
    let content = &anthropic[0]["content"][0]["content"];
    assert_eq!(content[0]["text"], chat[0]["content"]);
    assert_eq!(content[1]["source"]["data"], expected);

    let responses = responses(&parts, &reader, Some(dir.path()));
    assert_eq!(responses[0]["output"][0]["text"], chat[0]["content"]);
    assert_eq!(responses[0]["output"][1]["type"], "input_image");
    assert_eq!(
        responses[0]["output"][1]["image_url"],
        format!("data:image/png;base64,{expected}")
    );
    assert_eq!(
        reader.get(&receipt.id).unwrap().unwrap().payload,
        receipt.payload
    );
}
