//! Documents that live beside the ledger rather than inside it.
//!
//! A system prompt, a command's output, a tool schema — these are documents:
//! they have lines and paragraphs, and both people and agents READ them. Put
//! inside a JSON string they stop being documents. Every newline becomes the
//! two characters `\n`, every quote becomes `\"`, and 27 KB of prose arrives
//! as a single line.
//!
//! That matters because the ledger's first reader is the agent itself, and its
//! tools work by line. Measured on a real ledger: the same system prompt is one
//! 56 045-byte line inside the record and 191 lines as a file. Searching it in
//! place finds the containing line and clips it to 400 characters — the
//! envelope fields, never the match. As a file, the same search names the line.
//!
//! So a document moves to a file next to the ledger and the event keeps a
//! reference to it. The reference carries enough — size, line count, an opening
//! preview — that most questions are answered without opening anything.
//!
//! This module is the READING half: what a reference looks like and how to
//! follow one. Writing them is the ledger's business, at append time.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where one event's documents live: the ledger file's own name, minus the
/// extension. `…/20260729-074225-tui.jsonl` keeps its documents in
/// `…/20260729-074225-tui/`.
///
/// Beside the ledger rather than in one shared store, so that a conversation
/// stays one movable thing. Sharing documents across conversations was
/// measured and is not worth it: deduplicating inside each ledger takes 25.5 MB
/// of real records down to 5.8 MB, and doing it globally reaches 5.3 MB — the
/// repetition is within a conversation, not between them.
pub fn documents_dir(ledger: &Path) -> PathBuf {
    if ledger.is_dir()
        || ledger
            .extension()
            .is_some_and(|extension| extension == "ledger")
    {
        ledger.join("documents")
    } else {
        ledger.with_extension("")
    }
}

/// A field that was moved out to a file.
///
/// `file` is a bare name, resolved against [`documents_dir`] — never a path.
/// A reference that could name `../../` anything would turn a ledger into a
/// way to read arbitrary files, and a ledger is not always written by someone
/// you trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRef {
    pub file: String,
    pub bytes: u64,
    pub lines: Option<u64>,
    pub preview: Option<String>,
}

