"""Synchronization may request native auto-merge only for its own stable-history PR."""
from copy import deepcopy
import unittest
from unittest.mock import Mock, patch

from sync_develop import enable_auto_merge, synchronize


class SyncAPI:
    repository = "IamK77/Lattice"
    token = "test-only-token"

    def __init__(self):
        self.main = "a" * 40
        self.develop = "b" * 40
        self.ahead = 2
        self.prs = []
        self.writes = []

    def request(self, method, path, data=None):
        if method == "GET" and path.startswith("/git/ref/heads/"):
            return {"object": {"type": "commit", "sha": self.main if path.endswith("/main") else self.develop}}
        if method == "GET" and path == f"/compare/{self.develop}...{self.main}":
            return {"status": "diverged" if self.ahead else "behind", "ahead_by": self.ahead}
        if method == "POST" and path == "/pulls":
            self.writes.append(deepcopy(data))
            result = {"number": 12, "state": "open", "draft": False,
                      "head": {"ref": data["head"], "sha": self.main, "repo": {"full_name": self.repository}},
                      "base": {"ref": data["base"]}, "user": {"login": "github-actions[bot]"},
                      "title": data["title"], "body": data["body"], "html_url": "https://example.invalid/sync"}
            self.prs.append(result)
            return deepcopy(result)
        raise AssertionError((method, path, data))

    def pull_requests(self, head):
        assert head == "main"
        return deepcopy(self.prs)


class SynchronizationTests(unittest.TestCase):
    def setUp(self):
        self.api = SyncAPI()
        self.merge = Mock()

    def test_only_a_pr_is_created_and_retries_reuse_it_without_changing_human_notes(self):
        result = synchronize(self.api, self.merge)
        self.assertEqual(result["outcome"], "native-auto-merge-requested")
        self.assertEqual(len(self.api.writes), 1)
        self.assertEqual(self.api.writes[0]["head"], "main")
        self.assertEqual(self.api.writes[0]["base"], "develop")
        self.api.prs[0]["title"] = "chore: maintainer-customized synchronization"
        self.api.prs[0]["body"] = "Keep these review notes."
        synchronize(self.api, self.merge)
        self.assertEqual(len(self.api.writes), 1)
        self.assertEqual(self.api.prs[0]["body"], "Keep these review notes.")
        self.assertEqual(self.merge.call_count, 2)

    def test_already_contained_history_has_no_write_or_merge(self):
        self.api.ahead = 0
        self.assertEqual(synchronize(self.api, self.merge)["outcome"], "already-contained")
        self.assertEqual(self.api.writes, [])
        self.merge.assert_not_called()

    def test_existing_human_pr_is_not_automated_or_overwritten(self):
        synchronize(self.api, self.merge)
        self.merge.reset_mock()
        self.api.prs[0]["user"]["login"] = "maintainer"
        result = synchronize(self.api, self.merge)
        self.assertEqual(result["outcome"], "existing-human-pr")
        self.assertEqual(len(self.api.writes), 1)
        self.merge.assert_not_called()

    def test_foreign_misdirected_duplicate_or_draft_prs_cannot_gain_auto_merge(self):
        synchronize(self.api, self.merge)
        original = deepcopy(self.api.prs[0])
        changes = [lambda pr: pr["head"].update(ref="release/next"),
                   lambda pr: pr["head"]["repo"].update(full_name="foreign/repository"),
                   lambda pr: pr.update(draft=True), lambda pr: pr.update(number=True)]
        for change in changes:
            with self.subTest(change=change):
                self.api.prs = [deepcopy(original)]
                change(self.api.prs[0])
                self.merge.reset_mock()
                with self.assertRaises(ValueError):
                    synchronize(self.api, self.merge)
                self.merge.assert_not_called()
        self.api.prs = [deepcopy(original), deepcopy(original)]
        with self.assertRaisesRegex(ValueError, "Multiple"):
            synchronize(self.api, self.merge)

    def test_cli_requests_native_checks_with_exact_head_and_never_uses_admin_bypass(self):
        synchronize(self.api, self.merge)
        with patch("sync_develop.subprocess") as process:
            enable_auto_merge(self.api, self.api.prs[0])
        command = process.run.call_args.args[0]
        self.assertIn("--auto", command)
        self.assertIn("--merge", command)
        self.assertEqual(command[command.index("--match-head-commit") + 1], self.api.main)
        self.assertNotIn("--admin", command)
        self.assertTrue(process.run.call_args.kwargs["check"])

    def test_native_auto_merge_failure_is_visible_and_does_not_retry(self):
        self.merge.side_effect = RuntimeError("Native auto-merge unavailable")
        with self.assertRaisesRegex(RuntimeError, "unavailable"):
            synchronize(self.api, self.merge)
        self.merge.assert_called_once()
        self.assertEqual(len(self.api.writes), 1)


if __name__ == "__main__":
    unittest.main()
