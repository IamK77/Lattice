"""The native recipe binds compiler, checkout, binary, and generated reports."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from build_release import build
from package_release import TARGETS, verify_archive
from release_git import git
import test_package_release as fixtures


class BuildTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "assets").mkdir()
        for name in ["LICENSE", "NOTICE", "assets/README.md", "about.toml", "about.hbs"]:
            (self.root / name).write_text("Synthetic tracked input.\n")
        (self.root / "Cargo.toml").write_text('[package]\nname = "lattice"\nversion = "0.1.0"\n')
        (self.root / "Cargo.lock").write_text('version = 4\n[[package]]\nname = "lattice"\nversion = "0.1.0"\n')
        (self.root / ".gitignore").write_text("/target/\n")
        git(self.root, "init", "-q", "-b", "main")
        for key, value in [("user.name", "Build Test"), ("user.email", "build@example.invalid"), ("commit.gpgsign", "false"), ("core.hooksPath", str(self.root / "no-hooks"))]:
            git(self.root, "config", key, value)
        git(self.root, "add", ".")
        git(self.root, "commit", "-qm", "build: initialize recipe fixture")
        self.commit = git(self.root, "rev-parse", "HEAD")
        self.target = TARGETS[1]
        self.binary = self.root / "target" / self.target / "release/lattice"
        self.output = self.root / "target/dist"
        self.commands = []
        self.compiler_calls = 0
        self.mutation = None
        self.compiler = f"rustc 1.98.0 fixture\nhost: {self.target}\n"
        # Leave Git's real subprocesses intact: only build-driver calls are
        # replaced, not the shared subprocess module used by Git verification.
        self.driver = patch("build_release.subprocess")
        self.process = self.driver.start()
        self.addCleanup(self.driver.stop)
        self.process.run.side_effect = self.run_command
        self.process.check_output.side_effect = self.output_command
        self.environment = patch.dict("os.environ", {"RUSTC": "", "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": ""})
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def run_command(self, arguments, **kwargs):
        self.commands.append(arguments)
        if arguments[:2] == ["cargo", "build"]:
            self.assertIn("--locked", arguments)
            self.assertIn("--release", arguments)
            self.assertEqual(kwargs["env"]["LATTICE_RELEASE_VERSION"], "0.1.0")
            self.binary.parent.mkdir(parents=True)
            self.binary.write_bytes(fixtures.PackageTests().macho())
        elif arguments[:3] == ["cargo", "about", "generate"]:
            self.assertIn("--fail", arguments)
            self.assertIn("--locked", arguments)
            self.licenses = Path(arguments[arguments.index("--output-file") + 1])
            self.licenses.write_text("<section>Apache License</section>")
        else:
            raise AssertionError(arguments)

    def output_command(self, arguments, **kwargs):
        if arguments[0] == "rustc":
            self.compiler_calls += 1
            return self.compiler + ("changed compiler\n" if self.mutation == "compiler" and self.compiler_calls > 1 else "")
        if arguments[0] == "otool":
            return f"{self.binary}:\n  /usr/lib/libSystem.B.dylib\n"
        if arguments[:2] == ["cargo", "metadata"]:
            if self.mutation == "binary":
                self.binary.write_bytes(self.binary.read_bytes() + b"swapped after compilation")
            if self.mutation == "license":
                self.licenses.write_text("<section>Apache License from a different build</section>")
            if self.mutation == "system":
                (self.licenses.parent / "SYSTEM-DEPENDENCIES.txt").write_text("Different native dependencies")
            if self.mutation == "source":
                (self.root / "Cargo.lock").write_text((self.root / "Cargo.lock").read_text() + "# changed\n")
            return json.dumps({"packages": [{"id": "root", "name": "lattice", "version": "0.1.0", "source": None, "license": "Apache-2.0"}],
                               "resolve": {"root": "root", "nodes": [{"id": "root", "dependencies": []}]}})
        raise AssertionError(arguments)

    def invoke(self):
        with patch("package_release.check_binary"):
            return build(self.root, self.output, "0.1.0", self.commit, self.target, True)

    def test_complete_recipe_keeps_build_time_compiler_and_rechecks_contents(self):
        archive = self.invoke()
        verify_archive(archive, "0.1.0", self.commit, self.target, True, execute=False)
        self.assertEqual([command[:2] for command in self.commands], [["cargo", "build"], ["cargo", "about"]])
        self.assertEqual(self.compiler_calls, 2)
        self.assertEqual(git(self.root, "status", "--porcelain"), "")

    def test_changed_binary_is_rejected_even_with_same_version_and_architecture(self):
        self.mutation = "binary"
        with self.assertRaisesRegex(ValueError, "Artifact inputs differ"):
            self.invoke()
        self.assertFalse(self.output.exists())

    def test_stale_license_report_is_not_rebound_to_the_current_build(self):
        self.mutation = "license"
        with self.assertRaisesRegex(ValueError, "Artifact inputs differ"):
            self.invoke()
        self.assertFalse(self.output.exists())

    def test_changed_native_dependency_inspection_is_rejected(self):
        self.mutation = "system"
        with self.assertRaisesRegex(ValueError, "Artifact inputs differ"):
            self.invoke()
        self.assertFalse(self.output.exists())

    def test_changed_source_is_not_described_as_a_clean_commit(self):
        self.mutation = "source"
        with self.assertRaisesRegex(ValueError, "clean checkout"):
            self.invoke()
        self.assertFalse(self.output.exists())

    def test_changed_compiler_is_not_mixed_with_old_build_metadata(self):
        self.mutation = "compiler"
        with self.assertRaisesRegex(ValueError, "compiler changed"):
            self.invoke()
        self.assertFalse(self.output.exists())

    def test_dirty_source_and_wrong_host_fail_before_compilation(self):
        (self.root / "untracked.rs").write_text("untracked source")
        with self.assertRaisesRegex(ValueError, "clean checkout"):
            self.invoke()
        self.assertEqual(self.commands, [])
        (self.root / "untracked.rs").unlink()
        self.compiler = "rustc fixture\nhost: x86_64-unknown-linux-gnu\n"
        with self.assertRaisesRegex(ValueError, "native host"):
            self.invoke()
        self.assertEqual(self.commands, [])


if __name__ == "__main__":
    unittest.main()
