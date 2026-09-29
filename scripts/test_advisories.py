from copy import deepcopy
from datetime import date
import unittest

from check_advisories import errors


class AdvisoryTests(unittest.TestCase):
    def setUp(self):
        self.config = {"advisories": {"ignore": [{"id": "RUSTSEC-2024-0001", "reason": "upstream migration"}]}}
        self.records = [{"id": "RUSTSEC-2024-0001", "package": "example", "version": "1.0.0", "owner": "maintainer", "reason": "upstream migration", "expires": "2026-10-28"}]
        self.packages = [{"name": "example", "version": "1.0.0"}]
        self.today = date(2026, 10, 27)

    def check(self, records=None):
        return errors(self.config, self.records if records is None else records, self.packages, self.today)

    def test_valid_and_expiry_boundary(self):
        self.assertEqual(self.check(), [])
        self.today = date(2026, 10, 28)
        self.assertTrue(self.check())
        self.today = date(2026, 10, 29)
        self.assertTrue(self.check())

    def test_records_must_match_ignores_and_current_lock(self):
        self.assertTrue(self.check([]))
        self.assertTrue(self.check(self.records + self.records))
        self.packages[0]["version"] = "1.0.1"
        self.assertTrue(self.check())

    def test_missing_review_information_is_rejected(self):
        for field in ["owner", "reason", "expires"]:
            records = deepcopy(self.records)
            records[0].pop(field)
            with self.subTest(field=field):
                self.assertTrue(self.check(records))
        self.records[0]["expires"] = "next month"
        self.assertTrue(self.check())


if __name__ == "__main__":
    unittest.main()
