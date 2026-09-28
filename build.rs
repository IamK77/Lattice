//! The version, computed from the history at build time.
//!
//! `v{major}.{feat}.{fix}-{hash}`
//!
//! - `major` is the person's call, and is the one number written down: it is
//!   the `version` in `Cargo.toml`.
//! - `feat` counts the commits that added something, since the major changed.
//! - `fix` counts the ones that corrected something, since the last addition —
//!   the same rule semantic versioning uses for a patch number.
//! - `hash` says exactly which commit this binary is, which is the part that
//!   matters when someone reports a problem.
//!
//! WHAT A COMMIT IS is read from a `Type:` trailer at the end of its message,
//! not from a prefix on its subject line. The subject lines here are Chinese
//! sentences written for a person to read, and `feat: ` in front of one would
//! cost that to save a parser some work.
//!
//! Without git — from a published archive, say — the version falls back to
//! what `Cargo.toml` says. A build that cannot see the history is not a build
//! that should fail.

use std::process::Command;

fn main() {
    // Cargo caches build scripts. Without these it would run once and then
    // report the same commit forever.
    //
    // Watch only existing paths: a missing rerun-if-changed target keeps
    // Cargo invalidating this build script. A new repository does not have
    // packed-refs until Git packs its references.
    //
    // Losing the watch on a file that is absent costs nothing: when `git gc`
    // does write it, that same pass rewrites `.git/refs`, which IS watched, so
    // the next build reruns this script and picks the new file up.
    for path in [".git/HEAD", ".git/refs", ".git/packed-refs"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let major = std::env::var("CARGO_PKG_VERSION")
        .ok()
        .and_then(|v| v.split('.').next().map(str::to_string))
        .unwrap_or_else(|| "0".to_string());

    let version = match history() {
        Some((feat, fix, hash)) => format!("v{major}.{feat}.{fix}-{hash}"),
        None => format!(
            "v{}",
            std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| major.clone())
        ),
    };
    println!("cargo:rustc-env=LATTICE_VERSION={version}");
}

/// (additions, corrections since the last addition, short hash).
fn history() -> Option<(usize, usize, String)> {
    let hash = run(&["rev-parse", "--short", "HEAD"])?;

    // Oldest first, one line per commit: the type, or empty where none was
    // given. `%(trailers:key=Type,valueonly)` is empty for a commit with no
    // such trailer, which is exactly the "counts as neither" case.
    let log = run(&[
        "log",
        "--reverse",
        "--format=%(trailers:key=Type,valueonly,separator=)",
    ])?;

    let mut feat = 0usize;
    let mut fix = 0usize;
    for line in log.lines() {
        match line.trim() {
            "feat" => {
                feat += 1;
                // A new addition starts the corrections over, the way a minor
                // version resets a patch number.
                fix = 0;
            }
            "fix" | "perf" => fix += 1,
            _ => {}
        }
    }
    Some((feat, fix, hash))
}

fn run(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
