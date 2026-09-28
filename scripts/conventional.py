"""Shared Conventional Commit parsing for checks and release notes."""
from dataclasses import dataclass
import re

TYPES = {"feat", "fix", "perf", "refactor", "docs", "test", "style", "chore", "ci", "build", "revert"}
SUBJECT = re.compile(
    r"(?P<kind>" + "|".join(sorted(TYPES)) + r")"
    r"(?:\((?P<scope>[^()\s]+)\))?(?P<breaking>!)?: (?P<description>\S.*)"
)


@dataclass(frozen=True)
class Commit:
    kind: str
    scope: str
    description: str
    breaking: bool


def parse(message):
    lines = message.strip().splitlines()
    match = SUBJECT.fullmatch(lines[0]) if lines else None
    if not match:
        return None
    breaking = bool(match["breaking"]) or bool(re.search(r"(?m)^BREAKING[ -]CHANGE: \S", message))
    return Commit(match["kind"], match["scope"] or "", match["description"], breaking)
