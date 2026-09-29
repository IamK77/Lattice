"""Read immutable Git snapshots and produce release-candidate files."""
import json
import re
import subprocess
import tomllib

from release_plan import Version, prepare_files, propose

FILES = ("Cargo.toml", "Cargo.lock", "clients/ink/package.json", "clients/ink/package-lock.json", "CHANGELOG.md")
RECORD = ".release-candidate.json"
BRANCHES = {"develop": "release/next", "main": "release/hotfix-next"}
BRANCH = BRANCHES["develop"]


def git(root, *arguments):
    return subprocess.check_output(["git", "-C", str(root), *arguments], text=True).rstrip("\n")


def sha(value):
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("Expected a full GitHub commit SHA.")
    return value


def file_at(root, commit, path):
    sha(commit)
    if path not in (*FILES, RECORD):
        raise ValueError("Unsupported release file.")
    mode = git(root, "ls-tree", commit, "--", path).split(" ", 1)[0]
    if mode != "100644":
        raise ValueError(f"Release input must be a regular non-executable file: {path}")
    # Preserve the original trailing newlines, unlike scalar Git queries.
    return subprocess.check_output(["git", "-C", str(root), "show", f"{commit}:{path}"]).decode("utf-8")


def is_ancestor(root, older, newer):
    result = subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", sha(older), sha(newer)], check=False)
    if result.returncode not in (0, 1):
        raise ValueError("Cannot determine commit ancestry.")
    return result.returncode == 0


def commits_since(root, source, previous):
    sha(source)
    start = None
    if previous is not None:
        Version.read(previous)
        start = sha(git(root, "rev-parse", f"refs/tags/v{previous}^{{commit}}"))
        if not is_ancestor(root, start, source):
            raise ValueError("The published release is not an ancestor of development.")
    revision = f"{start}..{source}" if start else source
    return [{"sha": commit, "message": git(root, "show", "-s", "--format=%B", commit)}
            for commit in git(root, "rev-list", "--reverse", "--no-merges", revision, "--").splitlines()]


def candidate(root, source, previous, source_branch="develop"):
    if source_branch not in BRANCHES:
        raise ValueError("Release source must be develop or main.")
    files = {path: file_at(root, source, path) for path in FILES}
    commits = commits_since(root, source, previous)
    version = propose(tomllib.loads(files["Cargo.toml"])["package"]["version"], previous,
                      [commit["message"] for commit in commits])
    if version is None:
        return None
    updated = prepare_files(files, version, commits)
    record = {"schema": 1, "source": source, "source_branch": source_branch, "previous": previous, "version": version}
    updated[RECORD] = json.dumps(record, indent=2, sort_keys=True) + "\n"
    return updated


def verify_candidate(root, head):
    record = json.loads(file_at(root, head, RECORD))
    if set(record) != {"schema", "source", "source_branch", "previous", "version"} or record["schema"] != 1:
        raise ValueError("Unsupported release-candidate record.")
    generated = candidate(root, sha(record["source"]), record["previous"], record["source_branch"])
    if generated is None:
        raise ValueError("Candidate has no releasable changes.")
    changed = set(git(root, "diff", "--name-only", record["source"], sha(head), "--").splitlines())
    if not changed.issubset(generated):
        raise ValueError("Candidate contains manual code changes; refusing to overwrite them.")
    for path, expected in generated.items():
        if file_at(root, head, path) != expected:
            raise ValueError(f"Candidate was manually edited; review before regenerating: {path}")
    return record
