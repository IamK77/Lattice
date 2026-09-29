"""Return protected main history through a PR, never a direct develop push."""
import json
import os
import subprocess

from github_release import GitHub
from release_git import sha
from sync_git import SyncGit


BRANCH = "hotfix/sync-main"
TITLE = "chore: synchronize protected main into development"
BODY = """## Stable-history synchronization

This PR returns the latest reviewed `main` history to `develop`, including release
versions and change notes. A reusable `hotfix/sync-main` branch combines both
protected tips so strict up-to-date checks can pass. No development changes are
sent to main. Conflicts or manual branch edits stop synchronization; rerun after
protected branches advance, and delete the temporary branch after merging.

Native GitHub auto-merge waits for the normal branch protection and required PR
checks. If GitHub requires approval for repository-bot workflows, a maintainer
must approve those checks. This workflow does not replace checks, bypass branch
protection, resolve conflicts, or approve a release candidate.
"""


def branch_commit(api, name, missing_ok=False):
    result = api.request("GET", f"/git/ref/heads/{name}", missing_ok=missing_ok)
    if result is None:
        return None
    reference = result["object"]
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
    if (pull_request["base"]["ref"] != "develop" or pull_request["head"]["ref"] != BRANCH or
            pull_request["head"]["repo"]["full_name"].lower() != api.repository.lower()):
        raise ValueError("Only same-repository main-to-develop PRs can be synchronized.")
    if type(pull_request.get("number")) is not int or pull_request["number"] <= 0:
        raise ValueError("Invalid synchronization PR identifier.")
    if pull_request.get("state") != "open" or pull_request.get("draft") is not False:
        raise ValueError("The synchronization PR is not open and ready for checks.")
    sha(pull_request["head"]["sha"])


def merge_into_sync(api, graph, tip, head):
    if graph.ancestor(head, tip):
        return tip
    tree = graph.merged_tree(tip, head)
    if branch_commit(api, BRANCH) != tip:
        raise ValueError("Synchronization branch moved; inspect it before rerunning.")
    result = api.request("POST", "/merges", {
        "base": BRANCH, "head": head,
        "commit_message": "chore: integrate protected history into synchronization branch",
    })
    if not result:
        raise ValueError("Unexpected no-op merge; inspect the synchronization branch.")
    merged = sha(result["sha"])
    if branch_commit(api, BRANCH) != merged or graph.tree(merged) != tree:
        raise ValueError("Synchronization merge differs from the inspected result.")
    return merged


def synchronize(api, auto_merge=enable_auto_merge, graph=None):
    main = branch_commit(api, "main")
    develop = branch_commit(api, "develop")
    comparison = api.request("GET", f"/compare/{develop}...{main}")
    if comparison["status"] not in {"ahead", "behind", "identical", "diverged"} or type(comparison.get("ahead_by")) is not int:
        raise ValueError("Invalid synchronization ancestry response.")
    if comparison["ahead_by"] < 0:
        raise ValueError("Invalid synchronization commit count.")
    if comparison["ahead_by"] == 0:
        return {"outcome": "already-contained", "main": main, "develop": develop}
    candidates = api.pull_requests(BRANCH)
    if len(candidates) > 1:
        raise ValueError("Multiple synchronization PRs need maintainer inspection.")
    if candidates:
        pull_request = candidates[0]
        valid_sync_pr(api, pull_request)
        if pull_request["user"]["login"] != "github-actions[bot]":
            return {"outcome": "existing-human-pr", "number": pull_request["number"], "url": pull_request["html_url"]}
    graph = graph or SyncGit(os.getcwd(), api.repository)
    expected_tree = graph.merged_tree(develop, main)
    tip = branch_commit(api, BRANCH, missing_ok=True)
    if tip:
        if candidates and pull_request["head"]["sha"] != tip:
            raise ValueError("Synchronization PR and branch disagree; inspect before rerunning.")
        graph.validate(tip, main, develop)
    else:
        if candidates:
            raise ValueError("The open synchronization PR has lost its branch.")
        api.request("POST", "/git/refs", {"ref": f"refs/heads/{BRANCH}", "sha": develop})
        tip = develop
    tip = merge_into_sync(api, graph, tip, develop)
    tip = merge_into_sync(api, graph, tip, main)
    graph.validate(tip, main, develop)
    if not graph.ancestor(main, tip) or not graph.ancestor(develop, tip) or graph.tree(tip) != expected_tree:
        raise ValueError("Synchronization does not combine both protected histories exactly.")
    if branch_commit(api, "main") != main or branch_commit(api, "develop") != develop:
        raise ValueError("Protected history advanced; rerun synchronization before enabling auto-merge.")
    if candidates:
        pull_request = api.request("GET", f"/pulls/{pull_request['number']}")
    else:
        pull_request = api.request("POST", "/pulls", {"title": TITLE, "body": BODY, "head": BRANCH, "base": "develop"})
    valid_sync_pr(api, pull_request)
    if pull_request["user"]["login"] != "github-actions[bot]" or pull_request["head"]["sha"] != tip:
        raise ValueError("Automatic synchronization requires the unchanged repository-bot PR.")
    if branch_commit(api, BRANCH) != tip:
        raise ValueError("Synchronization branch advanced before auto-merge.")
    auto_merge(api, pull_request)
    return {"outcome": "native-auto-merge-requested", "number": pull_request["number"], "url": pull_request["html_url"]}


if __name__ == "__main__":
    print(json.dumps(synchronize(GitHub()), indent=2))
