//! Byte snapshots and explicit LSP UTF-16 positions. Never slice at a guessed
//! character boundary or attach a fresh hash to text read from another version.
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) const MAX_SOURCE: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Source {
    pub path: PathBuf,
    pub text: String,
    pub version: String,
    lines: std::sync::OnceLock<Vec<u32>>,
}

impl Source {
    #[cfg(test)]
    pub fn fixture(text: &str) -> Self {
        Self {
            path: PathBuf::from("/fixture.rs"),
            text: text.into(),
            version: format!("sha256:{:x}", Sha256::digest(text.as_bytes())),
            lines: std::sync::OnceLock::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        if path.to_str().is_none() {
            return Err("source path is not UTF-8".into());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Inspect the opened descriptor without waiting for a FIFO writer.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(&path).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("source must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take((MAX_SOURCE + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_SOURCE {
            return Err("source exceeds the 8 MiB snapshot limit".into());
        }
        let version = format!("sha256:{:x}", Sha256::digest(&bytes));
        let text = String::from_utf8(bytes).map_err(|_| "source is not UTF-8")?;
        Ok(Self {
            path,
            text,
            version,
            lines: std::sync::OnceLock::new(),
        })
    }

    pub fn uri(&self) -> Result<String, String> {
        reqwest::Url::from_file_path(&self.path)
            .map(|url| url.to_string())
            .map_err(|_| "cannot encode file URI".into())
    }

    pub fn verify(&self) -> Result<(), String> {
        if Self::load(&self.path)?.version != self.version {
            return Err("source changed during navigation; locate it again".into());
        }
        Ok(())
    }

    /// Tool coordinates are one-based Unicode scalar columns, not bytes or
    /// UTF-16 units. The protocol position is zero-based UTF-16.
    pub fn position(&self, line: u64, column: u64) -> Result<Value, String> {
        let row = line.checked_sub(1).ok_or("line must be at least 1")?;
        let col = column.checked_sub(1).ok_or("column must be at least 1")? as usize;
        let (_, text) = self.line_text(row)?;
        if col > text.chars().count() {
            return Err("column is outside the source line".into());
        }
        let units: usize = text.chars().take(col).map(char::len_utf16).sum();
        Ok(json!({"line": row, "character": units}))
    }

    pub fn offset(&self, position: &Value) -> Result<usize, String> {
        let line = position["line"].as_u64().ok_or("missing protocol line")?;
        let units = position["character"]
            .as_u64()
            .ok_or("missing protocol character")? as usize;
        let (start, text) = self.line_text(line)?;
        let mut seen = 0;
        for (byte, ch) in text.char_indices() {
            if seen == units {
                return Ok(start + byte);
            }
            seen += ch.len_utf16();
            if seen > units {
                return Err("protocol position splits a UTF-16 surrogate pair".into());
            }
        }
        if seen == units {
            Ok(start + text.len())
        } else {
            Err("protocol column is outside the snapshot".into())
        }
    }

    fn line_text(&self, wanted: u64) -> Result<(usize, &str), String> {
        let bytes = self.text.as_bytes();
        let lines = self.lines.get_or_init(|| {
            // u32 is enough for the bounded snapshot. Reserve explicitly so
            // a newline-only file does not double the allocation at growth.
            let capacity = bytes
                .iter()
                .filter(|&&b| matches!(b, b'\r' | b'\n'))
                .count()
                + 1;
            let mut lines = Vec::with_capacity(capacity);
            lines.push(0);
            let mut at = 0;
            while at < bytes.len() {
                if matches!(bytes[at], b'\r' | b'\n') {
                    if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
                        at += 1;
                    }
                    lines.push((at + 1) as u32);
                }
                at += 1;
            }
            lines
        });
        let wanted =
            usize::try_from(wanted).map_err(|_| "protocol line is outside the snapshot")?;
        let start = *lines
            .get(wanted)
            .ok_or("protocol line is outside the snapshot")? as usize;
        let mut end = lines
            .get(wanted + 1)
            .map_or(bytes.len(), |&offset| offset as usize);
        if end > start && bytes[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        Ok((start, &self.text[start..end]))
    }

    /// Read counts LF-delimited lines, whereas LSP also recognizes bare CR.
    /// Convert through the actual byte offset instead of copying line numbers.
    pub fn read_from(&self, byte: usize) -> Value {
        let prefix = &self.text[..byte];
        let line = prefix.bytes().filter(|&byte| byte == b'\n').count() + 1;
        let column = prefix.rfind('\n').map_or(byte, |at| byte - at - 1);
        json!({"path":self.path,"from":line,"fromByte":column,"expectedVersion":self.version})
    }

    pub fn slice(&self, range: &Value) -> Result<&str, String> {
        let start = self.offset(&range["start"])?;
        let end = self.offset(&range["end"])?;
        self.text
            .get(start..end)
            .ok_or_else(|| "reversed or invalid protocol range".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_preserve_unicode_crlf_and_exclusive_ends() {
        let source = Source::fixture("a😀中\r\nnext\n");
        assert_eq!(
            source.position(1, 3).unwrap(),
            json!({"line":0,"character":3})
        );
        assert!(source.offset(&json!({"line":0,"character":2})).is_err());
        let range = json!({"start":{"line":0,"character":1},"end":{"line":1,"character":0}});
        assert_eq!(source.slice(&range).unwrap(), "😀中\r\n");
        assert_eq!(
            source.offset(&json!({"line":2,"character":0})).unwrap(),
            source.text.len()
        );
        assert!(source.position(1, 9).is_err());
        assert!(source.position(0, 1).is_err());
    }

    #[test]
    fn mixed_line_endings_keep_protocol_positions_on_the_original_bytes() {
        let source = Source::fixture("a\rb\r\nc\nd");
        assert_eq!(source.read_from(5)["from"], 2);
        assert_eq!(source.read_from(2)["fromByte"], 2);
        assert_eq!(source.offset(&json!({"line":1,"character":0})).unwrap(), 2);
        assert_eq!(
            source.offset(&json!({"line":3,"character":1})).unwrap(),
            source.text.len()
        );
    }

    #[test]
    fn content_changes_invalidate_snapshots_even_at_the_same_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "old").unwrap();
        let source = Source::load(&path).unwrap();
        source.verify().unwrap();
        std::fs::write(&path, "new").unwrap();
        assert!(source.verify().is_err());
    }
}
