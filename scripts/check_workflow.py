"""Check branch routing and English Conventional Commit subjects."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


from conventional import TYPES, parse


def branch_error(base, head, same_repository=True, author=None):
    if base == "develop":
        if head == "main" and same_repository:
            return None
        if same_repository and author == "dependabot[bot]" and head.startswith("dependabot/") and len(head) > len("dependabot/"):
            return None
        prefixes = ("feature/", "release/", "hotfix/")
    elif base == "main":
        prefixes = ("release/", "hotfix/")
    else:
        return "Pull requests must target develop or main."
    if any(head.startswith(prefix) and len(head) > len(prefix) for prefix in prefixes):
        return None
    return f"Branch {head!r} cannot be merged into {base!r}."


def message_errors(message):
    lines = message.strip().splitlines()
    if not lines:
        return ["Commit message is empty."]
    errors = []
    if parse(message) is None:
        errors.append("Use an English Conventional Commit subject: type(scope): description.")
    if re.search(r"[\u3400-\u9fff]", lines[0]):
        errors.append("Write the commit subject in English.")
    return errors


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def check_range(base, head):
    revision = f"{base}..{head}" if base and set(base) != {"0"} else head
    errors = []
    for commit in git("rev-list", revision, "--").splitlines():
        for error in message_errors(git("show", "-s", "--format=%B", commit)):
            errors.append(f"{commit[:12]}: {error}")
    return errors


def check_event(event_name, event):
    errors = []
    if event_name == "pull_request":
        pr = event["pull_request"]
        base, head = pr["base"], pr["head"]
        error = branch_error(
            base["ref"], head["ref"], base["repo"]["id"] == head["repo"]["id"],
            pr.get("user", {}).get("login"),
        )
        if error:
            errors.append(error)
        merge_message = pr["title"] + "\n\n" + (pr.get("body") or "")
        errors.extend(f"Pull request: {error}" for error in message_errors(merge_message))
        errors.extend(check_range(base["sha"], head["sha"]))
    elif event_name == "push":
        # Branch deletion has no commit to validate.
        if not event.get("deleted", False):
            # A deliberately rewritten history may no longer contain the old
            # tip in a fresh checkout. Validate the entire new history instead.
            base = None if event.get("forced", False) else event["before"]
            errors.extend(check_range(base, event["after"]))
    else:
        errors.append(f"Unsupported event: {event_name}")
    return errors


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    errors = check_event(os.environ["GITHUB_EVENT_NAME"], event)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print("Branch routing and Conventional Commit subjects passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
