"""Return protected main history through a PR, never a direct develop push."""
import json
import os
import subprocess

from github_release import GitHub
from release_git import sha


TITLE = "chore: synchronize protected main into development"
BODY = """## Stable-history synchronization

This PR returns the latest reviewed `main` history to `develop`, including release
versions and change notes. The protected main branch remains the source; it may
advance while the PR is open. No development changes are sent to main here.

Native GitHub auto-merge waits for the normal branch protection and required PR
checks. If GitHub requires approval for repository-bot workflows, a maintainer
must approve those checks. This workflow does not replace checks, bypass branch
protection, resolve conflicts, or approve a release candidate.
"""


def branch_commit(api, name):
    reference = api.request("GET", f"/git/ref/heads/{name}")["object"]
    if reference["type"] != "commit":
        raise ValueError("A synchronization branch must point to a commit.")
    return sha(reference["sha"])


def enable_auto_merge(api, pull_request):
    subprocess.run(["gh", "pr", "merge", str(pull_request["number"]),
                    "--repo", api.repository, "--auto", "--merge",
                    "--match-head-commit", sha(pull_request["head"]["sha"])],
                   check=True, timeout=60,
                   env=dict(os.environ, GH_TOKEN=api.token, GH_HOST="github.com", GH_PROMPT_DISABLED="1"))


def valid_sync_pr(api, pull_request):
    if (pull_request["base"]["ref"] != "develop" or pull_request["head"]["ref"] != "main" or
            pull_request["head"]["repo"]["full_name"].lower() != api.repository.lower()):
        raise ValueError("Only same-repository main-to-develop PRs can be synchronized.")
    if type(pull_request.get("number")) is not int or pull_request["number"] <= 0:
        raise ValueError("Invalid synchronization PR identifier.")
    if pull_request.get("state") != "open" or pull_request.get("draft") is not False:
        raise ValueError("The synchronization PR is not open and ready for checks.")
    sha(pull_request["head"]["sha"])


def synchronize(api, auto_merge=enable_auto_merge):
    main = branch_commit(api, "main")
    develop = branch_commit(api, "develop")
    comparison = api.request("GET", f"/compare/{develop}...{main}")
    if comparison["status"] not in {"ahead", "behind", "identical", "diverged"} or type(comparison.get("ahead_by")) is not int:
        raise ValueError("Invalid synchronization ancestry response.")
    if comparison["ahead_by"] < 0:
        raise ValueError("Invalid synchronization commit count.")
    if comparison["ahead_by"] == 0:
        return {"outcome": "already-contained", "main": main, "develop": develop}
    candidates = [pr for pr in api.pull_requests("main") if pr["base"]["ref"] == "develop"]
    if len(candidates) > 1:
        raise ValueError("Multiple synchronization PRs need maintainer inspection.")
    if candidates:
        pull_request = candidates[0]
        valid_sync_pr(api, pull_request)
        if pull_request["user"]["login"] != "github-actions[bot]":
            return {"outcome": "existing-human-pr", "number": pull_request["number"], "url": pull_request["html_url"]}
    else:
        pull_request = api.request("POST", "/pulls", {"title": TITLE, "body": BODY, "head": "main", "base": "develop"})
        valid_sync_pr(api, pull_request)
        if pull_request["user"]["login"] != "github-actions[bot]":
            raise ValueError("Automatic synchronization must run with the repository workflow token.")
    auto_merge(api, pull_request)
    return {"outcome": "native-auto-merge-requested", "number": pull_request["number"], "url": pull_request["html_url"]}


if __name__ == "__main__":
    print(json.dumps(synchronize(GitHub()), indent=2))
