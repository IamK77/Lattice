"""Reject unrecorded, stale, or expired advisory exceptions."""
from datetime import date, datetime, timezone
import json
from pathlib import Path
import sys
import tomllib


def errors(config, exceptions, packages, today):
    ignored = [item["id"] if isinstance(item, dict) else item for item in config.get("advisories", {}).get("ignore", [])]
    recorded = [item["id"] for item in exceptions]
    problems = []
    if len(set(ignored)) != len(ignored) or len(set(recorded)) != len(recorded) or set(ignored) != set(recorded):
        problems.append("Every ignored advisory must have exactly one matching exception record.")
    locked = {(item["name"], item["version"]) for item in packages}
    for item in exceptions:
        if not item.get("owner") or not item.get("reason"):
            problems.append(f"{item['id']}: owner and reason are required.")
        if (item.get("package"), item.get("version")) not in locked:
            problems.append(f"{item['id']}: dependency changed; review or remove the exception.")
        try:
            expires = date.fromisoformat(item["expires"])
        except (KeyError, ValueError):
            problems.append(f"{item['id']}: invalid expiry date.")
            continue
        if today >= expires:
            problems.append(f"{item['id']}: exception expired on {expires}; review is required.")
    return problems


def main():
    root = Path(__file__).resolve().parent.parent
    config = tomllib.loads((root / "deny.toml").read_text())
    exceptions = json.loads((root / "advisory-exceptions.json").read_text())
    packages = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    problems = errors(config, exceptions, packages, datetime.now(timezone.utc).date())
    for problem in problems:
        print(problem, file=sys.stderr)
    if not problems:
        print("Advisory exceptions are recorded, current, and unexpired.")
    return bool(problems)


if __name__ == "__main__":
    sys.exit(main())
