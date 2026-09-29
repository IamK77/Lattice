"""Exercise synchronization with real diverged Git histories and a fake API."""
from copy import deepcopy
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from release_git import git
from sync_develop import BRANCH, enable_auto_merge, synchronize
from sync_git import SyncGit


class SyncAPI:
    repository = "IamK77/Lattice"
    token = "test-only-token"

    def __init__(self, root):
        self.root = root
        self.graph = SyncGit(root, self.repository)
        git(root, "init", "-q")
        git(root, "config", "user.name", "Fixture")
        git(root, "config", "user.email", "fixture@example.invalid")
        git(root, "config", "commit.gpgsign", "false")
        self.base = self.commit(None, "base", "base")
        self.main = self.commit(self.base, "stable", "fix")
        self.develop = self.commit(self.base, "development", "feature")
        self.tip = None
        self.prs = []
        self.writes = []
        self.after_merge = None

    def commit(self, parent, name, content):
        if parent:
            git(self.root, "checkout", "--detach", "-q", parent)
        (self.root / name).write_text(content)
        git(self.root, "add", name)
        git(self.root, "commit", "-qm", "chore: fixture change")
        return git(self.root, "rev-parse", "HEAD")

    def refreshed(self, pr):
        result = deepcopy(pr)
        result["head"]["sha"] = self.tip
        return result

    def request(self, method, path, data=None, missing_ok=False):
        if method == "GET" and path.startswith("/git/ref/heads/"):
            name = path.removeprefix("/git/ref/heads/")
            commit = {"main": self.main, "develop": self.develop, BRANCH: self.tip}[name]
            if commit is None and missing_ok:
                return None
            assert commit
            return {"object": {"type": "commit", "sha": commit}}
        if method == "GET" and path == f"/compare/{self.develop}...{self.main}":
            ahead = int(git(self.root, "rev-list", "--count", f"{self.develop}..{self.main}"))
            return {"status": "diverged" if ahead else "behind", "ahead_by": ahead}
        if method == "GET" and path == "/pulls/12":
            return self.refreshed(self.prs[0])
        self.writes.append((method, path, deepcopy(data)))
        if method == "POST" and path == "/git/refs":
            assert data["ref"] == f"refs/heads/{BRANCH}"
            assert self.tip is None
            self.tip = data["sha"]
            return {"object": {"sha": self.tip}}
        if method == "POST" and path == "/merges":
            assert data["base"] == BRANCH
            git(self.root, "checkout", "--detach", "-q", self.tip)
            git(self.root, "merge", "--no-ff", "-m", data["commit_message"], data["head"])
            self.tip = git(self.root, "rev-parse", "HEAD")
            if self.after_merge:
                self.after_merge()
            return {"sha": self.tip}
        if method == "POST" and path == "/pulls":
            result = {"number": 12, "state": "open", "draft": False,
                      "head": {"ref": data["head"], "sha": self.tip, "repo": {"full_name": self.repository}},
                      "base": {"ref": data["base"]}, "user": {"login": "github-actions[bot]"},
                      "title": data["title"], "body": data["body"], "html_url": "https://example.invalid/sync"}
            self.prs.append(result)
            return deepcopy(result)
        raise AssertionError((method, path, data))

    def pull_requests(self, head):
        assert head == BRANCH
        return [self.refreshed(pr) for pr in self.prs]


class SynchronizationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="lattice-sync-test-")
        self.addCleanup(temporary.cleanup)
        self.api = SyncAPI(Path(temporary.name))
        self.merge = Mock()

    def sync(self):
        return synchronize(self.api, self.merge, self.api.graph)

    def test_diverged_histories_get_an_up_to_date_pr_without_mutating_protected_tips(self):
        main, develop = self.api.main, self.api.develop
        self.assertFalse(self.api.graph.ancestor(develop, main))
        self.assertEqual(self.sync()["outcome"], "native-auto-merge-requested")
        tip = self.api.tip
        self.assertNotEqual(tip, main)
        self.assertTrue(self.api.graph.ancestor(main, tip))
        self.assertTrue(self.api.graph.ancestor(develop, tip))
        self.assertEqual((self.api.main, self.api.develop), (main, develop))
        self.assertEqual(self.api.prs[0]["head"]["ref"], BRANCH)
        self.assertEqual(git(self.api.root, "show", f"{tip}:stable"), "fix")
        self.assertEqual(git(self.api.root, "show", f"{tip}:development"), "feature")

    def test_retry_reuses_branch_and_pr_preserving_notes_and_not_writing_again(self):
        self.sync()
        writes = deepcopy(self.api.writes)
        self.api.prs[0]["title"] = "chore: customized synchronization"
        self.api.prs[0]["body"] = "Keep these review notes."
        self.sync()
        self.assertEqual(self.api.writes, writes)
        self.assertEqual(self.api.prs[0]["body"], "Keep these review notes.")
        self.assertEqual(self.merge.call_count, 2)

    def test_both_protected_tips_can_advance_without_new_branches_or_force_updates(self):
        self.sync()
        old_tip = self.api.tip
        self.api.main = self.api.commit(self.api.main, "stable-next", "next fix")
        self.api.develop = self.api.commit(self.api.develop, "development-next", "next feature")
        self.sync()
        self.assertTrue(self.api.graph.ancestor(old_tip, self.api.tip))
        self.assertTrue(self.api.graph.ancestor(self.api.main, self.api.tip))
        self.assertTrue(self.api.graph.ancestor(self.api.develop, self.api.tip))
        self.assertEqual(len(self.api.prs), 1)
        self.assertEqual(sum(path == "/git/refs" for _, path, _ in self.api.writes), 1)
        self.assertFalse(any(method == "PATCH" for method, _, _ in self.api.writes))

    def test_already_contained_history_has_no_write_or_merge(self):
        self.api.main = self.api.base
        self.assertEqual(self.sync()["outcome"], "already-contained")
        self.assertEqual(self.api.writes, [])
        self.merge.assert_not_called()

    def test_existing_human_pr_is_not_automated_or_overwritten(self):
        self.sync()
        self.merge.reset_mock()
        writes = deepcopy(self.api.writes)
        self.api.prs[0]["user"]["login"] = "maintainer"
        self.assertEqual(self.sync()["outcome"], "existing-human-pr")
        self.assertEqual(self.api.writes, writes)
        self.merge.assert_not_called()

    def test_foreign_misdirected_duplicate_or_draft_prs_cannot_gain_auto_merge(self):
        self.sync()
        original = deepcopy(self.api.prs[0])
        changes = [lambda pr: pr["head"].update(ref="main"),
                   lambda pr: pr["head"]["repo"].update(full_name="foreign/repository"),
                   lambda pr: pr["base"].update(ref="main"),
                   lambda pr: pr.update(draft=True), lambda pr: pr.update(number=True)]
        for change in changes:
            with self.subTest(change=change):
                self.api.prs = [deepcopy(original)]
                change(self.api.prs[0])
                self.merge.reset_mock()
                with self.assertRaises(ValueError):
                    self.sync()
                self.merge.assert_not_called()
        self.api.prs = [deepcopy(original), deepcopy(original)]
        with self.assertRaisesRegex(ValueError, "Multiple"):
            self.sync()

    def test_conflicts_stop_before_creating_a_branch_or_pr(self):
        self.api.main = self.api.commit(self.api.base, "base", "main edit")
        self.api.develop = self.api.commit(self.api.base, "base", "develop edit")
        with self.assertRaisesRegex(ValueError, "conflict"):
            self.sync()
        self.assertEqual(self.api.writes, [])
        self.merge.assert_not_called()

    def test_manual_branch_edits_are_not_silently_preserved_or_overwritten(self):
        self.sync()
        self.merge.reset_mock()
        self.api.tip = self.api.commit(self.api.tip, "manual", "do not overwrite")
        writes = deepcopy(self.api.writes)
        with self.assertRaisesRegex(ValueError, "manual changes"):
            self.sync()
        self.assertEqual(self.api.writes, writes)
        self.merge.assert_not_called()

    def test_merge_shaped_manual_edits_are_also_rejected(self):
        edited = self.api.commit(self.api.develop, "manual", "hidden in merge")
        self.api.tip = git(self.api.root, "commit-tree", self.api.graph.tree(edited),
                           "-p", self.api.develop, "-p", self.api.main, "-m", "chore: fake merge")
        with self.assertRaisesRegex(ValueError, "manual changes"):
            self.api.graph.validate(self.api.tip, self.api.main, self.api.develop)
        with self.assertRaisesRegex(ValueError, "manual changes"):
            self.sync()
        self.assertEqual(self.api.writes, [])
        self.merge.assert_not_called()

    def test_partial_branch_creation_can_be_resumed(self):
        self.api.tip = self.api.develop
        self.sync()
        self.assertEqual(len(self.api.prs), 1)
        self.assertFalse(any(path == "/git/refs" for _, path, _ in self.api.writes))

    def test_source_advance_during_run_stops_before_pr_and_auto_merge(self):
        newer = self.api.commit(self.api.develop, "concurrent", "new work")
        self.api.after_merge = lambda: setattr(self.api, "develop", newer)
        with self.assertRaisesRegex(ValueError, "Protected history advanced"):
            self.sync()
        self.assertEqual(self.api.prs, [])
        self.merge.assert_not_called()

    def test_cli_requests_native_checks_with_exact_head_and_never_uses_admin_bypass(self):
        self.sync()
        with patch("sync_develop.subprocess") as process:
            enable_auto_merge(self.api, self.api.prs[0])
        command = process.run.call_args.args[0]
        self.assertIn("--auto", command)
        self.assertIn("--merge", command)
        self.assertEqual(command[command.index("--match-head-commit") + 1], self.api.tip)
        self.assertNotIn("--admin", command)
        self.assertTrue(process.run.call_args.kwargs["check"])

    def test_native_auto_merge_failure_is_visible_and_does_not_retry(self):
        self.merge.side_effect = RuntimeError("Native auto-merge unavailable")
        with self.assertRaisesRegex(RuntimeError, "unavailable"):
            self.sync()
        self.merge.assert_called_once()
        self.assertEqual(len(self.api.prs), 1)


if __name__ == "__main__":
    unittest.main()
