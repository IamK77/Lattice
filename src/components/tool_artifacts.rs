//! Ordinary, readable evidence files. Names are generated here, never supplied
//! by fetched content. In-memory kernels get a persistent temporary directory.
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) fn directory(ledger: Option<&Path>) -> Result<PathBuf, String> {
    let dir = match ledger {
        Some(path) => crate::contracts::document::documents_dir(path),
        None => tempfile::Builder::new()
            .prefix("lattice-artifacts-")
            .tempdir()
            .map_err(|e| e.to_string())?
            .keep(),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    dir.canonicalize().map_err(|e| e.to_string())
}

pub(super) fn store(dir: &Path, bytes: &[u8], extension: &str) -> Result<Value, String> {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let extension: String = extension
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let path = dir.join(format!("{digest}.{extension}"));
    let mut staged = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    staged.write_all(bytes).map_err(|e| e.to_string())?;
    staged.flush().map_err(|e| e.to_string())?;
    match staged.persist_noclobber(&path) {
        Ok(_) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !meta.file_type().is_file()
                || meta.len() != bytes.len() as u64
                || std::fs::read(&path).map_err(|e| e.to_string())? != bytes
            {
                return Err("existing artifact does not match its content address".to_string());
            }
        }
        Err(e) => return Err(e.to_string()),
    }
    Ok(json!({"path": path, "bytes": bytes.len(), "sha256": digest}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_is_exact_deduplicated_and_tampering_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let first = store(dir.path(), b"full evidence\n", "txt").unwrap();
        assert_eq!(first, store(dir.path(), b"full evidence\n", "txt").unwrap());
        let path = first["path"].as_str().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"full evidence\n");
        std::fs::write(path, "tampered").unwrap();
        assert!(store(dir.path(), b"full evidence\n", "txt").is_err());
    }
}
