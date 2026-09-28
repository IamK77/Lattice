"""Create and inspect native release archives without installing anything."""
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib

from release_git import git, sha
from release_plan import Version

TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-apple-darwin")
CONTENTS = {"lattice", "LICENSE", "NOTICE", "ASSET-ATTRIBUTION.md", "THIRD-PARTY-LICENSES.html",
            "BUILD-INFO.json", "DEPENDENCIES.json", "SYSTEM-DEPENDENCIES.txt"}
SOURCE_INPUTS = ("Cargo.toml", "Cargo.lock", "about.toml", "about.hbs")


def checksum(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError("Build inputs must be regular files.")
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def receipt_identity(receipt, version, commit, target, preview):
    if not isinstance(receipt, dict):
        raise ValueError("Archive build information must be an object.")
    for field, expected in {"schema": 1, "version": version, "commit": commit, "target": target, "preview": preview}.items():
        if type(receipt.get(field)) is not type(expected) or receipt[field] != expected:
            raise ValueError(f"Archive identity mismatch: {field}")
    if not isinstance(receipt.get("rustc"), str) or not receipt["rustc"].strip():
        raise ValueError("Build-time compiler information is required.")
    for field, names in [("inputs", set(SOURCE_INPUTS)), ("files", CONTENTS - {"BUILD-INFO.json"})]:
        hashes = receipt.get(field)
        if not isinstance(hashes, dict) or set(hashes) != names:
            raise ValueError(f"Incomplete build receipt: {field}")
        if any(not isinstance(value, str) or len(value) != 64 or any(char not in "0123456789abcdef" for char in value) for value in hashes.values()):
            raise ValueError(f"Invalid build receipt digest: {field}")


def architecture(header, target):
    if target == TARGETS[0]:
        valid = len(header) >= 20 and header[:6] == b"\x7fELF\x02\x01" and struct.unpack_from("<H", header, 18)[0] == 62
    elif target == TARGETS[1]:
        valid = len(header) >= 16 and header[:4] == b"\xcf\xfa\xed\xfe" and struct.unpack_from("<I", header, 4)[0] == 0x0100000c and struct.unpack_from("<I", header, 12)[0] == 2
    else:
        raise ValueError("Unsupported distribution target.")
    if not valid:
        raise ValueError(f"Executable header does not match {target}.")


def check_binary(binary, version, target):
    if binary.is_symlink() or not binary.is_file():
        raise ValueError("Release executable must be a regular file.")
    with binary.open("rb") as handle:
        architecture(handle.read(32), target)
    with tempfile.TemporaryDirectory() as home:
        env = {"PATH": os.environ.get("PATH", ""), "HOME": home, "LATTICE_HOME": home, "LANG": "C.UTF-8"}
        for arguments, expected in [(('--version',), f"v{version}\n"), (('--help',), None)]:
            result = subprocess.run([str(binary.resolve()), *arguments], cwd=home, env=env,
                                    capture_output=True, text=True, timeout=20, check=True)
            if expected is not None and result.stdout != expected:
                raise ValueError("Packaged executable version differs from the release version.")
            if expected is None and "lattice" not in (result.stdout + result.stderr).lower():
                raise ValueError("Packaged executable help did not identify Lattice.")


def inventory(metadata, locked, commit):
    identities = {item["id"]: f"{item['name']}@{item['version']}" for item in metadata["packages"]}
    if len(set(identities.values())) != len(identities):
        raise ValueError("Ambiguous dependency identities need an inventory schema update.")
    root_id = metadata["resolve"].get("root")
    if root_id not in identities:
        raise ValueError("Distribution inventory requires a single root Cargo package.")
    if any(item.get("source") is None and item["id"] != root_id for item in metadata["packages"]):
        raise ValueError("Local dependencies outside the root package need explicit source provenance.")
    checksums = {(item["name"], item["version"], item.get("source")): item.get("checksum") for item in locked["package"]}
    edges = {node["id"]: sorted(identities[item] for item in node["dependencies"]) for node in metadata["resolve"]["nodes"]}
    packages = []
    for item in metadata["packages"]:
        packages.append({"id": identities[item["id"]], "name": item["name"], "version": item["version"],
                         "license": item.get("license"), "source": item.get("source") or f"git-commit:{commit}",
                         "checksum": checksums.get((item["name"], item["version"], item.get("source"))),
                         "dependencies": edges.get(item["id"], [])})
    return {"schema": 1, "scope": "Locked Cargo metadata graph, not a binary-derived SBOM; may include build, development, and other-target dependencies.",
            "root": identities[root_id], "packages": sorted(packages, key=lambda item: item["id"])}


def verify_inventory(graph, version, commit):
    if not isinstance(graph, dict) or type(graph.get("schema")) is not int or graph["schema"] != 1:
        raise ValueError("Unsupported dependency inventory.")
    packages = graph.get("packages")
    if not isinstance(packages, list) or not packages or any(not isinstance(item, dict) for item in packages):
        raise ValueError("Dependency inventory must contain package records.")
    identities = [item.get("id") for item in packages]
    if any(not isinstance(item, str) for item in identities) or len(set(identities)) != len(identities):
        raise ValueError("Dependency inventory identities must be unique strings.")
    root = f"lattice@{version}"
    if graph.get("root") != root or root not in identities:
        raise ValueError("Dependency inventory has a different root package.")
    for item in packages:
        dependencies = item.get("dependencies")
        if not isinstance(dependencies, list) or any(not isinstance(dependency, str) or dependency not in identities for dependency in dependencies):
            raise ValueError("Dependency inventory has unresolved edges.")
        if item["id"] == root and item.get("source") != f"git-commit:{commit}":
            raise ValueError("Dependency inventory has a different source commit.")


def package(root, binary, licenses, system_dependencies, output, version, commit, target, preview=False, *, receipt, dependencies):
    Version.read(version)
    sha(commit)
    if git(root, "rev-parse", "HEAD") != commit or tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"] != version:
        raise ValueError("Checkout, manifest, and requested release identity must agree.")
    if git(root, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("Packaging requires the clean checkout used for the build.")
    receipt_identity(receipt, version, commit, target, preview)
    sources = {"lattice": binary, "LICENSE": root / "LICENSE", "NOTICE": root / "NOTICE",
               "ASSET-ATTRIBUTION.md": root / "assets/README.md", "THIRD-PARTY-LICENSES.html": licenses,
               "SYSTEM-DEPENDENCIES.txt": system_dependencies, "DEPENDENCIES.json": dependencies}
    if receipt["inputs"] != {name: checksum(root / name) for name in SOURCE_INPUTS}:
        raise ValueError("Source inputs differ from the build receipt.")
    if receipt["files"] != {name: checksum(path) for name, path in sources.items()}:
        raise ValueError("Artifact inputs differ from the build receipt.")
    check_binary(binary, version, target)
    license_text = licenses.read_text()
    if "<section>" not in license_text or "Apache License" not in license_text:
        raise ValueError("Missing generated third-party license text.")
    if not system_dependencies.read_text().strip():
        raise ValueError("Missing native dependency inspection.")
    epoch = int(git(root, "show", "-s", "--format=%ct", commit))
    name = f"lattice-v{version}-{target}"
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{name}.tar.gz"
    if archive.exists():
        raise ValueError("Refusing to overwrite an existing archive.")
    with tempfile.TemporaryDirectory() as directory:
        staging = Path(directory)
        for destination, source in sources.items():
            if source.is_symlink() or not source.is_file():
                raise ValueError(f"Distribution input must be a regular file: {destination}")
            shutil.copyfile(source, staging / destination)
        (staging / "BUILD-INFO.json").write_text(json.dumps(receipt, indent=2) + "\n")
        with tempfile.TemporaryDirectory(dir=output) as archive_directory:
            pending = Path(archive_directory) / archive.name
            with pending.open("xb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as tar:
                    for path in sorted(staging.iterdir()):
                        info = tar.gettarinfo(str(path), arcname=f"{name}/{path.name}")
                        info.uid = info.gid = 0
                        info.uname = info.gname = ""
                        info.mtime = epoch
                        info.mode = 0o755 if path.name == "lattice" else 0o644
                        with path.open("rb") as content:
                            tar.addfile(info, content)
            verify_archive(pending, version, commit, target, preview)
            # Same-filesystem, exclusive publication: no partial final name and
            # no replacement if another writer created it during verification.
            os.link(pending, archive)
    with archive.open("rb") as handle:
        digest = hashlib.file_digest(handle, "sha256").hexdigest()
    print(f"Verified archive: {archive.name} ({digest})")
    return archive


def verify_archive(archive, version, commit, target, preview, *, execute=True):
    prefix = f"lattice-v{version}-{target}/"
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        if len(members) != len(CONTENTS) or {member.name for member in members} != {prefix + name for name in CONTENTS}:
            raise ValueError("Unexpected, missing, or duplicate archive members.")
        if any(not member.isfile() for member in members):
            raise ValueError("Release archives must not contain links or special files.")
        build = json.load(tar.extractfile(prefix + "BUILD-INFO.json"))
        receipt_identity(build, version, commit, target, preview)
        for name, expected in build["files"].items():
            with tar.extractfile(prefix + name) as content:
                if hashlib.file_digest(content, "sha256").hexdigest() != expected:
                    raise ValueError(f"Archive contents differ from the build receipt: {name}")
        verify_inventory(json.load(tar.extractfile(prefix + "DEPENDENCIES.json")), version, commit)
        member = tar.getmember(prefix + "lattice")
        if member.mode != 0o755:
            raise ValueError("Executable mode was not preserved.")
        with tar.extractfile(member) as content:
            architecture(content.read(32), target)
        if not execute:
            return
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "lattice"
            with tar.extractfile(member) as source, binary.open("wb") as destination:
                shutil.copyfileobj(source, destination)
            binary.chmod(0o755)
            check_binary(binary, version, target)
