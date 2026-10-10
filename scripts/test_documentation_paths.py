"""Guard English repository paths and references after documentation renames."""
import json
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import tempfile
import unittest
from urllib.parse import quote, unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
MIGRATIONS = "docs/path-migrations.json"
REDIRECTS = "docs/path-redirects.json"
DIRECTORIES = {"guides", "development", "contracts", "maintenance", "records"}


def non_ascii_paths(paths):
    return [name for name in paths if not name.isascii()]


def obsolete_references(root, paths, moves):
    # Removed non-ASCII basenames remain unambiguous anywhere. English basenames
    # can legitimately survive a directory move or belong to another component.
    tokens = {old: old if PurePosixPath(old).name.isascii() else PurePosixPath(old).name
              for old in moves}
    found = []
    for name in paths:
        # Only migration metadata deliberately preserves old path references.
        if name in {MIGRATIONS, REDIRECTS}:
            continue
        try:
            text = (root / name).read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue  # Binary assets are not text references.
        for line, text_line in enumerate(unquote(text).splitlines(), 1):
            hits = {token for token in tokens.values() if token in text_line}
            if name.endswith(".md"):
                for link in re.findall(r"\]\(([^)\n]+)\)", text_line):
                    parsed = urlsplit(link)
                    if parsed.scheme or parsed.netloc or not parsed.path:
                        continue
                    target = posixpath.normpath(posixpath.join(posixpath.dirname(name), parsed.path))
                    if target in tokens:
                        hits.add(tokens[target])
            found.extend((name, line, token) for token in sorted(hits))
    return found


def redirect_targets(text):
    lines = text.splitlines()
    if not lines or not lines[0].startswith("# "):
        raise ValueError("a documentation redirect must start with a heading")
    targets = []
    for line in lines[1:]:
        if not line.strip():
            continue
        match = re.fullmatch(r'(?:- <a id="([^\"]+)"></a> )?\[[^\]]+\]\(([^)]+)\)', line)
        if not match:
            raise ValueError("a documentation redirect contains links, not a second article")
        anchor, target = match.groups()
        if anchor is not None and anchor != unquote(urlsplit(target).fragment):
            raise ValueError("a legacy bookmark must link to its own chapter")
        targets.append(target)
    if not targets:
        raise ValueError("a documentation redirect must have a destination")
    return targets


class DocumentationPathTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.paths = subprocess.check_output(
            ["git", "ls-files", "-z"], cwd=ROOT,
        ).decode("utf-8").rstrip("\0").split("\0")
        cls.moves = json.loads((ROOT / MIGRATIONS).read_text(encoding="utf-8"))
        cls.redirects = json.loads((ROOT / REDIRECTS).read_text(encoding="utf-8"))

    def test_tracked_paths_use_english_filenames(self):
        self.assertTrue(self.paths, "the repository scan must not be empty")
        self.assertEqual(non_ascii_paths(self.paths), [])

    def test_migration_targets_are_canonical_and_old_paths_are_removed_or_redirected(self):
        self.assertTrue(self.moves)
        self.assertTrue(set(self.redirects) <= self.moves.keys())
        self.assertIsInstance(self.redirects, dict)
        for old, new in self.moves.items():
            with self.subTest(old=old, new=new):
                self.assertTrue(new.isascii())
                self.assertTrue(new.startswith("docs/") and new.endswith(".md"))
                self.assertNotIn(new, self.moves, "historical aliases must point directly to the final article")
                self.assertIn(new, self.paths)
                self.assertTrue((ROOT / new).is_file())
                self.assertEqual((ROOT / old).exists(), old in self.redirects)

    def test_shallow_layout_and_bilingual_directory_indexes(self):
        root_entries = {"docs/README.md", "docs/README.zh-CN.md", *self.redirects}
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

    def test_legacy_entry_redirects_link_only_to_the_canonical_article(self):
        for old, expected in self.redirects.items():
            with self.subTest(document=old):
                self.assertTrue(expected, "preserve the independently recorded legacy bookmarks")
                self.assertEqual(len(expected), len(set(expected)))
                self.assertIn(old, self.paths)
                text = (ROOT / old).read_text(encoding="utf-8")
                targets = redirect_targets(text)
                fragments = set()
                for link in targets:
                    parsed = urlsplit(link)
                    self.assertFalse(parsed.scheme or parsed.netloc or parsed.query)
                    self.assertEqual((ROOT / old).parent.joinpath(unquote(parsed.path)).resolve(),
                                     (ROOT / self.moves[old]).resolve())
                    if parsed.fragment:
                        fragments.add(unquote(parsed.fragment))
                ids = re.findall(r'<a id="([^\"]+)">', text)
                self.assertEqual(len(ids), len(expected))
                self.assertEqual(set(ids), set(expected))
                self.assertEqual(fragments, set(expected))

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
        old = next(old for old in self.moves if not PurePosixPath(old).name.isascii())
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

    def test_only_migration_metadata_may_preserve_old_names(self):
        old = next(old for old in self.moves if not PurePosixPath(old).name.isascii())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            (root / MIGRATIONS).write_text(json.dumps(self.moves), encoding="utf-8")
            (root / REDIRECTS).write_text(json.dumps([old], ensure_ascii=False), encoding="utf-8")
            (root / "reference.json").write_text(json.dumps({"path": old}, ensure_ascii=False), encoding="utf-8")
            (root / "asset.bin").write_bytes(b"\xff\xfe")
            self.assertEqual(
                obsolete_references(root, [MIGRATIONS, REDIRECTS, "reference.json", "asset.bin"], self.moves),
                [("reference.json", 1, PurePosixPath(old).name)],
            )

    def test_directory_moves_detect_old_paths_without_rejecting_reused_basenames(self):
        moves = {"docs/guide.md": "docs/guides/guide.md",
                 "docs/reference.md": "docs/development/README.md"}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = "docs/development/README.md"
            (root / source).parent.mkdir(parents=True)
            (root / source).write_text(
                "[old](../guide.md#part)\n[new](../guides/guide.md)\n"
                "[other](../../clients/ink/reference.md)\n[renamed](../reference.md)\n"
                "[encoded](%2E%2E/guide.md)\n", encoding="utf-8",
            )
            (root / "config.yml").write_text(
                "url: https://example.invalid/blob/main/docs/guide.md\n"
                "path: docs/guides/guide.md\n", encoding="utf-8",
            )
            self.assertEqual(obsolete_references(root, [source, "config.yml"], moves), [
                (source, 1, "docs/guide.md"), (source, 4, "docs/reference.md"),
                (source, 5, "docs/guide.md"), ("config.yml", 1, "docs/guide.md"),
            ])

    def test_redirect_shape_accepts_links_and_rejects_duplicate_article_bodies(self):
        self.assertEqual(redirect_targets(
            '# Moved\n\n[Open](guides/guide.md)\n'
            '- <a id="part"></a> [Part](guides/guide.md#part)\n'
        ), ["guides/guide.md", "guides/guide.md#part"])
        for invalid in ["", "[Open](guide.md)\n", "# Moved\n",
                        "# Moved\nOriginal article body.\n", "# Moved\n```bash\nexit\n```\n",
                        '# Moved\n- <a id="part"></a> [Wrong](guide.md#other)\n']:
            with self.subTest(text=invalid), self.assertRaises(ValueError):
                redirect_targets(invalid)

    def test_missing_tracked_text_is_not_silently_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                obsolete_references(Path(directory), ["missing.md"], self.moves)


if __name__ == "__main__":
    unittest.main()
