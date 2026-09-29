"""Exercise candidate creation against real Git objects and a local API stand-in."""
from copy import deepcopy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from prepare_release import prepare, replace_description
from release_git import BRANCH, FILES, RECORD, candidate, commits_since, git, verify_candidate
from github_release import GitHub, NoRedirect, latest_published


class LocalGitHub:
    repository = "example/lattice"

    def __init__(self, root):
        self.root = root
        self.prs = []
        self.mutations = []
        self.fail_create_pr = False
        self.published = []
        self.follow_closed_heads = False

    def releases(self):
        return deepcopy(self.published)

    def pull_requests(self, head, state="open"):
        result = deepcopy([pr for pr in self.prs if pr["head"]["ref"] == head])
        for pr in result:
            if pr["state"] == "open" or self.follow_closed_heads:
                pr["head"]["sha"] = git(self.root, "rev-parse", f"refs/heads/{head}")
        return result

    def request(self, method, path, data=None, missing_ok=False):
        if method != "GET":
            self.mutations.append((method, path, data))
        if method == "GET" and path.startswith("/git/ref/heads/"):
            ref = "refs/heads/" + path.removeprefix("/git/ref/heads/")
            result = subprocess.run(["git", "-C", str(self.root), "rev-parse", "--verify", ref], capture_output=True, text=True)
            if result.returncode and missing_ok:
                return None
            if result.returncode:
                raise ValueError("Missing ref")
            return {"object": {"sha": result.stdout.strip()}}
        if method == "GET" and path.startswith("/git/commits/"):
            return {"tree": {"sha": git(self.root, "rev-parse", path.split("/")[-1] + "^{tree}")}}
        if path == "/git/trees":
            with tempfile.TemporaryDirectory() as directory:
                env = dict(os.environ, GIT_INDEX_FILE=str(Path(directory) / "index"))
                command = ["git", "-C", str(self.root)]
                subprocess.run([*command, "read-tree", data["base_tree"]], env=env, check=True)
                for entry in data["tree"]:
                    blob = subprocess.check_output([*command, "hash-object", "-w", "--stdin"], input=entry["content"], text=True).strip()
                    subprocess.run([*command, "update-index", "--add", "--cacheinfo", f"{entry['mode']},{blob},{entry['path']}"], env=env, check=True)
                return {"sha": subprocess.check_output([*command, "write-tree"], env=env, text=True).strip()}
        if path == "/git/commits":
            args = ["commit-tree", data["tree"], "-m", data["message"]]
            for parent in data["parents"]:
                args.extend(["-p", parent])
            return {"sha": git(self.root, *args)}
        if path == "/git/refs":
            git(self.root, "update-ref", data["ref"], data["sha"], "0" * 40)
            return {}
        if path.startswith("/git/refs/heads/"):
            assert data["force"] is False
            ref = "refs/heads/" + path.removeprefix("/git/refs/heads/")
            old = git(self.root, "rev-parse", ref)
            git(self.root, "merge-base", "--is-ancestor", old, data["sha"])
            git(self.root, "update-ref", ref, data["sha"], old)
            return {}
        if path == "/pulls":
            if self.fail_create_pr:
                raise RuntimeError("Injected API failure after branch creation")
            pr = {"number": len(self.prs) + 1, "state": "open", "merged_at": None,
                  "user": {"login": "github-actions[bot]"}, "base": {"ref": data["base"]},
                  "head": {"ref": data["head"], "sha": git(self.root, "rev-parse", f"refs/heads/{data['head']}"), "repo": {"full_name": self.repository}},
                  "title": data["title"], "body": data["body"], "html_url": "https://example.invalid/pr"}
            self.prs.append(pr)
            return deepcopy(pr)
        if path.startswith("/pulls/"):
            pr = self.prs[int(path.split("/")[-1]) - 1]
            pr.update(data)
            return deepcopy(pr)
        raise AssertionError((method, path))


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        project = Path(__file__).resolve().parent.parent
        for name in FILES:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text((project / name).read_text())
        git(self.root, "init", "-q", "-b", "develop")
        for key, value in [("user.name", "Release Test"), ("user.email", "release@example.invalid"), ("commit.gpgsign", "false"), ("core.hooksPath", str(self.root / "no-hooks"))]:
            git(self.root, "config", key, value)
        git(self.root, "add", ".")
        git(self.root, "commit", "-qm", "feat: initialize")
        git(self.root, "branch", "main")
        self.api = LocalGitHub(self.root)
        git(self.root, "config", "core.autocrlf", "false")

    def publish_first(self):
        prepare(self.root, self.api)
        self.api.prs[0]["head"]["sha"] = git(self.root, "rev-parse", BRANCH)
        git(self.root, "switch", "-q", "main")
        git(self.root, "merge", "--no-ff", "-qm", "chore(release): publish initial version", BRANCH)
        git(self.root, "tag", "v0.1.0")
        self.api.prs[0].update(state="closed", merged_at="2026-01-01T00:00:00Z")
        self.api.published.append({"tag_name": "v0.1.0", "draft": False, "prerelease": False})
        git(self.root, "switch", "-q", "develop")
        git(self.root, "merge", "--no-ff", "-qm", "chore: synchronize stable release", "main")

    def change_version(self, version):
        from release_plan import cargo_manifest
        path = self.root / "Cargo.toml"
        path.write_text(cargo_manifest(path.read_text(), version))
        git(self.root, "add", "Cargo.toml")
        git(self.root, "commit", "-qm", "chore: adjust explicit release version")

    def test_second_release_recovers_partial_creation_and_advancing_source(self):
        self.publish_first()
        self.api.follow_closed_heads = True
        git(self.root, "commit", "--allow-empty", "-qm", "fix: correction after first release")
        self.api.fail_create_pr = True
        with self.assertRaisesRegex(RuntimeError, "Injected API"):
            prepare(self.root, self.api)
        partial = git(self.root, "rev-parse", BRANCH)
        git(self.root, "commit", "--allow-empty", "-qm", "feat: new capability")
        self.api.fail_create_pr = False
        prepare(self.root, self.api)
        self.assertEqual(len(self.api.prs), 2)
        git(self.root, "merge-base", "--is-ancestor", partial, BRANCH)
        record = verify_candidate(self.root, git(self.root, "rev-parse", BRANCH))
        self.assertEqual((record["version"], record["previous"]), ("0.2.0", "0.1.0"))

    def test_withdrawn_override_closes_stale_candidate_without_losing_notes(self):
        self.publish_first()
        notes = self.root / "CHANGELOG.md"
        notes.write_text(notes.read_text().replace("## [Unreleased]\n", "## [Unreleased]\n\n### Changed\n\n- Curated maintenance release.\n", 1))
        git(self.root, "add", "CHANGELOG.md")
        git(self.root, "commit", "-qm", "docs: curate maintenance release notes")
        self.change_version("0.2.0")
        prepare(self.root, self.api)
        self.api.prs[-1]["body"] += "\nKeep these review notes."
        self.change_version("0.1.0")
        self.assertIsNone(prepare(self.root, self.api))
        self.assertEqual(self.api.prs[-1]["state"], "closed")
        self.assertIn("withdrawn", self.api.prs[-1]["body"])
        self.assertTrue(self.api.prs[-1]["body"].endswith("Keep these review notes."))

    def test_manual_title_survives_same_version_and_version_change(self):
        prepare(self.root, self.api)
        title = "chore(release): ship with explicit maintainer context"
        self.api.prs[0]["title"] = title
        prepare(self.root, self.api)
        self.assertEqual(self.api.prs[0]["title"], title)
        self.change_version("0.2.0")
        prepare(self.root, self.api)
        self.assertEqual(self.api.prs[0]["title"], title)
        self.assertIn("v0.2.0", self.api.prs[0]["body"])

    def test_crlf_git_blobs_are_preserved_and_repreparation_is_idempotent(self):
        for name in ["Cargo.toml", "Cargo.lock"]:
            path = self.root / name
            path.write_bytes(path.read_bytes().replace(b"\n", b"\r\n"))
        git(self.root, "add", ".")
        git(self.root, "commit", "-qm", "chore: retain CRLF source formatting")
        prepare(self.root, self.api)
        first = git(self.root, "rev-parse", BRANCH)
        for name in ["Cargo.toml", "Cargo.lock"]:
            actual = subprocess.check_output(["git", "-C", str(self.root), "show", f"{first}:{name}"])
            self.assertEqual(actual, (self.root / name).read_bytes())
        prepare(self.root, self.api)
        self.assertEqual(git(self.root, "rev-parse", BRANCH), first)

    def test_create_refresh_and_idempotent_prepare_preserve_ancestry(self):
        prepare(self.root, self.api)
        first = git(self.root, "rev-parse", BRANCH)
        self.assertEqual(verify_candidate(self.root, first)["version"], "0.1.0")
        prepare(self.root, self.api)
        self.assertEqual(git(self.root, "rev-parse", BRANCH), first)
        self.assertEqual(len(self.api.prs), 1)
        self.api.prs[0]["body"] += "\nMaintainer review notes."
        git(self.root, "commit", "--allow-empty", "-qm", "fix: improve behavior")
        source = git(self.root, "rev-parse", "HEAD")
        prepare(self.root, self.api)
        refreshed = git(self.root, "rev-parse", BRANCH)
        self.assertNotEqual(first, refreshed)
        git(self.root, "merge-base", "--is-ancestor", first, refreshed)
        git(self.root, "merge-base", "--is-ancestor", source, refreshed)
        self.assertEqual(verify_candidate(self.root, refreshed)["source"], source)
        self.assertTrue(self.api.prs[0]["body"].endswith("Maintainer review notes."))
        self.assertEqual(len(commits_since(self.root, refreshed, None)), 3)

    def test_partial_creation_is_recovered_even_if_development_advances(self):
        self.api.fail_create_pr = True
        with self.assertRaisesRegex(RuntimeError, "Injected API"):
            prepare(self.root, self.api)
        first = git(self.root, "rev-parse", BRANCH)
        git(self.root, "commit", "--allow-empty", "-qm", "fix: another correction")
        self.api.fail_create_pr = False
        prepare(self.root, self.api)
        git(self.root, "merge-base", "--is-ancestor", first, BRANCH)
        self.assertEqual(len(self.api.prs), 1)
        verify_candidate(self.root, git(self.root, "rev-parse", BRANCH))

    def test_manual_candidate_changes_and_closed_pr_are_not_overwritten(self):
        prepare(self.root, self.api)
        git(self.root, "switch", "-q", BRANCH)
        (self.root / "manual.txt").write_text("Keep this change.\n")
        git(self.root, "add", "manual.txt")
        git(self.root, "commit", "-qm", "fix: manually stabilize")
        git(self.root, "switch", "-q", "develop")
        self.api.mutations.clear()
        with self.assertRaisesRegex(ValueError, "manual code changes"):
            prepare(self.root, self.api)
        self.assertEqual(self.api.mutations, [])
        self.api.prs[0]["state"] = "closed"
        self.assertIsNone(prepare(self.root, self.api))
        self.assertEqual(self.api.mutations, [])

    def test_hotfix_candidate_does_not_include_unreleased_development(self):
        git(self.root, "commit", "--allow-empty", "-qm", "feat: unreleased capability")
        development = git(self.root, "rev-parse", "HEAD")
        git(self.root, "switch", "-q", "main")
        git(self.root, "commit", "--allow-empty", "-qm", "fix: repair stable behavior")
        prepare(self.root, self.api, source_branch="main")
        head = git(self.root, "rev-parse", "release/hotfix-next")
        record = verify_candidate(self.root, head)
        self.assertEqual(record["source_branch"], "main")
        self.assertNotIn("unreleased capability", git(self.root, "show", f"{head}:CHANGELOG.md"))
        result = subprocess.run(["git", "-C", str(self.root), "merge-base", "--is-ancestor", development, head], check=False)
        self.assertEqual(result.returncode, 1)

    def test_source_symlinks_are_not_release_inputs(self):
        path = self.root / "Cargo.toml"
        original = path.read_text()
        path.unlink()
        (self.root / "elsewhere.toml").write_text(original)
        path.symlink_to("elsewhere.toml")
        git(self.root, "add", ".")
        git(self.root, "commit", "-qm", "chore: change manifest layout")
        with self.assertRaisesRegex(ValueError, "regular non-executable"):
            candidate(self.root, git(self.root, "rev-parse", "HEAD"), None)
        self.assertEqual(self.api.mutations, [])


class ApiPolicyTests(unittest.TestCase):
    def test_latest_means_highest_published_stable_not_a_draft(self):
        releases = [{"tag_name": version, "draft": draft, "prerelease": prerelease} for version, draft, prerelease in [
            ("v1.2.0", False, False), ("v1.1.9", False, False), ("v9.0.0", True, False), ("v2.0.0-rc.1", False, True),
        ]]
        self.assertEqual(latest_published(releases), "1.2.0")
        self.assertIsNone(latest_published([]))

    def test_tokens_cannot_be_redirected_and_repository_paths_are_bounded(self):
        self.assertIsNone(NoRedirect().redirect_request(None, None, 302, "", {}, "https://example.invalid"))
        with self.assertRaises(ValueError):
            GitHub("example/repo/../../other", "test-token")
        api = GitHub("example/repo", "test-token")
        with self.assertRaises(ValueError):
            api.request("GET", "https://example.invalid")
        with self.assertRaises(ValueError):
            replace_description("Manual body without generated markers", "replacement")


if __name__ == "__main__":
    unittest.main()
