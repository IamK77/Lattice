"""Describe the exact native archives that will be attested and distributed."""
import argparse
import hashlib
import json
from pathlib import Path

from package_release import TARGETS, verify_archive
from release_git import sha
from release_plan import Version


MANIFEST = "release-manifest.json"
CHECKSUMS = "SHA256SUMS"
PROVENANCE = "provenance.json"


def archive_names(version):
    Version.read(version)
    return [f"lattice-v{version}-{target}.tar.gz" for target in TARGETS]


def digest(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError("Release assets must be regular files.")
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def write_exact(path, content):
    if path.exists():
        if path.is_symlink() or path.read_bytes() != content:
            raise ValueError(f"Refusing to replace a different release asset: {path.name}")
    else:
        with path.open("xb") as handle:
            handle.write(content)


def describe(directory, version, commit, preview):
    sha(commit)
    names = archive_names(version)
    allowed = {*names, CHECKSUMS, MANIFEST, PROVENANCE}
    if {path.name for path in directory.iterdir()} - allowed:
        raise ValueError("Unexpected files in the distribution directory.")
    artifacts = []
    for target, name in zip(TARGETS, names):
        path = directory / name
        checksum = digest(path)
        # Native execution already happened in each read-only build job. The
        # publisher only inspects data, never runs an artifact with write access.
        verify_archive(path, version, commit, target, preview, execute=False)
        artifacts.append({"name": name, "sha256": checksum, "bytes": path.stat().st_size, "target": target})
    checksums = "".join(f"{item['sha256']}  {item['name']}\n" for item in artifacts)
    write_exact(directory / CHECKSUMS, checksums.encode("utf-8"))
    record = {"schema": 1, "version": version, "commit": commit, "preview": preview,
              "archives": artifacts, "checksums_sha256": digest(directory / CHECKSUMS)}
    write_exact(directory / MANIFEST, (json.dumps(record, indent=2, sort_keys=True) + "\n").encode("utf-8"))
    return record


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--preview", action="store_true")
    args = parser.parse_args()
    print(json.dumps(describe(args.directory, args.version, args.commit, args.preview), indent=2))
