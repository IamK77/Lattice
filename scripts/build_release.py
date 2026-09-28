"""Build and capture release inputs in one clean, native checkout."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib

from package_release import SOURCE_INPUTS, TARGETS, checksum, inventory, package
from release_git import git, sha
from release_plan import Version

def clean_checkout(root, commit):
    if git(root, "rev-parse", "HEAD") != sha(commit):
        raise ValueError("Build checkout does not match the requested commit.")
    if git(root, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("Release builds require a clean checkout, including untracked source files.")


def build(root, output, version, commit, target, preview):
    Version.read(version)
    clean_checkout(root, commit)
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    if manifest["package"]["version"] != version:
        raise ValueError("Build version differs from Cargo.toml.")
    if target not in TARGETS:
        raise ValueError("Unsupported distribution target.")
    for variable in ["RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"]:
        if os.environ.get(variable):
            raise ValueError(f"Release recipe does not support a custom {variable}.")
    compiler = subprocess.check_output(["rustc", "--version", "--verbose"], text=True)
    if f"host: {target}\n" not in compiler:
        raise ValueError("Release packaging must run on the target's native host.")
    inputs = {name: checksum(root / name) for name in SOURCE_INPUTS}
    env = dict(os.environ, LATTICE_RELEASE_VERSION=version, CARGO_TARGET_DIR=str(root / "target"))
    subprocess.run(["cargo", "build", "--release", "--locked", "--bin", "lattice", "--target", target], cwd=root, env=env, check=True)
    binary = root / "target" / target / "release/lattice"
    binary_digest = checksum(binary)
    with tempfile.TemporaryDirectory() as directory:
        staging = Path(directory)
        licenses = staging / "THIRD-PARTY-LICENSES.html"
        subprocess.run(["cargo", "about", "generate", "--locked", "--all-features", "--workspace", "--fail",
                        "--manifest-path", "Cargo.toml", "--config", "about.toml", "--output-file", str(licenses), "about.hbs"], cwd=root, check=True)
        license_digest = checksum(licenses)
        command = ["otool", "-L", str(binary)] if target == TARGETS[1] else ["ldd", str(binary)]
        native = subprocess.check_output(command, text=True).replace(str(binary), "lattice")
        if target == TARGETS[0]:
            native += "\nELF version requirements:\n" + subprocess.check_output(["readelf", "--version-info", str(binary)], text=True)
        system = staging / "SYSTEM-DEPENDENCIES.txt"
        system.write_text(native)
        system_digest = checksum(system)
        metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"], cwd=root, text=True))
        dependencies = staging / "DEPENDENCIES.json"
        dependencies.write_text(json.dumps(inventory(metadata, tomllib.loads((root / "Cargo.lock").read_text()), commit), indent=2) + "\n")
        clean_checkout(root, commit)
        if inputs != {name: checksum(root / name) for name in SOURCE_INPUTS} or compiler != subprocess.check_output(["rustc", "--version", "--verbose"], text=True):
            raise ValueError("Build inputs or compiler changed while producing the artifacts.")
        receipt = {"schema": 1, "version": version, "commit": commit, "target": target, "preview": preview,
                   "rustc": compiler.strip(), "inputs": inputs, "files": {
                       "lattice": binary_digest, "THIRD-PARTY-LICENSES.html": license_digest,
                       "SYSTEM-DEPENDENCIES.txt": system_digest, "DEPENDENCIES.json": checksum(dependencies),
                       "LICENSE": checksum(root / "LICENSE"), "NOTICE": checksum(root / "NOTICE"),
                       "ASSET-ATTRIBUTION.md": checksum(root / "assets/README.md")}}
        return package(root, binary, licenses, system, output, version, commit, target, preview,
                       receipt=receipt, dependencies=dependencies)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--preview", action="store_true")
    args = parser.parse_args()
    build(Path(__file__).resolve().parent.parent, args.output, args.version, args.commit, args.target, args.preview)
