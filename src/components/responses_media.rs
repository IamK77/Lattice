//! Generated images are durable documents, never base64 ledger lines.
use crate::contracts::document::{store_bytes, DocRef};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::path::Path;

use super::media_document::{read_png, validate_png, MAX_IMAGE_BYTES};

/// Persist final PNGs before publishing the model result. The request selects
/// PNG explicitly; other encodings must not be mislabeled with a PNG suffix.
pub fn persist(response: &mut Value, directory: Option<&Path>) -> Result<(), String> {
    let Some(output) = response["output"].as_array_mut() else {
        return Ok(());
    };
    for item in output {
        if item["type"] != "image_generation_call" {
            continue;
        }
        if item["status"] != "completed" {
            return Err("generated image is incomplete".into());
        }
        let encoded = item["result"]
            .as_str()
            .ok_or("generated image has no data")?;
        if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("generated image exceeds 20 MiB".into());
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "invalid generated image base64")?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err("generated image exceeds 20 MiB".into());
        }
        validate_png(&bytes)?;
        let directory = directory.ok_or("generated images require a persistent ledger")?;
        let reference = store_bytes(directory, &bytes, "png")?;
        read_png(&reference, directory)?;
        item.as_object_mut()
            .ok_or("image item must be an object")?
            .remove("result");
        item["image"] =
            json!({"file":reference.file,"bytes":reference.bytes,"mediaType":"image/png"});
        item["savedPath"] = json!(directory.join(&reference.file).display().to_string());
    }
    Ok(())
}

/// A stateless edit uses the locally saved pixels, not an expiring provider ID.
/// Keep the generation metadata in the ledger; send a normal image input here.
pub fn restore_input(items: Vec<Value>, directory: Option<&Path>) -> Result<Vec<Value>, String> {
    items.into_iter().map(|mut item| {
        if item["type"] == "function_call_output" {
            if let Some(result) = item["output"].as_str().and_then(|text| serde_json::from_str::<Value>(text).ok()) {
                let images = super::media_document::tool_pngs(&result, directory)?;
                if !images.is_empty() {
                    let mut content = vec![json!({"type":"input_text","text":item["output"]})];
                    for data in images {
                        content.push(json!({"type":"input_image","image_url":format!("data:image/png;base64,{data}"),"detail":"original"}));
                    }
                    item["output"] = json!(content);
                }
            }
            return Ok(item);
        }
        if item["type"] != "image_generation_call" { return Ok(item); }
        let reference = DocRef::of(&item["image"]).ok_or("generated image reference is invalid")?;
        let bytes = read_png(&reference, directory.ok_or("image history requires its ledger directory")?)?;
        if bytes.len() > MAX_IMAGE_BYTES { return Err("image history exceeds 20 MiB".into()); }
        validate_png(&bytes)?;
        Ok(json!({"type":"message","role":"user","content":[
            {"type":"input_text","text":"Image generated earlier in this conversation, retained for reference or editing."},
            {"type":"input_image","image_url":format!("data:image/png;base64,{}", STANDARD.encode(bytes))}
        ]}))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn picture() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255, 0, 0])
                .unwrap();
        }
        bytes
    }
    #[test]
    fn generated_pixels_live_beside_the_ledger_and_survive_stateless_restore() {
        let directory = tempfile::tempdir().unwrap();
        let pixels = picture();
        let mut response = json!({"output":[{"type":"image_generation_call","status":"completed","id":"ig_one","result":STANDARD.encode(&pixels)}]});
        persist(&mut response, Some(directory.path())).unwrap();
        assert!(response["output"][0].get("result").is_none());
        let serialized = serde_json::to_vec(&response).unwrap();
        let reopened: Value = serde_json::from_slice(&serialized).unwrap();
        let restored = restore_input(
            reopened["output"].as_array().unwrap().clone(),
            Some(directory.path()),
        )
        .unwrap();
        assert_eq!(
            restored[0]["content"][1]["image_url"],
            format!("data:image/png;base64,{}", STANDARD.encode(pixels))
        );
        assert!(!serde_json::to_string(&response).unwrap().contains("base64"));
        let reference = DocRef::of(&response["output"][0]["image"]).unwrap();
        std::fs::remove_file(directory.path().join(reference.file)).unwrap();
        assert!(restore_input(
            response["output"].as_array().unwrap().clone(),
            Some(directory.path())
        )
        .is_err());
    }
    #[test]
    fn tool_screenshots_remain_tool_results_not_fabricated_user_messages() {
        let directory = tempfile::tempdir().unwrap();
        let reference = store_bytes(directory.path(), &picture(), "png").unwrap();
        let result = json!({"completedActions":1,"latticeImages":[{"file":reference.file,"mediaType":"image/png"}]}).to_string();
        let restored = restore_input(
            vec![json!({"type":"function_call_output","call_id":"call_browser","output":result})],
            Some(directory.path()),
        )
        .unwrap();
        assert_eq!(restored[0]["type"], "function_call_output");
        assert_eq!(restored[0]["call_id"], "call_browser");
        assert_eq!(restored[0]["output"][1]["type"], "input_image");
        assert!(restore_input(vec![json!({"type":"function_call_output","output":json!({"latticeImages":[{"file":"../secret"}]}).to_string()})], Some(directory.path())).is_err());
    }

    #[test]
    fn a_corrupt_existing_artifact_cannot_be_reported_as_a_new_success() {
        let dir = tempfile::tempdir().unwrap();
        let original = json!({"output":[{"type":"image_generation_call","status":"completed","result":STANDARD.encode(picture())}]});
        let mut saved = original.clone();
        persist(&mut saved, Some(dir.path())).unwrap();
        let reference = DocRef::of(&saved["output"][0]["image"]).unwrap();
        std::fs::write(dir.path().join(reference.file), b"corrupt").unwrap();
        assert!(persist(&mut original.clone(), Some(dir.path())).is_err());
    }

    #[test]
    fn ordinary_tool_business_data_named_images_is_not_an_attachment_contract() {
        let item = json!({"type":"function_call_output","call_id":"business","output":json!({"images":[{"url":"https://example.com/photo.jpg"}]}).to_string()});
        assert_eq!(restore_input(vec![item.clone()], None).unwrap(), vec![item]);
    }

    #[test]
    fn malformed_or_unfinished_images_are_not_successful_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        for (status, data) in [
            ("completed", "!".to_owned()),
            ("completed", STANDARD.encode(b"not a png")),
            ("in_progress", STANDARD.encode(picture())),
        ] {
            let mut response =
                json!({"output":[{"type":"image_generation_call","status":status,"result":data}]});
            assert!(persist(&mut response, Some(directory.path())).is_err());
        }
    }
}
