"""Publication needs an exact reviewed merge, not a dispatch or movable ref."""
from copy import deepcopy
import unittest
from unittest.mock import Mock

from prepare_release import prepare
from release_git import BRANCH, git
import test_prepare_release as fixtures
from validate_release import approved_candidate, tag_commit, validate, workflow_identity


class ApprovalTests(unittest.TestCase):
    def setUp(self):
        fixtures.PreparationTests.setUp(self)
        prepare(self.root, self.api)
        self.candidate = git(self.root, "rev-parse", BRANCH)
        git(self.root, "switch", "-q", "main")
        git(self.root, "merge", "--no-ff", "-qm", "chore(release): approve release", BRANCH)
        self.commit = git(self.root, "rev-parse", "HEAD")
        self.pr = deepcopy(self.api.prs[0])
        self.pr.update(merged=True, merge_commit_sha=self.commit)
        self.client = Mock(repository=self.api.repository)
        self.client.releases.return_value = []
        self.client.request.side_effect = lambda method, path, **kwargs: deepcopy(self.pr) if path == "/pulls/1" else None
        self.event = {"action": "closed", "number": 1, "pull_request": deepcopy(self.pr)}

    def test_only_exact_approved_merge_is_publishable(self):
        record = approved_candidate(self.root, self.pr, self.api.repository)
        self.assertEqual(record["version"], "0.1.0")
        result = validate(self.root, self.event, "pull_request", self.client)
        self.assertEqual(result["commit"], self.commit)
        self.assertFalse(result["preview"])
        self.assertFalse(result["published"])
        for changed in [{"merged": False}, {"base": {"ref": "develop"}},
                        {"merge_commit_sha": self.candidate},
                        {"head": dict(self.pr["head"], ref="release/workflow-bootstrap")},
                        {"head": dict(self.pr["head"], repo={"full_name": "foreign/repository"})}]:
            with self.subTest(changed=changed):
                pr = dict(self.pr, **changed)
                with self.assertRaises(ValueError):
                    approved_candidate(self.root, pr, self.api.repository)

    def test_dispatch_is_always_preview_and_pins_the_requested_sha(self):
        event = {"inputs": {"expected_sha": self.commit, "publish": "true"}}
        result = validate(self.root, event, "workflow_dispatch", self.client)
        self.assertTrue(result["preview"])
        event["inputs"]["expected_sha"] = self.candidate
        with self.assertRaisesRegex(ValueError, "moved"):
            validate(self.root, event, "workflow_dispatch", self.client)
        for event_name in ["push", "release", "workflow_run"]:
            with self.assertRaises(ValueError):
                validate(self.root, self.event, event_name, self.client)

    def test_signing_identity_uses_the_event_commit_and_an_allowed_ref(self):
        identity = {"commit": self.commit, "preview": False, "source_branch": "develop"}
        self.assertEqual(workflow_identity(identity, self.commit, "refs/heads/main")["source_ref"], "refs/heads/main")
        with self.assertRaisesRegex(ValueError, "exact build commit"):
            workflow_identity(identity, self.candidate, "refs/heads/main")
        with self.assertRaisesRegex(ValueError, "allowed source ref"):
            workflow_identity(identity, self.commit, "refs/heads/release/next")
        identity["preview"] = True
        self.assertEqual(workflow_identity(identity, self.commit, "refs/heads/release/next")["source_ref"], "refs/heads/release/next")
        for ref in ["refs/heads/feature/unreviewed", "refs/heads/release/hotfix-next", "refs/tags/v0.1.0"]:
            with self.assertRaisesRegex(ValueError, "allowed source ref"):
                workflow_identity(identity, self.commit, ref)

    def test_approval_change_or_new_publication_requires_review(self):
        self.event["pull_request"]["merge_commit_sha"] = self.candidate
        with self.assertRaisesRegex(ValueError, "approval changed"):
            validate(self.root, self.event, "pull_request", self.client)
        self.event["pull_request"] = deepcopy(self.pr)
        self.client.releases.return_value = [{"tag_name": "v0.0.9", "draft": False, "prerelease": False}]
        with self.assertRaisesRegex(ValueError, "history changed"):
            validate(self.root, self.event, "pull_request", self.client)

    def test_tags_are_resolved_not_reassigned_and_published_releases_are_immutable(self):
        published = {"tag_name": "v0.1.0", "draft": False, "immutable": True, "prerelease": False}
        self.client.releases.side_effect = lambda: [deepcopy(published)]
        def response(method, path, **kwargs):
            if path == "/pulls/1":
                return deepcopy(self.pr)
            if path.startswith("/git/ref/tags/"):
                return {"object": {"type": "tag", "sha": "b" * 40}}
            if path.startswith("/git/tags/"):
                return {"object": {"type": "commit", "sha": self.commit}}
            return {"draft": False, "immutable": True, "prerelease": False}
        self.client.request.side_effect = response
        self.assertEqual(tag_commit(self.client, "0.1.0"), self.commit)
        self.assertTrue(validate(self.root, self.event, "pull_request", self.client)["published"])
        original = self.commit
        self.commit = "c" * 40
        with self.assertRaisesRegex(ValueError, "never retag"):
            validate(self.root, self.event, "pull_request", self.client)
        self.commit = original
        published["immutable"] = False
        with self.assertRaisesRegex(ValueError, "immutable"):
            validate(self.root, self.event, "pull_request", self.client)


if __name__ == "__main__":
    unittest.main()
