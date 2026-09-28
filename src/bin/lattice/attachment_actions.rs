//! Receive attachments after modal routing. Draft owns unsent references;
//! this operation borrows only the draft, model catalog and document directory.
//! A returned notice replaces the current notice; None leaves it unchanged.
use crate::terminal_host::draft::Draft;
use lattice::view::ModelView;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "attachment_actions/tests.rs"]
mod tests;

pub(super) struct Attachments<'a> {
    pub draft: &'a mut Draft,
    pub catalog: &'a ModelView,
    pub documents: Option<&'a Path>,
}

impl Attachments<'_> {
    /// The caller has already routed forms and blocked other modal views.
    pub fn paste(&mut self, text: &str) -> Option<String> {
        match dragged_image(text) {
            Some((at, media)) => self.image(&at, media),
            None => {
                match dragged_file(text) {
                    Some((path, label)) => {
                        if !self.draft.edit().attach_file(&path, &label) {
                            self.draft.edit().paste(text);
                        }
                    }
                    None => self.draft.edit().paste(text),
                }
                None
            }
        }
    }

    pub fn clipboard(&mut self) -> Option<String> {
        match clipboard_image() {
            Some((bytes, ext)) => {
                let media = image_media_type(&format!("x.{ext}")).unwrap_or("image/png");
                let label = clipboard_label(&bytes);
                self.bytes(&bytes, ext, media, &label)
            }
            None => Some("no picture on the clipboard".into()),
        }
    }

    /// Read now, not when sending: the source may change or disappear later.
    /// Reading precedes capability and storage checks, including on failure.
    pub fn image(&mut self, at: &Path, media: &'static str) -> Option<String> {
        let bytes = match std::fs::read(at) {
            Ok(b) => b,
            Err(e) => return Some(format!("cannot read {}: {e}", at.display())),
        };
        let ext = at
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let label = at
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "image".to_string());
        self.bytes(&bytes, &ext, media, &label)
    }

    /// Store before registering with Draft; a full editor does not roll back
    /// either the saved document or the reference registered by Draft.
    pub fn bytes(&mut self, bytes: &[u8], ext: &str, media: &str, label: &str) -> Option<String> {
        // A known refusal must happen before writing, rather than mid-turn at
        // the provider. An absent current catalog row is not a refusal.
        if let Some(row) = self.catalog.current() {
            if !row.accepts_images {
                let others: Vec<&str> = self
                    .catalog
                    .rows
                    .iter()
                    .filter(|r| r.accepts_images && r.id != row.id)
                    .map(|r| r.id.as_str())
                    .collect();
                let how = if others.is_empty() {
                    "no model here says it can — add \"acceptsImages\": true to its                  profile in ~/.lattice/models.json"
                        .to_string()
                } else {
                    format!("/model to switch: {}", others.join(", "))
                };
                return Some(format!("{} cannot read pictures — {how}", row.id));
            }
        }
        let Some(dir) = self.documents else {
            return Some("this conversation has no ledger, so it cannot hold a picture".into());
        };
        match lattice::contracts::document::store_bytes(dir, bytes, ext) {
            Ok(stored) => {
                if !self.draft.attach(stored, media, label) {
                    return Some("too many attachments on one line".into());
                }
                None
            }
            Err(e) => Some(e),
        }
    }
}

/// Closed list: an unknown extension stays a path, not bytes sent to a model.
fn image_media_type(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

/// Terminals deliver dragged paths as text, often with shell quoting. Only a
/// single path is recognized; a sentence mentioning a file remains text.
fn dragged_path(pasted: &str) -> Option<String> {
    let one = pasted.trim();
    if one.contains('\n') {
        return None;
    }
    Some(
        one.strip_prefix('\'')
            .and_then(|r| r.strip_suffix('\''))
            .or_else(|| one.strip_prefix('"').and_then(|r| r.strip_suffix('"')))
            .unwrap_or(one)
            .replace("\\ ", " "),
    )
}

fn dragged_image(pasted: &str) -> Option<(PathBuf, &'static str)> {
    let bare = dragged_path(pasted)?;
    let media = image_media_type(&bare)?;
    let at = PathBuf::from(&bare);
    at.is_file().then_some((at, media))
}

/// Non-image files stay text: their path, not their contents, reaches the model.
fn dragged_file(pasted: &str) -> Option<(String, String)> {
    let bare = dragged_path(pasted)?;
    let at = Path::new(&bare);
    let meta = std::fs::metadata(at).ok()?;
    if !meta.is_file() {
        return None;
    }
    let name = at.file_name()?.to_string_lossy().into_owned();
    Some((
        bare.clone(),
        format!("{name} ({})", human_bytes(meta.len() as usize)),
    ))
}

/// Bracketed paste carries only text, so images need a separate system query.
/// macOS returns PNG clipboard data as AppleScript hexadecimal notation; other
/// platforms keep the existing unsupported result rather than guessing.
#[cfg(target_os = "macos")]
fn clipboard_image() -> Option<(Vec<u8>, &'static str)> {
    let out = std::process::Command::new("osascript")
        .args(["-e", "the clipboard as «class PNGf»"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let hex: String = text
        .split_once("PNGf")?
        .1
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.len() < 16 || !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    (bytes.len() == hex.len() / 2).then_some((bytes, "png"))
}

#[cfg(not(target_os = "macos"))]
fn clipboard_image() -> Option<(Vec<u8>, &'static str)> {
    None
}

/// Distinguish unnamed pictures by dimensions, falling back to byte size.
fn clipboard_label(bytes: &[u8]) -> String {
    match png_dimensions(bytes) {
        Some((w, h)) => format!("clipboard {w}×{h}"),
        None => format!("clipboard {}", human_bytes(bytes.len())),
    }
}

/// Read only the fixed signature and IHDR dimensions, not a full PNG decode.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (w, h) = (read(16), read(20));
    (w > 0 && h > 0).then_some((w, h))
}

fn human_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{n} B")
    }
}
