from copy import deepcopy
import json
from pathlib import Path
import tomllib
import unittest

from release_plan import Version, propose, cargo_manifest, cargo_lock, changelog, prepare_files


class VersionTests(unittest.TestCase):
    def test_initial_explicit_and_maintenance_only_versions(self):
        self.assertEqual(propose("0.1.0", None, ["feat: initialize"]), "0.1.0")
        self.assertEqual(propose("2.0.0", "1.4.2", ["docs: explain migration"]), "2.0.0")
        self.assertIsNone(propose("1.4.2", "1.4.2", ["docs: improve help", "ci: pin actions"]))
        with self.assertRaises(ValueError):
            propose("1.4.1", "1.4.2", [])

    def test_bump_precedence_and_pre_one_breaking_changes(self):
        for messages, expected in [
            (["fix: handle missing paths"], "1.4.3"),
            (["perf: reduce allocations"], "1.4.3"),
            (["revert: restore the prior behavior"], "1.4.3"),
            (["fix: correct output", "feat: add a command"], "1.5.0"),
            (["feat!: replace the interface", "fix: correct output"], "2.0.0"),
            (["fix: correct the contract\n\nBREAKING CHANGE: migrate configuration"], "2.0.0"),
        ]:
            with self.subTest(messages=messages):
                self.assertEqual(propose("1.4.2", "1.4.2", messages), expected)
        self.assertEqual(propose("0.4.2", "0.4.2", ["feat!: replace the interface"]), "0.5.0")

    def test_invalid_or_prerelease_versions_are_explicitly_rejected(self):
        for value in ["v1.2.3", "01.2.3", "1.2", "1.2.3-rc.1", "1.2.3+build", "1.2.3\n", "1.2.3/next"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                Version.read(value)
        with self.assertRaises(ValueError):
            propose("1.0.0", "1.0.0", ["unclassified change"])


class ManifestTests(unittest.TestCase):
    def test_real_files_change_only_the_version_fields(self):
        root = Path(__file__).resolve().parent.parent
        names = ["Cargo.toml", "Cargo.lock", "clients/ink/package.json", "clients/ink/package-lock.json", "CHANGELOG.md"]
        files = {name: (root / name).read_text() for name in names}
        original = deepcopy(files)
        updated = prepare_files(files, "2.4.7", [{"sha": "a" * 40, "message": "fix: verify packaging"}])
        self.assertEqual(files, original)
        cargo = tomllib.loads(files["Cargo.toml"])
        cargo["package"]["version"] = "2.4.7"
        self.assertEqual(tomllib.loads(updated["Cargo.toml"]), cargo)
        lock = tomllib.loads(files["Cargo.lock"])
        for package in lock["package"]:
            if package["name"] == "lattice" and "source" not in package:
                package["version"] = "2.4.7"
        self.assertEqual(tomllib.loads(updated["Cargo.lock"]), lock)
        package = json.loads(files["clients/ink/package.json"])
        package["version"] = "2.4.7"
        self.assertEqual(json.loads(updated["clients/ink/package.json"]), package)
        frontend_lock = json.loads(files["clients/ink/package-lock.json"])
        frontend_lock["version"] = frontend_lock["packages"][""]["version"] = "2.4.7"
        self.assertEqual(json.loads(updated["clients/ink/package-lock.json"]), frontend_lock)

    def test_package_section_is_not_confused_with_dependency_versions(self):
        manifest = '[package]\nname = "app"\nversion = "1.0.0" # selected\n\n[dependencies.other]\nversion = "1.0.0"\n'
        result = cargo_manifest(manifest, "1.0.1")
        self.assertIn('version = "1.0.1" # selected', result)
        self.assertEqual(tomllib.loads(result)["dependencies"]["other"]["version"], "1.0.0")
        lock = 'version = 4\n\n[[package]]\nname = "app"\nversion = "1.0.0"\n\n[[package]]\nname = "app"\nversion = "1.0.0"\nsource = "registry+example"\n'
        result = tomllib.loads(cargo_lock(lock, "app", "1.0.1"))
        self.assertEqual([item["version"] for item in result["package"]], ["1.0.1", "1.0.0"])


class ChangelogTests(unittest.TestCase):
    def test_curated_notes_and_past_releases_survive(self):
        past = '## [1.0.0]\n\nEarlier notes.\n'
        text = '# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Curated detail.\n\n' + past
        result = changelog(text, "1.1.0", [
            {"sha": "a" * 40, "message": "feat(cli): add a command"},
            {"sha": "b" * 40, "message": "docs: explain the command"},
            {"sha": "c" * 40, "message": "fix!: require migration"},
        ])
        self.assertTrue(result.endswith(past))
        self.assertIn('## [Unreleased]\n\n## [1.1.0]', result)
        self.assertEqual(result.count('### Added'), 1)
        self.assertIn('- Curated detail.', result)
        self.assertIn('cli: add a command', result)
        self.assertNotIn('explain the command', result)
        self.assertIn('### Breaking changes', result)

    def test_missing_duplicate_or_empty_release_notes_are_rejected(self):
        for text in ['# Changelog', '## [Unreleased]\n', '## [Unreleased]\n\n## [1.0.0]\nold', '## [Unreleased]\n## [Unreleased]']:
            with self.subTest(text=text), self.assertRaises(ValueError):
                changelog(text, '1.0.0', [])


if __name__ == "__main__":
    unittest.main()
