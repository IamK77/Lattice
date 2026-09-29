"""Stage and publish exact approved assets without running distributed executables."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from github_release import GitHub, release_for_version
from release_git import file_at
from release_manifest import CHECKSUMS, MANIFEST, PROVENANCE, archive_names, describe, digest
from retained_release import retained_artifact
from validate_release import tag_commit, validate, workflow_identity


def asset_names(version):
    return [*archive_names(version), CHECKSUMS, MANIFEST, PROVENANCE]


def release_id(release):
    value = release["id"]
    if type(value) is not int or value <= 0:
        raise ValueError("Invalid release identifier.")
    return value


def listed_assets(api, release, version):
    result = {}
    for asset in api.pages(f"/releases/{release_id(release)}/assets"):
        name = asset["name"]
        if name not in asset_names(version) or name in result:
            raise ValueError("Unexpected or duplicate draft assets; inspect them manually.")
        if asset.get("state") != "uploaded" or type(asset.get("size")) is not int or not 0 < asset["size"] <= 128 * 1024 * 1024:
            raise ValueError("Incomplete or oversized release asset; inspect the draft manually.")
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", asset.get("digest") or ""):
            raise ValueError("GitHub did not supply a SHA-256 asset digest.")
        result[name] = asset
    return result


def check_asset(asset, path):
    if asset["digest"] != "sha256:" + digest(path) or asset["size"] != path.stat().st_size:
        raise ValueError(f"Existing asset differs; refusing replacement: {path.name}")


def gh_environment(api):
    return dict(os.environ, GH_TOKEN=api.token, GH_HOST="github.com", GH_PROMPT_DISABLED="1")


def download_asset(api, asset, path):
    identifier = asset["id"]
    if type(identifier) is not int or identifier <= 0:
        raise ValueError("Invalid release asset identifier.")
    if path.exists() or path.is_symlink():
        check_asset(asset, path)
        return
    # gh handles authenticated GitHub asset redirects. No caller-supplied URL
    # receives a token; only this fixed repository API endpoint is requested.
    with tempfile.TemporaryFile() as stream:
        subprocess.run(["gh", "api", f"repos/{api.repository}/releases/assets/{identifier}",
                        "--header", "Accept: application/octet-stream"],
                       stdout=stream, check=True, env=gh_environment(api), timeout=180)
        size = stream.tell()
        stream.seek(0)
        checksum = hashlib.file_digest(stream, "sha256").hexdigest()
        if size != asset["size"] or "sha256:" + checksum != asset["digest"]:
            raise ValueError("Downloaded release asset does not match GitHub's digest.")
        stream.seek(0)
        with path.open("xb") as destination:
            shutil.copyfileobj(stream, destination)


def verify_provenance(api, directory, identity):
    bundle = directory / PROVENANCE
    digest(bundle)
    source_ref = identity.get("source_ref", "refs/heads/main" if not identity.get("preview") else None)
    if not source_ref:
        raise ValueError("Preview verification requires the exact workflow source ref.")
    for name in asset_names(identity["version"])[:-1]:
        subprocess.run(["gh", "attestation", "verify", str(directory / name),
                        "--bundle", str(bundle), "--repo", api.repository,
                        "--source-digest", identity["commit"], "--source-ref", source_ref,
                        "--signer-workflow", f"{api.repository}/.github/workflows/release.yml",
                        "--deny-self-hosted-runners"],
                       check=True, env=gh_environment(api), timeout=120)


def existing_release(api, identity):
    # The by-tag endpoint documents published releases only. Complete listing
    # with the publication token also sees drafts; a lookup 404 is not absence.
    release = release_for_version(api.releases(), identity["version"])
    if release and release["draft"] and release.get("target_commitish") != identity["commit"]:
        raise ValueError("The existing draft belongs to a different source commit.")
    return release


def stage(root, directory, event, event_name, api, verifier=verify_provenance):
    identity = validate(root, event, event_name, api)
    directory.mkdir(parents=True, exist_ok=True)
    release = existing_release(api, identity)
    assets = listed_assets(api, release, identity["version"]) if release and not identity["preview"] else {}
    if identity["published"] and not identity["preview"]:
        if set(assets) != set(asset_names(identity["version"])):
            raise ValueError("Published release has an incomplete asset set.")
        for name, asset in assets.items():
            download_asset(api, asset, directory / name)
    describe(directory, identity["version"], identity["commit"], identity["preview"])
    # Reuse the stored bundle: a new signing invocation legitimately produces
    # different bytes. A retry must not overwrite the already uploaded proof.
    if PROVENANCE in assets:
        download_asset(api, assets[PROVENANCE], directory / PROVENANCE)
    reused = (directory / PROVENANCE).exists()
    if reused:
        verifier(api, directory, identity)
    for name, asset in assets.items():
        check_asset(asset, directory / name)
    return dict(identity, bundle_reused=reused)


def release_notes(root, identity):
    text = file_at(root, identity["commit"], "CHANGELOG.md")
    marker = f"## [{identity['version']}]"
    if text.count(marker) != 1:
        raise ValueError("Expected one reviewed changelog section for this release.")
    section = text.split(marker, 1)[1]
    following = re.search(r"(?m)^## ", section)
    notes = (section[:following.start()] if following else section).strip()
    if not notes:
        raise ValueError("Release notes cannot be empty.")
    return notes + "\n"


def finalize(root, directory, event, event_name, api, verifier=verify_provenance, *, verify_only=False):
    identity = validate(root, event, event_name, api)
    describe(directory, identity["version"], identity["commit"], identity["preview"])
    verifier(api, directory, identity)
    if identity["preview"] or verify_only:
        return dict(identity, outcome="verified-preview" if identity["preview"] else "verified-distribution")
    release = existing_release(api, identity)
    names = asset_names(identity["version"])
    assets = listed_assets(api, release, identity["version"]) if release else {}
    for name, asset in assets.items():
        check_asset(asset, directory / name)
    if identity["published"]:
        if set(assets) != set(names):
            raise ValueError("Published release has an incomplete asset set.")
        return dict(identity, outcome="verified-existing-publication")
    notes = release_notes(root, identity)
    tag = tag_commit(api, identity["version"])
    if tag is None:
        api.request("POST", "/git/refs", {"ref": f"refs/tags/v{identity['version']}", "sha": identity["commit"]})
    elif tag != identity["commit"]:
        raise ValueError("Release tag changed; never retag it.")
    if release is None:
        release = api.request("POST", "/releases", {
            "tag_name": f"v{identity['version']}", "target_commitish": identity["commit"],
            "name": f"v{identity['version']}", "body": notes,
            "draft": True, "prerelease": False, "generate_release_notes": False,
        })
    identifier = release_id(release)
    # Upload proof first, making interrupted publication resumable with the
    # original bundle rather than a newly generated signature.
    for name in [PROVENANCE, *names[:-1]]:
        if name not in assets:
            api.upload_asset(identifier, directory / name)
    actual = listed_assets(api, release, identity["version"])
    if set(actual) != set(names):
        raise ValueError("Draft asset set is incomplete; nothing was published.")
    for name, asset in actual.items():
        check_asset(asset, directory / name)
    if tag_commit(api, identity["version"]) != identity["commit"]:
        raise ValueError("Release tag changed before publication; refusing to publish.")
    current = api.request("GET", f"/releases/{identifier}")
    if (release_id(current) != identifier or current["tag_name"] != f"v{identity['version']}" or
            current["draft"] is not True or current["prerelease"] is not False or current["target_commitish"] != identity["commit"]):
        raise ValueError("Draft state changed concurrently; inspect it before retrying.")
    try:
        api.request("PATCH", f"/releases/{identifier}", {"draft": False, "body": notes, "make_latest": "true"})
    except Exception as error:
        raise RuntimeError("Publication outcome is unknown; inspect the release before retrying. It may already be public.") from error
    try:
        published = api.request("GET", f"/releases/{identifier}")
        if (release_id(published) != identifier or published["tag_name"] != f"v{identity['version']}" or
                published.get("immutable") is not True or published["draft"] is not False or published["prerelease"] is not False):
            raise ValueError("The expected immutable stable publication was not confirmed.")
        if tag_commit(api, identity["version"]) != identity["commit"]:
            raise ValueError("Published tag points to another commit.")
        final_assets = listed_assets(api, published, identity["version"])
        if set(final_assets) != set(names):
            raise ValueError("Published assets changed.")
        for name, asset in final_assets.items():
            check_asset(asset, directory / name)
    except Exception as error:
        raise RuntimeError("Publication already occurred, but its final immutability, tag, or asset verification failed; inspect the release without deleting or retagging it.") from error
    return dict(identity, published=True, outcome="published")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["retained", "stage", "verify", "finalize"])
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--event", type=Path, default=os.environ.get("GITHUB_EVENT_PATH"))
    parser.add_argument("--event-name", default=os.environ.get("GITHUB_EVENT_NAME"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    event = json.loads(args.event.read_text())
    api = GitHub()
    signing = workflow_identity(validate(root, event, args.event_name, api), os.environ["GITHUB_SHA"], os.environ["GITHUB_REF"])
    def verify_for_event(client, directory, identity):
        verify_provenance(client, directory, dict(identity, source_ref=signing["source_ref"]))
    if args.operation == "retained":
        retained = retained_artifact(api, os.environ["GITHUB_RUN_ID"], signing["commit"]) if not signing["preview"] and not signing["published"] else ""
        result = dict(signing, retained_id=retained)
    else:
        if args.directory is None:
            parser.error("--directory is required for staging or verification")
        if args.operation == "stage":
            result = stage(root, args.directory, event, args.event_name, api, verify_for_event)
        else:
            result = finalize(root, args.directory, event, args.event_name, api, verify_for_event, verify_only=args.operation == "verify")
    print(json.dumps(result, indent=2))
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as handle:
            for key in ["version", "commit", "preview", "published", "bundle_reused", "retained_id"]:
                if key in result:
                    value = str(result[key]).lower() if isinstance(result[key], bool) else result[key]
                    handle.write(f"{key}={value}\n")
