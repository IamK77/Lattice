//! Fetching a folder from a source — the shared half of "install from
//! somewhere". A local directory is copied (symlinks skipped: fetched
//! content is data, and a symlink could smuggle in files from outside it);
//! a git URL is shallow-cloned and its .git directory removed (an installed
//! thing is data, not a checkout). Used by the skill library's install_skill
//! and the workshop's install_component_from.

use std::io::Read;
use std::path::Path;

pub(crate) enum FetchError {
    /// The caller was cancelled mid-fetch (interrupt, deadline)
    Cancelled,
    /// The source could not be fetched; the message says why
    Failed(String),
}

pub(crate) fn is_git_source(source: &str) -> bool {
    source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.ends_with(".git")
}

pub(crate) fn source_basename(source: &str) -> String {
    let trimmed = source.trim_end_matches('/');
    let base = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed);
    base.trim_end_matches(".git").to_string()
}

/// Fetch `source` (local directory or git URL) into `into`, polling
/// `cancelled` while a clone runs.
pub(crate) fn fetch(
    source: &str,
    into: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), FetchError> {
    if is_git_source(source) {
        git_clone(source, into, cancelled)
    } else {
        copy_dir(Path::new(source), into)
    }
}

/// Shallow-clone a git source, honoring cancellation (the child is killed).
fn git_clone(source: &str, into: &Path, cancelled: &dyn Fn() -> bool) -> Result<(), FetchError> {
    let mut child = std::process::Command::new("git")
        .args(["clone", "--depth", "1", source])
        .arg(into)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        // git asks the TERMINAL for credentials when a source needs them, and
        // it inherits ours. A private URL therefore stopped the clone dead
        // waiting for a username, with the prompt fighting the TUI for the
        // screen and nothing able to answer it. A source that needs
        // credentials should fail and say so, not hang the session.
        .stdin(std::process::Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .spawn()
        .map_err(|e| FetchError::Failed(format!("cannot run git: {e}")))?;
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(FetchError::Cancelled);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(FetchError::Failed(format!("waiting on git failed: {e}"))),
        }
    };
    let stderr = String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default()).to_string();
    if !status.success() {
        let excerpt: String = stderr.chars().take(500).collect();
        return Err(FetchError::Failed(format!("git clone failed: {excerpt}")));
    }
    let _ = std::fs::remove_dir_all(into.join(".git"));
    Ok(())
}

/// Recursive copy, skipping symlinks.
fn copy_dir(from: &Path, to: &Path) -> Result<(), FetchError> {
    if !from.is_dir() {
        return Err(FetchError::Failed(format!(
            "source is not a directory: {}",
            from.display()
        )));
    }
    let failed = |e: std::io::Error| FetchError::Failed(format!("copy failed: {e}"));
    std::fs::create_dir_all(to).map_err(failed)?;
    for entry in std::fs::read_dir(from).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let kind = entry.file_type().map_err(failed)?;
        let target = to.join(entry.file_name());
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(failed)?;
        }
    }
    Ok(())
}
