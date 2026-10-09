//! The user's standing choices, in the user directory.
//!
//! Distinct from the assembly overlay next to it: an overlay says what is
//! ASSEMBLED (which components exist, how they are wired), and installing
//! writes it. This file says how the person likes it SET — choices that
//! belong to them rather than to any one conversation.
//!
//! That difference is the reason this exists at all. A setting recovered from
//! the ledger would come back only in the stream it was set in; the next
//! conversation would silently start over at the default. Someone who chose
//! to think harder chose it for their work, not for one transcript.
//!
//! Precedence, strongest first: what was said on THIS launch (an environment
//! variable the user typed), then this file, then the built-in default.
//! Something typed a second ago outranks something chosen last week — and
//! nothing here can silently overrule an instruction the user just gave.

use std::path::PathBuf;

use serde_json::{json, Value};

/// `~/.lattice/preferences.json`, or `LATTICE_PREFERENCES` when set.
pub fn path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("LATTICE_PREFERENCES") {
        return (!custom.is_empty()).then(|| PathBuf::from(custom));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".lattice/preferences.json"))
}

/// Read the whole document. A missing file is an empty document; so is an
/// unreadable or malformed one — a preference that cannot be parsed must not
/// stop the program starting, because nothing here is load-bearing.
pub fn load() -> Value {
    match path() {
        Some(path) => load_from(&path),
        None => json!({}),
    }
}

/// `load`, told where to look. Split out so the behaviour can be tested
/// against a real file without a test having to move `HOME`.
pub fn load_from(path: &std::path::Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    }
}

/// Read one preference.
pub fn get(key: &str) -> Option<Value> {
    let doc = load();
    doc.get(key).filter(|v| !v.is_null()).cloned()
}

/// Write one preference, leaving the rest of the document alone. Returns the
/// path written, or an explanation.
///
/// Read-modify-write rather than overwrite: this file is one document with
/// many settings in it, and a `/effort` command must not delete a choice it
/// knows nothing about.
pub fn set(key: &str, value: Value) -> Result<PathBuf, String> {
    let Some(path) = path() else {
        return Err("no home directory to save preferences in".to_string());
    };
    set_in(&path, key, value)
}

/// `set`, told where to write.
pub fn set_in(path: &std::path::Path, key: &str, value: Value) -> Result<PathBuf, String> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("json.lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|_| "preferences are being edited; try again".to_string())?;
    let original = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut doc = match &original {
        Some(bytes) => serde_json::from_slice::<Value>(bytes).map_err(|_| {
            format!(
                "{} is invalid JSON; refusing to overwrite preferences",
                path.display()
            )
        })?,
        None => json!({}),
    };
    if !doc.is_object() {
        return Err("preferences must be an object; refusing to overwrite them".into());
    }
    doc[key] = value;
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(staged.as_file_mut(), &doc).map_err(|e| e.to_string())?;
    staged.write_all(b"\n").map_err(|e| e.to_string())?;
    staged.as_file().sync_all().map_err(|e| e.to_string())?;
    let current = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if current != original {
        return Err("preferences changed during save; try again".into());
    }
    staged
        .persist(path)
        .map_err(|e| format!("cannot replace preferences: {}", e.error))?;
    Ok(path.to_path_buf())
}

/// Which thinking setting wins, given what was said on this launch and what
/// was chosen before. Separated from reading the environment and the file so
/// the RULE can be tested without either.
///
/// The order is the point: something typed a second ago outranks something
/// chosen last week. Without that, `LATTICE_THINKING=off lattice` would be
/// silently ignored by anyone who had ever run `/effort` — a command doing
/// nothing, with no way to tell.
pub fn resolve_thinking(said_this_launch: Option<&str>, standing: Option<Value>) -> Option<Value> {
    match said_this_launch {
        // Said, but empty: send no parameter at all. Not the same as off, and
        // the only safe setting for an endpoint that rejects unknown ones.
        Some("") => None,
        Some("off") | Some("false") => Some(json!(false)),
        Some(word) => Some(json!(word)),
        None => Some(standing.unwrap_or_else(|| json!("high"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_typed_this_launch_outranks_the_standing_choice() {
        let standing = || Some(json!("max"));

        assert_eq!(
            resolve_thinking(Some("off"), standing()),
            Some(json!(false)),
            "an instruction given a second ago must not be overruled by a file"
        );
        assert_eq!(
            resolve_thinking(None, standing()),
            Some(json!("max")),
            "nothing said this launch: the standing choice holds"
        );
        assert_eq!(
            resolve_thinking(None, None),
            Some(json!("high")),
            "and with neither, the product default"
        );
        assert_eq!(
            resolve_thinking(Some(""), standing()),
            None,
            "saying nothing at all stays expressible"
        );
    }

    /// One document, many settings. A command that sets one key must not
    /// delete a choice it knows nothing about — including keys written by a
    /// later version of the program.
    #[test]
    fn writing_one_preference_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, r#"{"thinking":"low","something-else":42}"#).unwrap();

        set_in(&path, "thinking", json!("max")).unwrap();

        let doc = load_from(&path);
        assert_eq!(doc["thinking"], "max");
        assert_eq!(doc["something-else"], 42, "an unknown key must survive");
    }

    /// Nothing here is load-bearing: a corrupt file must not stop startup.
    #[test]
    fn a_broken_document_reads_as_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, "{not json at all").unwrap();
        assert_eq!(load_from(&path), json!({}));
        assert_eq!(load_from(&dir.path().join("absent.json")), json!({}));
    }
}
