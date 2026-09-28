"""Check branch routing and commit trailers without executing repository code."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


TYPES = {"feat", "fix", "perf", "refactor", "docs", "test", "style", "chore"}


def branch_error(base, head, same_repository=True):
    if base == "develop":
        if head == "main" and same_repository:
            return None
        prefixes = ("feature/", "release/", "hotfix/")
    elif base == "main":
        prefixes = ("release/", "hotfix/")
    else:
        return "Pull requests must target develop or main."
    if any(head.startswith(prefix) and len(head) > len(prefix) for prefix in prefixes):
        return None
    return f"Branch {head!r} cannot be merged into {base!r}."


def message_errors(message, merge=False):
    lines = message.strip().splitlines()
    if not lines:
        return ["Commit message is empty."]
    errors = []
    subject = re.fullmatch(
        r"(?P<type>feat|fix|perf|refactor|docs|test|style|chore)"
        r"(?:\([^()\s]+\))?!?: (?P<description>\S.*)", lines[0],
    )
    if not subject:
        errors.append("Use an English Conventional Commit subject: type(scope): description.")
    if re.search(r"[\u3400-\u9fff]", lines[0]):
        errors.append("Write the commit subject in English.")
    trailers = [line for line in lines if line.startswith("Type:")]
    if len(trailers) != 1 or lines[-1] not in {f"Type: {kind}" for kind in TYPES}:
        errors.append("End with exactly one Type: trailer using an allowed value.")
    else:
        if subject and lines[-1] != f"Type: {subject.group('type')}":
            errors.append("The Type: trailer must match the Conventional Commit prefix.")
        parsed = subprocess.check_output(
            ["git", "interpret-trailers", "--parse"], input=message, text=True,
        ).splitlines()
        if lines[-1] not in parsed:
            errors.append("Separate the Type: trailer from the subject/body with a blank line.")
        if merge and lines[-1] != "Type: chore":
            errors.append("Merge commits must use Type: chore to avoid double-counting versions.")
    return errors


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def check_range(base, head):
    revision = f"{base}..{head}" if base and set(base) != {"0"} else head
    errors = []
    for row in git("rev-list", "--parents", revision, "--").splitlines():
        commit, *parents = row.split()
        for error in message_errors(git("show", "-s", "--format=%B", commit), len(parents) > 1):
            errors.append(f"{commit[:12]}: {error}")
    return errors


def check_event(event_name, event):
    errors = []
    if event_name == "pull_request":
        pr = event["pull_request"]
        base, head = pr["base"], pr["head"]
        error = branch_error(base["ref"], head["ref"], base["repo"]["id"] == head["repo"]["id"])
        if error:
            errors.append(error)
        merge_message = pr["title"] + "\n\n" + (pr.get("body") or "")
        errors.extend(f"Pull request: {error}" for error in message_errors(merge_message, merge=True))
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
    print("Branch routing and commit trailers passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
