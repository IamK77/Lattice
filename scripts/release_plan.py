"""Pure release planning and narrowly scoped manifest/changelog updates."""
from dataclasses import dataclass
import json
import re
import tomllib

from conventional import parse


@dataclass(frozen=True, order=True)
class Version:
    major: int
    minor: int
    patch: int

    @classmethod
    def read(cls, value):
        if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", value):
            raise ValueError("Automatic publication supports stable X.Y.Z versions only.")
        return cls(*(int(part) for part in value.split(".")))

    def __str__(self):
        return f"{self.major}.{self.minor}.{self.patch}"


def propose(current, published, messages):
    current = Version.read(current)
    if published is None:
        return str(current)
    previous = Version.read(published)
    if current < previous:
        raise ValueError("Cargo version is behind the latest published release; synchronize main first.")
    if current > previous:
        return str(current)
    commits = [parse(message) for message in messages]
    if any(commit is None for commit in commits):
        raise ValueError("Release changes must have Conventional Commit subjects.")
    if any(commit.breaking for commit in commits):
        return str(Version(previous.major + 1, 0, 0) if previous.major else Version(0, previous.minor + 1, 0))
    if any(commit.kind == "feat" for commit in commits):
        return str(Version(previous.major, previous.minor + 1, 0))
    if any(commit.kind in {"fix", "perf", "revert"} for commit in commits):
        return str(Version(previous.major, previous.minor, previous.patch + 1))
    return None


def _version_assignment(text, old, new):
    pattern = r'(?m)^(version\s*=\s*)"' + re.escape(old) + r'"(\s*(?:#.*)?)$'
    result, count = re.subn(pattern, lambda match: f'{match[1]}"{new}"{match[2]}', text)
    if count != 1:
        raise ValueError("Expected exactly one package version assignment.")
    return result


def cargo_manifest(text, new):
    before = tomllib.loads(text)
    old = before["package"]["version"]
    sections = re.split(r"(?m)(?=^\[)", text)
    indexes = [i for i, section in enumerate(sections) if section.startswith(("[package]\n", "[package]\r\n"))]
    if len(indexes) != 1:
        raise ValueError("Expected one standalone Cargo package section.")
    index = indexes[0]
    sections[index] = _version_assignment(sections[index], old, new)
    result = "".join(sections)
    before["package"]["version"] = new
    if tomllib.loads(result) != before:
        raise ValueError("Release update changed unrelated Cargo metadata.")
    return result


def cargo_lock(text, package, new):
    before = tomllib.loads(text)
    packages = [item for item in before["package"] if item["name"] == package and "source" not in item]
    if len(packages) != 1:
        raise ValueError("Expected exactly one local release package in Cargo.lock.")
    old = packages[0]["version"]
    chunks = re.split(r"(?m)(^\[\[package\]\]\r?\n)", text)
    matched = 0
    for index in range(2, len(chunks), 2):
        item = tomllib.loads(chunks[index])
        if item.get("name") == package and "source" not in item:
            chunks[index] = _version_assignment(chunks[index], old, new)
            matched += 1
    if matched != 1:
        raise ValueError("Unsupported Cargo.lock layout.")
    result = "".join(chunks)
    packages[0]["version"] = new
    if tomllib.loads(result) != before:
        raise ValueError("Release update changed unrelated locked dependencies.")
    return result


def changelog(text, version, commits):
    marker = "## [Unreleased]"
    if text.count(marker) != 1 or re.search(r"(?m)^## \[" + re.escape(version) + r"\]", text):
        raise ValueError("Changelog needs one Unreleased section and no duplicate release.")
    introduction, remainder = text.split(marker)
    following = re.search(r"(?m)^## \[", remainder)
    body = remainder[:following.start()] if following else remainder
    history = remainder[following.start():] if following else ""
    additions = {}
    for commit in commits:
        parsed = parse(commit["message"])
        if parsed is None:
            raise ValueError("Invalid release commit subject.")
        category = "Breaking changes" if parsed.breaking else {
            "feat": "Added", "fix": "Fixed", "perf": "Performance", "revert": "Fixed",
        }.get(parsed.kind)
        if category is None:
            continue
        # Notes are reviewed in a PR; no commit text is executed as a command.
        description = re.sub(r"([\\`*\[\]<>])", r"\\\1", parsed.description)
        scope = f"{parsed.scope}: " if parsed.scope else ""
        additions.setdefault(category, []).append(f"- {scope}{description} ({commit['sha'][:12]}).")
    for category, entries in additions.items():
        heading = f"### {category}"
        match = re.search(r"(?m)^" + re.escape(heading) + r"\s*$", body)
        if match:
            next_heading = re.search(r"(?m)^### ", body[match.end():])
            end = match.end() + next_heading.start() if next_heading else len(body)
            body = body[:end].rstrip() + "\n" + "\n".join(entries) + "\n\n" + body[end:]
        else:
            body = body.rstrip() + f"\n\n{heading}\n\n" + "\n".join(entries) + "\n"
    if not body.strip():
        raise ValueError("A release needs reviewable change notes.")
    return introduction + marker + f"\n\n## [{version}]\n\n" + body.strip() + "\n\n" + history


def prepare_files(files, version, commits):
    Version.read(version)
    manifest = tomllib.loads(files["Cargo.toml"])
    package = json.loads(files["clients/ink/package.json"])
    lock = json.loads(files["clients/ink/package-lock.json"])
    if not package.get("private") or manifest["package"].get("publish") is not False:
        raise ValueError("Registry publication is outside this release workflow.")
    package["version"] = version
    lock["version"] = version
    lock["packages"][""]["version"] = version
    return {
        "Cargo.toml": cargo_manifest(files["Cargo.toml"], version),
        "Cargo.lock": cargo_lock(files["Cargo.lock"], manifest["package"]["name"], version),
        "clients/ink/package.json": json.dumps(package, indent=2, ensure_ascii=False) + "\n",
        "clients/ink/package-lock.json": json.dumps(lock, indent=2, ensure_ascii=False) + "\n",
        "CHANGELOG.md": changelog(files["CHANGELOG.md"], version, commits),
    }
