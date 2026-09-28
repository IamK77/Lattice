"""Locate an immutable signed distribution from this workflow run for recovery."""
import re

from release_git import sha


def retained_artifact(api, run_id, commit):
    sha(commit)
    if not re.fullmatch(r"[1-9][0-9]*", str(run_id)):
        raise ValueError("Invalid workflow run identifier.")
    matches = [item for item in api.pages(f"/actions/runs/{run_id}/artifacts", list_key="artifacts")
               if item["name"] == f"sealed-release-{commit}"]
    if not matches:
        return ""
    if len(matches) != 1:
        raise ValueError("Multiple retained distributions require manual inspection.")
    artifact = matches[0]
    if artifact.get("expired") is not False:
        raise ValueError("The original signed distribution expired; do not reconstruct or overwrite the draft blindly.")
    if artifact.get("workflow_run", {}).get("head_sha") != commit:
        raise ValueError("The retained distribution belongs to another source event.")
    if type(artifact.get("id")) is not int or artifact["id"] <= 0 or not re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest") or ""):
        raise ValueError("The retained distribution has no valid identity or digest.")
    return str(artifact["id"])
