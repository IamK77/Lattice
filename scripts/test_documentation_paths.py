"""Guard English repository paths and the current documentation layout."""
from pathlib import Path, PurePosixPath
import subprocess
import unittest


ROOT = Path(__file__).resolve().parent.parent
DIRECTORIES = {"guides", "development", "contracts", "maintenance", "records"}


def non_ascii_paths(paths):
    return [name for name in paths if not name.isascii()]


class DocumentationPathTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.paths = subprocess.check_output(
            ["git", "ls-files", "-z"], cwd=ROOT,
        ).decode("utf-8").rstrip("\0").split("\0")

    def test_tracked_paths_use_english_filenames(self):
        self.assertTrue(self.paths, "the repository scan must not be empty")
        self.assertEqual(non_ascii_paths(self.paths), [])

    def test_shallow_layout_and_bilingual_directory_indexes(self):
        root_entries = {"docs/README.md", "docs/README.zh-CN.md"}
        self.assertEqual(
            {name for name in self.paths if PurePosixPath(name).parent == PurePosixPath("docs")},
            root_entries,
        )
        for name in self.paths:
            if not name.startswith("docs/") or not name.endswith(".md"):
                continue
            with self.subTest(document=name):
                parts = PurePosixPath(name).parts
                if len(parts) == 2:
                    self.assertIn(name, root_entries, "articles belong in reader directories")
                else:
                    self.assertEqual(len(parts), 3, "keep the approved layout shallow")
                    self.assertIn(parts[1], DIRECTORIES)
        for directory in DIRECTORIES:
            for suffix in [".md", ".zh-CN.md"]:
                self.assertIn(f"docs/{directory}/README{suffix}", self.paths)

    def test_current_designs_and_historical_evidence_have_distinct_homes(self):
        for name in [
            "docs/development/design-action-authorization.zh-CN.md",
            "docs/development/design-expert-execution.zh-CN.md",
            "docs/records/design-expert-snapshot-prototype.zh-CN.md",
            "docs/records/expert-management-prototype.zh-CN.md",
            "docs/records/signed-preview-acceptance.md",
            "docs/records/signed-preview-acceptance.zh-CN.md",
        ]:
            with self.subTest(document=name):
                self.assertIn(name, self.paths)
                self.assertEqual(list((ROOT / "docs").rglob(PurePosixPath(name).name)), [ROOT / name])

    def test_path_guard_detects_non_ascii_names_and_directories(self):
        bad_file = "docs/\u4e2d.md"
        bad_directory = "\u4e2d/guide.md"
        self.assertEqual(
            non_ascii_paths(["docs/guide.zh-CN.md", bad_file, bad_directory]),
            [bad_file, bad_directory],
        )


if __name__ == "__main__":
    unittest.main()
