"""Distribution descriptions bind both platform archives without executing them."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from package_release import TARGETS
from release_manifest import CHECKSUMS, MANIFEST, archive_names, describe, write_exact
import test_package_release as fixtures


class DistributionTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        maker = fixtures.PackageTests()
        for target, name in zip(TARGETS, archive_names("0.1.0")):
            maker.make_archive(self.root / name, target=target)

    def test_checksums_and_manifest_are_exact_and_idempotent_without_execution(self):
        with patch("package_release.check_binary") as smoke:
            result = describe(self.root, "0.1.0", "a" * 40, True)
            self.assertEqual(result, describe(self.root, "0.1.0", "a" * 40, True))
            smoke.assert_not_called()
        self.assertEqual(len(result["archives"]), 2)
        self.assertEqual(json.loads((self.root / MANIFEST).read_text()), result)
        expected = []
        for item in result["archives"]:
            payload = (self.root / item["name"]).read_bytes()
            self.assertEqual(item["bytes"], len(payload))
            self.assertEqual(item["sha256"], hashlib.sha256(payload).hexdigest())
            expected.append(f"{item['sha256']}  {item['name']}\n")
        self.assertEqual((self.root / CHECKSUMS).read_text(), "".join(expected))
        with self.assertRaisesRegex(ValueError, "replace"):
            write_exact(self.root / CHECKSUMS, b"different content")
        self.assertEqual((self.root / CHECKSUMS).read_text(), "".join(expected))

    def test_missing_extra_misidentified_and_symlink_assets_are_rejected(self):
        for commit, preview in [("b" * 40, True), ("a" * 40, False)]:
            with self.assertRaisesRegex(ValueError, "identity mismatch"):
                describe(self.root, "0.1.0", commit, preview)
        extra = self.root / "unexpected.txt"
        extra.write_text("unexpected")
        with self.assertRaisesRegex(ValueError, "Unexpected files"):
            describe(self.root, "0.1.0", "a" * 40, True)
        extra.unlink()
        archive = self.root / archive_names("0.1.0")[0]
        archive.unlink()
        with self.assertRaisesRegex(ValueError, "regular files"):
            describe(self.root, "0.1.0", "a" * 40, True)
        archive.symlink_to(self.root / archive_names("0.1.0")[1])
        with self.assertRaisesRegex(ValueError, "regular files"):
            describe(self.root, "0.1.0", "a" * 40, True)


if __name__ == "__main__":
    unittest.main()
