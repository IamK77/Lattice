//! Content-addressed media reads verify identity, not merely a valid encoding.
use crate::contracts::document::DocRef;
use std::io::Read;
use std::path::Path;

pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Validate pixels before publication, independent of their producing backend.
pub fn validate_png(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image exceeds 20 MiB".into());
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("invalid PNG: {e}"))?;
    let info = reader.info();
    if info.width == 0
        || info.height == 0
        || u64::from(info.width) * u64::from(info.height) > 16_777_216
    {
        return Err("image dimensions exceed the supported limit".into());
    }
    let size = reader
        .output_buffer_size()
        .ok_or("PNG output size is invalid")?;
    if size > 128 * 1024 * 1024 {
        return Err("decoded PNG exceeds memory limit".into());
    }
    let mut decoded = vec![0; size];
    reader
        .next_frame(&mut decoded)
        .map_err(|e| format!("invalid PNG pixels: {e}"))?;
    Ok(())
}

/// Tool screenshots are strict: silently dropping one would leave an agent
/// acting on an old image. All model dialects read the same verified pixels.
pub fn tool_pngs(
    result: &serde_json::Value,
    directory: Option<&Path>,
) -> Result<Vec<String>, String> {
    use base64::Engine;
    let Some(images) = result.get("latticeImages") else {
        return Ok(Vec::new());
    };
    let images = images.as_array().ok_or("tool images must be an array")?;
    if images.len() > 8 {
        return Err("too many tool result images".into());
    }
    let mut encoded = Vec::new();
    for image in images {
        if image.get("mediaType").is_some_and(|t| t != "image/png") {
            return Err("tool screenshot encoding must be PNG".into());
        }
        let reference = DocRef::of(image).ok_or("tool screenshot reference is invalid")?;
        let bytes = read_png(
            &reference,
            directory.ok_or("tool screenshots require their ledger directory")?,
        )?;
        validate_png(&bytes)?;
        encoded.push(base64::engine::general_purpose::STANDARD.encode(bytes));
    }
    Ok(encoded)
}

pub fn read_png(reference: &DocRef, directory: &Path) -> Result<Vec<u8>, String> {
    if DocRef::of(&serde_json::json!({"file":reference.file})).is_none() {
        return Err("unsafe image document name".into());
    }
    let path = directory.join(&reference.file);
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|e| format!("image {}: {e}", reference.file))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("image reference is not a regular file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|e| format!("image {}: {e}", reference.file))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > MAX_IMAGE_BYTES as u64 {
        return Err("image exceeds 20 MiB".into());
    }
    if reference.bytes != 0 && reference.bytes != size {
        return Err("image byte count no longer matches the ledger".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image grew beyond 20 MiB while reading".into());
    }
    let expected = crate::contracts::document::bytes_name(&bytes, "png");
    if reference.file != expected {
        return Err("image content no longer matches its recorded identity".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::document::store_bytes;
    #[test]
    fn replacing_an_image_does_not_silently_change_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        let reference = store_bytes(dir.path(), b"original", "png").unwrap();
        assert_eq!(read_png(&reference, dir.path()).unwrap(), b"original");
        std::fs::write(dir.path().join(&reference.file), b"replaced").unwrap();
        assert!(read_png(&reference, dir.path())
            .unwrap_err()
            .contains("identity"));
    }
    #[test]
    fn changed_size_and_oversize_files_are_rejected_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let mut reference = store_bytes(dir.path(), b"original", "png").unwrap();
        reference.bytes += 1;
        assert!(read_png(&reference, dir.path())
            .unwrap_err()
            .contains("byte count"));
        std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join(&reference.file))
            .unwrap()
            .set_len(MAX_IMAGE_BYTES as u64 + 1)
            .unwrap();
        assert!(read_png(&reference, dir.path())
            .unwrap_err()
            .contains("20 MiB"));
    }
    #[cfg(unix)]
    #[test]
    fn image_references_do_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let reference = store_bytes(dir.path(), b"original", "png").unwrap();
        let other = dir.path().join("other");
        std::fs::write(&other, b"original").unwrap();
        std::fs::remove_file(dir.path().join(&reference.file)).unwrap();
        std::os::unix::fs::symlink(&other, dir.path().join(&reference.file)).unwrap();
        assert!(read_png(&reference, dir.path()).is_err());
    }
}