impl DocRef {
    /// Read a reference out of a field value, if that is what it is.
    ///
    /// Returns `None` for an inline value — a plain string, an array — which
    /// is what makes old ledgers keep working: the shapes coexist, and a
    /// reader that follows references also reads records written before they
    /// existed.
    pub fn of(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let file = object.get("file")?.as_str()?;
        if !is_safe_name(file) {
            return None;
        }
        Some(DocRef {
            file: file.to_string(),
            bytes: object.get("bytes").and_then(Value::as_u64).unwrap_or(0),
            lines: object.get("lines").and_then(Value::as_u64),
            preview: object
                .get("preview")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    /// The document itself, read from the directory beside its ledger.
    pub fn read(&self, dir: &Path) -> Result<String, String> {
        let at = dir.join(&self.file);
        std::fs::read_to_string(&at).map_err(|e| format!("cannot read {}: {e}", at.display()))
    }

    /// The document as BYTES — for the ones that were never text.
    ///
    /// An image is not a big string that got moved out; it arrives as a file
    /// and never had a readable form. It shares this directory and this
    /// name-safety rule, and nothing else.
    pub fn read_bytes(&self, dir: &Path) -> Result<Vec<u8>, String> {
        let at = dir.join(&self.file);
        std::fs::read(&at).map_err(|e| format!("cannot read {}: {e}", at.display()))
    }
}

/// The same naming rule is used when storing bytes and verifying them later.
pub(crate) fn bytes_name(bytes: &[u8], extension: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(bytes));
    let ext: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_lowercase();
    if ext.is_empty() {
        digest[..32].to_string()
    } else {
        format!("{}.{ext}", &digest[..32])
    }
}

/// Put bytes in the directory beside a ledger and describe them.
///
/// CONTENT-ADDRESSED, for two reasons. The same picture attached twice is
/// stored once. And the name is computed here rather than taken from whoever
/// supplied the file — a name that came from outside would be a name an
/// attacker chooses, and `is_safe_name` is a check, not a guarantee about
/// where the check was applied.
///
/// Unlike a text document, this is written by the HOST and not by the kernel's
/// append. The kernel moves a field out when it grows; an image was already a
/// file before any event mentioned it, so there is nothing to move — only a
/// reference to record.
pub fn store_bytes(dir: &Path, bytes: &[u8], extension: &str) -> Result<DocRef, String> {
    let file = bytes_name(bytes, extension);
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let at = dir.join(&file);
    // Already there means already identical — the name IS the content.
    if !at.exists() {
        std::fs::write(&at, bytes).map_err(|e| format!("cannot write {}: {e}", at.display()))?;
    }
    Ok(DocRef {
        file,
        bytes: bytes.len() as u64,
        lines: None,
        preview: None,
    })
}

/// A file name and nothing else: no separators, no `..`, not hidden, not empty.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// A field's value, whether it was left inline or moved out.
///
/// Text documents come back as a JSON string and `.json` documents come back
/// parsed, because the field they belong to had that shape before it moved:
/// `system` was a string, `tools` was an array, and a reader should not have
/// to know which of them was big enough to be moved.
pub fn resolve(value: &Value, dir: Option<&Path>) -> Result<Value, String> {
    let Some(reference) = DocRef::of(value) else {
        return Ok(value.clone());
    };
    let Some(dir) = dir else {
        return Err(format!(
            "{} was moved out of the ledger, but this reader has no directory to look in",
            reference.file
        ));
    };
    let text = reference.read(dir)?;
    if reference.file.ends_with(".json") {
        serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", reference.file))
    } else {
        Ok(Value::String(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn documents_sit_in_a_directory_named_after_their_ledger() {
        assert_eq!(
            documents_dir(Path::new("/x/20260729-074225-tui.jsonl")),
            PathBuf::from("/x/20260729-074225-tui")
        );
    }

    /// An inline value passes through untouched. This is what lets a reader
    /// that follows references also read every ledger written before them.
    #[test]
    fn an_inline_value_is_returned_as_it_stands() {
        let dir = tempfile::tempdir().unwrap();
        let inline = json!("the whole system prompt, right here");
        assert_eq!(resolve(&inline, Some(dir.path())).unwrap(), inline);
        let array = json!([{"name": "Read"}]);
        assert_eq!(resolve(&array, Some(dir.path())).unwrap(), array);
        assert_eq!(resolve(&Value::Null, None).unwrap(), Value::Null);
        // An object that is not a reference is a value like any other
        let other = json!({"fingerprint": "sha256:…"});
        assert_eq!(resolve(&other, None).unwrap(), other);
    }

    #[test]
    fn a_reference_is_followed_and_a_json_document_comes_back_parsed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ev_4-system.md"), "# Who you are\n\nEva.\n").unwrap();
        std::fs::write(dir.path().join("ev_4-tools.json"), r#"[{"name":"Read"}]"#).unwrap();

        let text = resolve(
            &json!({"file": "ev_4-system.md", "bytes": 21, "lines": 3}),
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(text, json!("# Who you are\n\nEva.\n"));

        let tools = resolve(
            &json!({"file": "ev_4-tools.json", "bytes": 18}),
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(
            tools,
            json!([{"name": "Read"}]),
            "a field that was an array comes back an array"
        );
    }

    /// A ledger is data, and data is not always written by someone you trust.
    /// A reference that could name a path would make reading one a way to
    /// read anything on the machine.
    /// Content-addressed: the same picture twice is one file, and the name is
    /// computed here rather than accepted from outside.
    #[test]
    fn stored_bytes_are_named_by_their_content_and_read_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let png = [0x89u8, b'P', b'N', b'G', 0, 1, 2, 3];
        let one = store_bytes(dir.path(), &png, "PNG").unwrap();
        let two = store_bytes(dir.path(), &png, "png").unwrap();
        assert_eq!(one.file, two.file, "same content, same name");
        assert!(one.file.ends_with(".png"), "{}", one.file);
        assert_eq!(one.bytes, png.len() as u64);
        assert_eq!(one.read_bytes(dir.path()).unwrap(), png);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "stored once"
        );

        let other = store_bytes(dir.path(), b"different", "png").unwrap();
        assert_ne!(other.file, one.file, "different content, different name");
    }

    /// The extension comes from outside, so it is not allowed to be a path or
    /// anything else with a say in where the file lands.
    #[test]
    fn a_hostile_extension_cannot_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        for hostile in ["../../etc/passwd", "png/../..", "", "p n g", "PNG;rm -rf"] {
            let stored = store_bytes(dir.path(), b"xyz", hostile).unwrap();
            assert!(
                is_safe_name(&stored.file),
                "{hostile:?} produced {}",
                stored.file
            );
            assert!(dir.path().join(&stored.file).exists());
        }
    }

    #[test]
    fn a_reference_that_names_a_path_is_not_a_reference_at_all() {
        for hostile in [
            "../../../etc/passwd",
            "/etc/passwd",
            "..\\windows\\system32",
            ".hidden",
            "",
        ] {
            let value = json!({"file": hostile, "bytes": 1});
            assert_eq!(
                DocRef::of(&value),
                None,
                "{hostile:?} must not read as a reference"
            );
            // And it therefore passes through as an ordinary value, unread
            assert_eq!(resolve(&value, None).unwrap(), value);
        }
    }

    #[test]
    fn a_missing_directory_is_an_error_rather_than_a_silent_empty_string() {
        let value = json!({"file": "ev_4-system.md", "bytes": 21});
        let err = resolve(&value, None).unwrap_err();
        assert!(err.contains("ev_4-system.md"), "{err}");
        let dir = tempfile::tempdir().unwrap();
        let err = resolve(&value, Some(dir.path())).unwrap_err();
        assert!(err.contains("cannot read"), "{err}");
    }
}
