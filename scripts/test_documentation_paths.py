"""Guard English repository paths and references after documentation renames."""
import json
from pathlib import Path, PurePosixPath
import subprocess
import tempfile
import unittest
from urllib.parse import quote, unquote


ROOT = Path(__file__).resolve().parent.parent
MIGRATIONS = "docs/path-migrations.json"


def non_ascii_paths(paths):
    return [name for name in paths if not name.isascii()]


def obsolete_references(root, paths, moves):
    old_names = {PurePosixPath(name).name for name in moves}
    found = []
    for name in paths:
        # The mapping deliberately preserves old names for historical lookups.
        if name == MIGRATIONS:
            continue
        try:
            text = (root / name).read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue  # Binary assets are not text references.
        for line, text_line in enumerate(unquote(text).splitlines(), 1):
            for old_name in sorted(old_names):
                if old_name in text_line:
                    found.append((name, line, old_name))
    return found


class DocumentationPathTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.paths = subprocess.check_output(
            ["git", "ls-files", "-z"], cwd=ROOT,
        ).decode("utf-8").rstrip("\0").split("\0")
        cls.moves = json.loads((ROOT / MIGRATIONS).read_text(encoding="utf-8"))

    def test_tracked_paths_use_english_filenames(self):
        self.assertTrue(self.paths, "the repository scan must not be empty")
        self.assertEqual(non_ascii_paths(self.paths), [])

    def test_migration_targets_exist_and_original_paths_are_gone(self):
        self.assertTrue(self.moves)
        self.assertEqual(len(set(self.moves.values())), len(self.moves))
        for old, new in self.moves.items():
            with self.subTest(old=old, new=new):
                self.assertTrue(new.isascii())
                self.assertTrue(new.endswith(".zh-CN.md"))
                self.assertIn(new, self.paths)
                self.assertTrue((ROOT / new).is_file())
                self.assertFalse((ROOT / old).exists())

    def test_repository_text_does_not_reference_renamed_files(self):
        self.assertEqual(obsolete_references(ROOT, self.paths, self.moves), [])

    def test_path_guard_detects_non_ascii_names_and_directories(self):
        bad_file = "docs/\u4e2d.md"
        bad_directory = "\u4e2d/guide.md"
        self.assertEqual(
            non_ascii_paths(["docs/guide.zh-CN.md", bad_file, bad_directory]),
            [bad_file, bad_directory],
        )

    def test_reference_guard_detects_literal_and_url_encoded_paths(self):
        old = next(iter(self.moves))
        old_name = PurePosixPath(old).name
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "guide.md").write_text(
                f"[old]({old})\n[encoded]({quote(old)})\n", encoding="utf-8",
            )
            self.assertEqual(
                obsolete_references(root, ["guide.md"], self.moves),
                [("guide.md", 1, old_name), ("guide.md", 2, old_name)],
            )

    def test_only_the_migration_map_may_preserve_old_names(self):
        old = next(iter(self.moves))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            (root / MIGRATIONS).write_text(json.dumps(self.moves), encoding="utf-8")
            (root / "reference.json").write_text(json.dumps({"path": old}, ensure_ascii=False), encoding="utf-8")
            (root / "asset.bin").write_bytes(b"\xff\xfe")
            self.assertEqual(
                obsolete_references(root, [MIGRATIONS, "reference.json", "asset.bin"], self.moves),
                [("reference.json", 1, PurePosixPath(old).name)],
            )

    def test_missing_tracked_text_is_not_silently_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                obsolete_references(Path(directory), ["missing.md"], self.moves)


if __name__ == "__main__":
    unittest.main()
