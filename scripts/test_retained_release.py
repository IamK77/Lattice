"""Recovery selects one original signed distribution, never a rebuilt substitute."""
from copy import deepcopy
import unittest
from unittest.mock import Mock

from retained_release import retained_artifact


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.commit = "a" * 40
        self.record = {"id": 73, "name": "sealed-release-" + self.commit, "expired": False,
                       "workflow_run": {"head_sha": self.commit}, "digest": "sha256:" + "b" * 64}
        self.api = Mock()
        self.api.pages.return_value = [deepcopy(self.record)]

    def test_exact_retained_distribution_is_selected_by_run_and_source(self):
        self.assertEqual(retained_artifact(self.api, "123", self.commit), "73")
        self.api.pages.assert_called_once_with("/actions/runs/123/artifacts", list_key="artifacts")
        self.api.pages.return_value = []
        self.assertEqual(retained_artifact(self.api, "123", self.commit), "")

    def test_expired_foreign_unsigned_and_duplicate_artifacts_are_not_rebuilt_silently(self):
        for changed in [{"expired": True}, {"workflow_run": {"head_sha": "c" * 40}},
                        {"digest": None}, {"id": True}]:
            with self.subTest(changed=changed):
                self.api.pages.return_value = [dict(self.record, **changed)]
                with self.assertRaises(ValueError):
                    retained_artifact(self.api, "123", self.commit)
        self.api.pages.return_value = [deepcopy(self.record), deepcopy(self.record)]
        with self.assertRaisesRegex(ValueError, "Multiple"):
            retained_artifact(self.api, "123", self.commit)
        for run_id in [True, "../other", "0"]:
            with self.assertRaisesRegex(ValueError, "run identifier"):
                retained_artifact(self.api, run_id, self.commit)


if __name__ == "__main__":
    unittest.main()
