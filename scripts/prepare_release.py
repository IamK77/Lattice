"""Create or fast-forward one reviewed release PR; never publish or force-push."""
import argparse
import json
from pathlib import Path

from github_release import GitHub, latest_published
from release_git import BRANCHES, RECORD, candidate, file_at, git, is_ancestor, sha, verify_candidate
from release_plan import Version

START = "<!-- lattice-release:start -->"
END = "<!-- lattice-release:end -->"


def description(repository, record):
    branch = BRANCHES[record["source_branch"]]
    return f"""{START}
## Release candidate v{record['version']}

This PR is generated from `{record['source']}`. Review the version, Changelog,
compatibility notes, and all checks before merging. Merging this PR into `main`
is the publication approval; do not enable automatic merge on this release PR.

[Change notes](https://github.com/{repository}/blob/{branch}/CHANGELOG.md)

GitHub may require a maintainer to approve CI runs created by its repository
bot. That is permission to run checks, not permission to publish. Publication
after merge does not require a second release button.

New development is incorporated only while the generated candidate is unchanged.
Manual edits stop regeneration rather than being overwritten. Make a deliberate
version override in Cargo.toml on `{record['source_branch']}`, through its normal review process.
{END}"""


def replace_description(body, generated):
    if body.count(START) != 1 or body.count(END) != 1:
        raise ValueError("Release PR markers were edited; refusing to overwrite its description.")
    before, remainder = body.split(START)
    _, after = remainder.split(END)
    return before + generated + after


def prepare(root, api, source_branch="develop"):
    if source_branch not in BRANCHES:
        raise ValueError("Release source must be develop or main.")
    branch = BRANCHES[source_branch]
    source = sha(git(root, "rev-parse", "HEAD"))
    expected = sha(api.request("GET", f"/git/ref/heads/{source_branch}")["object"]["sha"])
    if source != expected:
        print("Source advanced after checkout; leave preparation to its next run.")
        return None
    stable = sha(api.request("GET", "/git/ref/heads/main")["object"]["sha"])
    if not is_ancestor(root, stable, source):
        raise ValueError("Synchronize main into develop before preparing another release.")
    releases = api.releases()
    if any(release["draft"] for release in releases):
        raise ValueError("A draft release exists; finish or explicitly resolve it before preparing another.")
    previous = latest_published(releases)
    proposals = api.pull_requests(branch, state="all")
    opened = [pr for pr in proposals if pr["state"] == "open"]
    if len(opened) > 1:
        raise ValueError("More than one release PR is open.")
    pr = opened[0] if opened else None
    latest = max(proposals, key=lambda item: item["number"], default=None)
    if not pr and latest and latest["state"] == "closed" and not latest["merged_at"]:
        print("The release PR was closed without merging; preparation remains paused until it is reopened.")
        return None
    ref = api.request("GET", f"/git/ref/heads/{branch}", missing_ok=True)
    old_head = sha(ref["object"]["sha"]) if ref else None
    if pr:
        if pr["base"]["ref"] != "main" or pr["head"]["repo"]["full_name"].lower() != api.repository.lower():
            raise ValueError("Release PR has an unexpected source or target.")
        if pr["user"]["login"] != "github-actions[bot]" or pr["head"]["sha"] != old_head:
            raise ValueError("The release PR is not the expected repository-bot candidate.")
    # Closed PR metadata may keep or follow a reused branch head. Git ancestry,
    # not that metadata, determines whether the candidate already reached main.
    active_candidate = bool(old_head and (pr or not is_ancestor(root, old_head, stable)))
    if active_candidate:
        # This also recovers a branch created before PR creation failed.
        old_record = verify_candidate(root, old_head)
        if old_record["source_branch"] != source_branch:
            raise ValueError("Candidate source branch changed unexpectedly.")
        if old_record["previous"] is not None and (previous is None or Version.read(previous) < Version.read(old_record["previous"])):
            raise ValueError("Published history moved backwards; explicit review is required.")
        if not is_ancestor(root, old_record["source"], source):
            raise ValueError("Candidate source history diverged; refusing to discard it.")
    files = candidate(root, source, previous, source_branch)
    if files is None:
        if pr:
            withdrawn = f"{START}\nThis candidate was withdrawn: the current source `{source}` no longer requests a release.\nReview the source before reopening and rerunning preparation.\n{END}"
            body = replace_description(pr.get("body") or "", withdrawn)
            api.request("PATCH", f"/pulls/{pr['number']}", {"state": "closed", "body": body})
        print("No user-facing changes or explicit version override require a release; any open candidate was withdrawn.")
        return None
    record = json.loads(files[RECORD])
    body = description(api.repository, record)
    if pr:
        body = replace_description(pr.get("body") or "", body)
    # Verify before mutation. Missing objects can be fetched read-only; no token
    # is put into a Git remote or persisted credential helper by this script.
    same = old_head and all(file_at(root, old_head, path) == content for path, content in files.items())
    if not same:
        base_tree = api.request("GET", f"/git/commits/{source}")["tree"]["sha"]
        tree = api.request("POST", "/git/trees", {
            "base_tree": base_tree,
            "tree": [{"path": path, "mode": "100644", "type": "blob", "content": content}
                     for path, content in files.items()],
        })
        parents = [old_head, source] if active_candidate else [source]
        commit = api.request("POST", "/git/commits", {
            "message": f"chore(release): prepare v{record['version']}",
            "tree": tree["sha"], "parents": parents,
        })
        if old_head:
            api.request("PATCH", f"/git/refs/heads/{branch}", {"sha": commit["sha"], "force": False})
        else:
            api.request("POST", "/git/refs", {"ref": f"refs/heads/{branch}", "sha": commit["sha"]})
    title = f"chore(release): prepare v{record['version']}"
    if pr:
        if pr["title"] != f"chore(release): prepare v{old_record['version']}":
            title = pr["title"]
        result = api.request("PATCH", f"/pulls/{pr['number']}", {"title": title, "body": body})
    else:
        result = api.request("POST", "/pulls", {"head": branch, "base": "main", "title": title, "body": body})
    print(f"Release PR ready for review: {result['html_url']}")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", choices=tuple(BRANCHES), default="develop")
    args = parser.parse_args()
    prepare(Path(__file__).resolve().parent.parent, GitHub(), args.source)
