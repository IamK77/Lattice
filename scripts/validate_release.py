"""Bind release builds to a canonical candidate and an explicit merge approval."""
import argparse
import json
import os
from pathlib import Path

from github_release import GitHub, latest_published, release_for_version
from release_git import BRANCHES, git, sha, verify_candidate
from release_plan import Version
from retained_release import retained_artifact


def tag_commit(api, version):
    ref = api.request("GET", f"/git/ref/tags/v{Version.read(version)}", missing_ok=True)
    if ref is None:
        return None
    target = ref["object"]
    for _ in range(10):
        commit = sha(target["sha"])
        if target["type"] == "commit":
            return commit
        if target["type"] != "tag":
            raise ValueError("Release tag does not resolve to a commit.")
        target = api.request("GET", f"/git/tags/{commit}")["object"]
    raise ValueError("Release tag nesting exceeds the supported bound.")


def approved_candidate(root, pull_request, repository):
    if pull_request.get("merged") is not True or pull_request["base"]["ref"] != "main":
        raise ValueError("Publication requires a merged release PR targeting main.")
    if pull_request["head"]["repo"]["full_name"].lower() != repository.lower():
        raise ValueError("Publication cannot use a cross-repository candidate.")
    commit = sha(pull_request["merge_commit_sha"])
    if git(root, "rev-parse", "HEAD") != commit:
        raise ValueError("Publication must use the exact approved merge commit.")
    record = verify_candidate(root, commit)
    if BRANCHES[record["source_branch"]] != pull_request["head"]["ref"]:
        raise ValueError("The approved branch does not match the candidate source.")
    return record


def validate(root, event, event_name, api):
    commit = sha(git(root, "rev-parse", "HEAD"))
    preview = event_name == "workflow_dispatch"
    if preview:
        requested = sha(event["inputs"]["expected_sha"])
        if commit != requested:
            raise ValueError("Preview checkout moved after it was requested.")
        record = verify_candidate(root, commit)
    else:
        if event_name != "pull_request" or event.get("action") != "closed" or event["pull_request"].get("merged") is not True:
            raise ValueError("Only merged release PRs can authorize publication.")
        number = event["number"]
        if not isinstance(number, int) or isinstance(number, bool) or number <= 0:
            raise ValueError("Invalid release PR number.")
        actual = api.request("GET", f"/pulls/{number}")
        if actual["merge_commit_sha"] != event["pull_request"]["merge_commit_sha"]:
            raise ValueError("Release approval changed after the event was recorded.")
        record = approved_candidate(root, actual, api.repository)
    version = record["version"]
    releases = api.releases()
    existing = release_for_version(releases, version)
    tag = tag_commit(api, version)
    if tag is not None and tag != commit:
        raise ValueError("The release tag already belongs to another commit; never retag it.")
    published = bool(existing and not existing["draft"])
    if published and (tag != commit or existing.get("immutable") is not True or existing["prerelease"]):
        raise ValueError("An existing publication is not the expected immutable stable release.")
    if not published:
        previous = latest_published(releases)
        if previous != record["previous"]:
            raise ValueError("Published history changed; regenerate and review the candidate.")
        if previous is not None and Version.read(version) <= Version.read(previous):
            raise ValueError("New releases must advance the published stable version.")
    return {"schema": 1, "version": version, "commit": commit, "preview": preview, "published": published,
            "source_branch": record["source_branch"]}


def workflow_identity(identity, event_commit, event_ref):
    # OIDC signs the event commit, not an arbitrary checkout HEAD.
    if sha(event_commit) != identity["commit"]:
        raise ValueError("The workflow event must name the exact build commit; dispatch on the candidate ref.")
    allowed = {"refs/heads/main"}
    if identity["preview"]:
        allowed.add("refs/heads/" + BRANCHES[identity["source_branch"]])
    if event_ref not in allowed:
        raise ValueError("The signing workflow is not running on an allowed source ref.")
    return dict(identity, source_ref=event_ref)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", type=Path, default=os.environ.get("GITHUB_EVENT_PATH"))
    parser.add_argument("--event-name", default=os.environ.get("GITHUB_EVENT_NAME"))
    args = parser.parse_args()
    api = GitHub()
    result = validate(Path(__file__).resolve().parent.parent, json.loads(args.event.read_text()), args.event_name, api)
    result = workflow_identity(result, os.environ["GITHUB_SHA"], os.environ["GITHUB_REF"])
    result["retained_id"] = retained_artifact(api, os.environ["GITHUB_RUN_ID"], result["commit"]) if not result["preview"] and not result["published"] else ""
    print(json.dumps(result, indent=2))
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as handle:
            for key in ["version", "commit", "preview", "published", "retained_id"]:
                value = str(result[key]).lower() if isinstance(result[key], bool) else result[key]
                handle.write(f"{key}={value}\n")
