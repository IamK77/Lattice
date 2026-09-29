use std::path::{Path, PathBuf};
use std::process::Command;

/// Cargo owns the version. A release build must explicitly confirm that same
/// version; ordinary builds remain distinguishable from published artifacts.
pub fn display_version(
    package: &str,
    revision: Option<&str>,
    release: Option<&str>,
) -> Result<String, String> {
    if let Some(release) = release {
        if release != package {
            return Err(format!(
                "release version {release:?} does not match Cargo package version {package:?}"
            ));
        }
        return Ok(format!("v{package}"));
    }
    let (core, metadata) = package.split_once('+').unwrap_or((package, ""));
    let suffix = if core.contains('-') { ".dev" } else { "-dev" };
    let mut version = format!("v{core}{suffix}");
    let metadata = match (metadata.is_empty(), revision) {
        (true, None) => String::new(),
        (true, Some(hash)) => format!("g{hash}"),
        (false, None) => metadata.to_owned(),
        (false, Some(hash)) => format!("{metadata}.g{hash}"),
    };
    if !metadata.is_empty() {
        version.push('+');
        version.push_str(&metadata);
    }
    Ok(version)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

/// Do not mistake an archive extracted inside another repository for that
/// repository's source. Worktree Git directories are resolved by Git itself.
pub fn repository_metadata(root: &Path) -> (Option<String>, Vec<PathBuf>) {
    let Some(top) = git(root, &["rev-parse", "--show-toplevel"]) else {
        return (None, vec![]);
    };
    if root.canonicalize().ok() != Path::new(&top).canonicalize().ok() {
        return (None, vec![]);
    }
    let revision = git(root, &["rev-parse", "--short=12", "HEAD"]);
    let mut watches = vec![];
    for name in ["HEAD", "refs", "packed-refs"] {
        if let Some(path) = git(root, &["rev-parse", "--git-path", name]) {
            let path = root.join(path);
            // Missing rerun-if-changed paths invalidate Cargo on every build.
            if path.exists() {
                watches.push(path);
            }
        }
    }
    if root.join(".git").is_file() {
        watches.push(root.join(".git"));
    }
    (revision, watches)
}
