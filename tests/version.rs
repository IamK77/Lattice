#[path = "../build_support/version.rs"]
mod version;

use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn cargo_version_is_the_only_release_version_authority() {
    assert_eq!(
        version::display_version("2.4.7", Some("abc123"), Some("2.4.7")).unwrap(),
        "v2.4.7"
    );
    assert_eq!(
        version::display_version("2.4.7", None, Some("2.4.7")).unwrap(),
        "v2.4.7"
    );
    assert!(version::display_version("2.4.7", Some("abc123"), Some("2.4.8")).is_err());
    assert!(version::display_version("2.4.7", None, Some("")).is_err());
}

#[test]
fn development_builds_and_archives_cannot_look_like_official_releases() {
    assert_eq!(
        version::display_version("0.1.0", Some("abc123"), None).unwrap(),
        "v0.1.0-dev+gabc123"
    );
    assert_eq!(
        version::display_version("0.1.0", None, None).unwrap(),
        "v0.1.0-dev"
    );
    assert_eq!(
        version::display_version("1.0.0-rc.1+vendor", Some("abc123"), None).unwrap(),
        "v1.0.0-rc.1.dev+vendor.gabc123"
    );
    assert_eq!(
        version::display_version("1.0.0+vendor", None, None).unwrap(),
        "v1.0.0-dev+vendor"
    );
}

#[test]
fn git_watches_exist_and_worktrees_resolve_their_actual_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "Version Test"]);
    git(&root, &["config", "user.email", "version@example.invalid"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    git(&root, &["config", "core.hooksPath", "/dev/null"]);
    git(
        &root,
        &["commit", "--allow-empty", "-qm", "feat: initialize test"],
    );
    let expected = git(&root, &["rev-parse", "--short=12", "HEAD"]);
    let (revision, paths) = version::repository_metadata(&root);
    assert_eq!(revision.as_deref(), Some(expected.as_str()));
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|path| path.exists()));
    assert!(!paths.iter().any(|path| path.ends_with("packed-refs")));

    let nested = root.join("archive");
    std::fs::create_dir(&nested).unwrap();
    assert_eq!(version::repository_metadata(&nested), (None, vec![]));
    let outside = temp.path().join("archive");
    std::fs::create_dir(&outside).unwrap();
    assert_eq!(version::repository_metadata(&outside), (None, vec![]));

    let worktree = temp.path().join("worktree");
    git(
        &root,
        &["worktree", "add", "--detach", worktree.to_str().unwrap()],
    );
    let (revision, paths) = version::repository_metadata(&worktree);
    assert_eq!(revision.as_deref(), Some(expected.as_str()));
    assert!(paths.iter().all(|path| path.exists()));
    assert!(paths.contains(&worktree.join(".git")));
    assert!(paths.iter().any(|path| path.ends_with("HEAD")));

    // Commit types and their counts no longer determine the release version.
    git(
        &root,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "feat: another change\n\nType: feat",
        ],
    );
    let (changed, _) = version::repository_metadata(&root);
    assert_ne!(changed, revision);
    assert_eq!(
        version::display_version("2.4.7", changed.as_deref(), Some("2.4.7")).unwrap(),
        "v2.4.7"
    );
}
