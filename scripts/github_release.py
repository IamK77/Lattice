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

    def pages(self, path, **parameters):
        results = []
        for page in range(1, 101):
            rows = self.request("GET", path + "?" + urlencode(dict(parameters, per_page=100, page=page)))
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
