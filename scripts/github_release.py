"""Small GitHub API client; release workflows use only their repository token."""
import json
import os
import re
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import HTTPRedirectHandler, Request, build_opener

from release_plan import Version


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        # Never forward the repository token or request body to a redirect target.
        return None


class GitHub:
    def __init__(self, repository=None, token=None):
        self.repository = repository or os.environ["GITHUB_REPOSITORY"]
        self.token = token or os.environ["GITHUB_TOKEN"]
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", self.repository):
            raise ValueError("Invalid repository identity.")

    def request(self, method, path, data=None, missing_ok=False):
        if not path.startswith("/") or "://" in path or ".." in path:
            raise ValueError("Invalid repository API path.")
        body = json.dumps(data).encode() if data is not None else None
        request = Request(f"https://api.github.com/repos/{self.repository}{path}", data=body, method=method, headers={
            "Authorization": f"Bearer {self.token}", "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28", "Content-Type": "application/json",
            "User-Agent": "Lattice-release-workflow",
        })
        try:
            with build_opener(NoRedirect()).open(request, timeout=60) as response:
                content = response.read()
                return json.loads(content) if content else None
        except HTTPError as error:
            if missing_ok and error.code == 404:
                return None
            raise RuntimeError(f"GitHub {method} {path} failed with HTTP {error.code}; no automatic retry.") from None

    def upload_asset(self, release_id, path):
        if type(release_id) is not int or release_id <= 0:
            raise ValueError("Invalid release identifier.")
        if not re.fullmatch(r"[A-Za-z0-9._-]+", path.name) or path.is_symlink() or not path.is_file():
            raise ValueError("Invalid release asset file.")
        if path.stat().st_size > 128 * 1024 * 1024:
            raise ValueError("Release asset exceeds the supported upload bound.")
        url = f"https://uploads.github.com/repos/{self.repository}/releases/{release_id}/assets?" + urlencode({"name": path.name})
        request = Request(url, data=path.read_bytes(), method="POST", headers={
            "Authorization": f"Bearer {self.token}", "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28", "Content-Type": "application/octet-stream",
            "User-Agent": "Lattice-release-workflow",
        })
        try:
            with build_opener(NoRedirect()).open(request, timeout=120) as response:
                return json.loads(response.read())
        except HTTPError as error:
            raise RuntimeError(f"GitHub asset upload failed with HTTP {error.code}; inspect the draft before retrying.") from None

    def pages(self, path, *, list_key=None, **parameters):
        results = []
        for page in range(1, 101):
            rows = self.request("GET", path + "?" + urlencode(dict(parameters, per_page=100, page=page)))
            if list_key is not None:
                rows = rows[list_key]
            if not isinstance(rows, list):
                raise ValueError("Expected a paginated GitHub list.")
            results.extend(rows)
            if len(rows) < 100:
                return results
        raise ValueError("GitHub pagination limit reached; refusing a partial release decision.")

    def releases(self):
        return self.pages("/releases")

    def pull_requests(self, head, state="open"):
        return self.pages("/pulls", state=state, head=f"{self.repository.split('/')[0]}:{head}")


def release_for_version(releases, version):
    tag = f"v{Version.read(version)}"
    matches = [release for release in releases if release["tag_name"] == tag]
    if len(matches) > 1:
        raise ValueError("Multiple releases use this tag; inspect them before proceeding.")
    return matches[0] if matches else None


def latest_published(releases):
    versions = []
    for release in releases:
        if release["draft"] or release["prerelease"]:
            continue
        tag = release["tag_name"]
        if not tag.startswith("v"):
            raise ValueError("Published releases must use vX.Y.Z tags.")
        versions.append(Version.read(tag[1:]))
    return str(max(versions)) if versions else None
