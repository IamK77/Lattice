"""Regression checks for the public contribution workflow."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_workflow as policy


class BranchTests(unittest.TestCase):
    def test_allowed_routes(self):
        for base, head in [
            ("develop", "feature/docs"), ("develop", "release/0.2.0"),
            ("develop", "hotfix/startup"), ("develop", "main"),
            ("main", "release/0.2.0"), ("main", "hotfix/startup"),
        ]:
            with self.subTest(base=base, head=head):
                self.assertIsNone(policy.branch_error(base, head))

    def test_invalid_routes(self):
        for base, head in [
            ("main", "feature/docs"), ("main", "develop"),
            ("develop", "docs"), ("develop", "feature/"),
            ("other", "feature/docs"), ("main", "release/"),
        ]:
            with self.subTest(base=base, head=head):
                self.assertIsNotNone(policy.branch_error(base, head))
        self.assertIsNotNone(policy.branch_error("develop", "main", False))
        self.assertIsNone(policy.branch_error("develop", "feature/fork", False))


class MessageTests(unittest.TestCase):
    def test_all_types_and_merge(self):
        for kind in policy.TYPES:
            self.assertEqual(policy.message_errors(f"完善贡献流程\n\nType: {kind}\n"), [])
        self.assertEqual(policy.message_errors("合并开发分支\n\nType: chore", merge=True), [])

    def test_invalid_messages(self):
        for message in [
            "", "Update workflow\n\nType: chore", "更新流程", "更新流程\n\nType: build",
            "增加新功能\nType: feat",
            "docs: 更新流程\n\nType: docs", "feat(cli)!: 更新流程\n\nType: feat",
            "更新流程\n\nType: chore\nextra", "更新流程\n\nType: docs\nType: chore",
        ]:
            with self.subTest(message=message):
                self.assertTrue(policy.message_errors(message))
        self.assertTrue(policy.message_errors("合并功能\n\nType: feat", merge=True))


class EventTests(unittest.TestCase):
    @patch.object(policy, "check_range", return_value=[])
    def test_pull_request_uses_actual_head_not_synthetic_merge(self, check):
        event = {"pull_request": {
            "title": "合并贡献流程", "body": "验证说明\n\nType: chore",
            "base": {"ref": "develop", "sha": "base", "repo": {"id": 1}},
            "head": {"ref": "feature/docs", "sha": "head", "repo": {"id": 2}},
        }}
        self.assertEqual(policy.check_event("pull_request", event), [])
        check.assert_called_once_with("base", "head")
        event["pull_request"]["base"]["ref"] = "main"
        self.assertTrue(policy.check_event("pull_request", event))
        event["pull_request"]["base"]["ref"] = "develop"
        event["pull_request"]["body"] = "Type: feat"
        self.assertTrue(policy.check_event("pull_request", event))
        event["pull_request"]["body"] = None
        self.assertTrue(policy.check_event("pull_request", event))

    @patch.object(policy, "check_range", return_value=[])
    def test_push_and_delete(self, check):
        self.assertEqual(policy.check_event("push", {"before": "old", "after": "new"}), [])
        check.assert_called_once_with("old", "new")
        check.reset_mock()
        self.assertEqual(policy.check_event("push", {"deleted": True}), [])
        check.assert_not_called()
        self.assertTrue(policy.check_event("unknown", {}))


class GitRangeTests(unittest.TestCase):
    def test_hotfix_sync_keeps_unreleased_work_off_main(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", "-C", directory, *args], text=True).strip()

            git("init", "-q", "-b", "main")
            git("config", "user.name", "Workflow Test")
            git("config", "user.email", "workflow@example.invalid")
            git("config", "commit.gpgsign", "false")
            git("config", "core.hooksPath", str(Path(directory) / "no-hooks"))
            git("commit", "--allow-empty", "-qm", "初始化测试\n\nType: chore")
            git("switch", "-qc", "develop")
            git("commit", "--allow-empty", "-qm", "加入未发布功能\n\nType: feat")
            development = git("rev-parse", "HEAD")
            git("switch", "-qc", "hotfix/startup", "main")
            git("commit", "--allow-empty", "-qm", "修正稳定线\n\nType: fix")
            git("switch", "-q", "main")
            git("merge", "--no-ff", "-qm", "合并紧急修复\n\nType: chore", "hotfix/startup")
            stable = git("rev-parse", "main")
            ancestry = subprocess.run(
                ["git", "-C", directory, "merge-base", "--is-ancestor", development, "main"],
                check=False,
            )
            self.assertEqual(ancestry.returncode, 1)
            git("switch", "-qc", "hotfix/sync-main", "main")
            git("merge", "--no-ff", "-qm", "同步开发目标\n\nType: chore", "develop")
            git("merge-base", "--is-ancestor", "develop", "HEAD")
            self.assertIsNone(policy.branch_error("develop", "hotfix/sync-main"))
            with patch.object(policy, "git", side_effect=git):
                self.assertEqual(policy.check_range("develop", "HEAD"), [])
            git("switch", "-q", "develop")
            git("merge", "--no-ff", "-qm", "带回稳定线修复\n\nType: chore", "hotfix/sync-main")
            git("merge-base", "--is-ancestor", stable, "develop")
            self.assertEqual(git("rev-parse", "main"), stable)

    def test_real_history_range_and_merge(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()

            git("init", "-q", "-b", "develop")
            git("config", "user.name", "Workflow Test")
            git("config", "user.email", "workflow@example.invalid")
            git("config", "commit.gpgsign", "false")
            git("config", "core.hooksPath", str(root / "no-hooks"))
            unparsed_message = "增加新功能\nType: feat"
            git("commit", "--allow-empty", "-qm", unparsed_message)
            self.assertEqual(git("show", "-s", "--format=%(trailers:key=Type,valueonly)"), "")
            self.assertTrue(policy.message_errors(unparsed_message))
            base = git("rev-parse", "HEAD")
            git("switch", "-qc", "feature/test")
            git("commit", "--allow-empty", "-qm", "增加检查\n\nType: feat")
            good = git("rev-parse", "HEAD")
            git("switch", "-q", "develop")
            git("merge", "--no-ff", "-qm", "合并检查\n\nType: chore", "feature/test")
            merged = git("rev-parse", "HEAD")
            git("commit", "--allow-empty", "-qm", "bad subject\n\nType: docs")
            bad = git("rev-parse", "HEAD")
            # Git -C supplies an isolated repository without changing process cwd.
            with patch.object(policy, "git", side_effect=git):
                self.assertEqual(policy.check_range(base, good), [])
                self.assertEqual(policy.check_range(base, merged), [])
                errors = policy.check_range(base, bad)
                self.assertEqual(len(errors), 1)
                self.assertIn(bad[:12], errors[0])
                self.assertTrue(policy.check_range("0" * 40, good))
                self.assertEqual(policy.check_range(good, good), [])
            # This is the same trailer traversal used by build.rs. The merge
            # must preserve the feature once, without adding another increment.
            trailers = git("log", "--format=%(trailers:key=Type,valueonly,separator=)", f"{base}..{merged}")
            self.assertEqual(trailers.splitlines().count("feat"), 1)
            self.assertEqual(trailers.splitlines().count("chore"), 1)
            event_path = root / "event.json"
            env = dict(os.environ, GITHUB_EVENT_NAME="push", GITHUB_EVENT_PATH=str(event_path))
            for head, expected in [(merged, 0), (bad, 1)]:
                event_path.write_text(json.dumps({"before": base, "after": head}))
                result = subprocess.run(
                    [sys.executable, str(Path(policy.__file__).resolve())],
                    cwd=root, env=env, text=True, capture_output=True,
                )
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
