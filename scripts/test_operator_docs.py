"""Keep executable documentation syntax and bilingual installation steps aligned."""
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


def blocks(name):
    return re.findall(r"(?ms)^```bash\n(.*?)^```[ \t]*$", (ROOT / "docs" / name).read_text())


def without_comments(block):
    # These installation snippets have only whole-line comments or a comment
    # introduced after two spaces, never a quoted hash with that prefix.
    return [line.split("  #", 1)[0].rstrip() for line in block.splitlines()
            if line.strip() and not line.lstrip().startswith("#")]


class OperatorDocumentationTests(unittest.TestCase):
    def test_all_operator_bash_examples_parse_without_execution(self):
        for name in ["guides/installation.md", "guides/installation.zh-CN.md", "guides/troubleshooting.md", "guides/troubleshooting.zh-CN.md",
                     "records/signed-preview-acceptance.md", "records/signed-preview-acceptance.zh-CN.md"]:
            snippets = blocks(name)
            self.assertTrue(snippets, name)
            for index, snippet in enumerate(snippets):
                with self.subTest(document=name, block=index):
                    result = subprocess.run(["bash", "-n"], input=snippet, text=True, capture_output=True, check=False)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def package_installation_script(self, name):
        candidates = [block for block in blocks(name) if 'destination="$root/v$version-$target"' in block]
        self.assertEqual(len(candidates), 1, "exercise exactly one package-installation example")
        return candidates[0]

    def test_release_identity_checks_abort_explicitly_before_download(self):
        for name in ["guides/installation.md", "guides/installation.zh-CN.md"]:
            candidates = [block for block in blocks(name) if "gh release download" in block]
            self.assertEqual(len(candidates), 1, "exercise exactly one release-verification example")
            # Bash 3.2 does not abort failed [[ ]] via errexit. Disable implicit
            # errexit here to exercise explicit refusal on every Bash version.
            script = candidates[0].replace("set -euo pipefail", "set -uo pipefail", 1)
            cases = [("false", "a" * 40, "0", "0", False),
                     ("true", "not-a-commit", "0", "0", False),
                     ("true", "a" * 40, "1", "0", False),
                     ("true", "a" * 40, "0", "1", False),
                     ("true", "a" * 40, "0", "0", True)]
            for release, commit, release_status, commit_status, accepted in cases:
                with self.subTest(document=name, case=(release, commit, release_status, commit_status)), tempfile.TemporaryDirectory() as directory:
                    home = Path(directory)
                    tools = home / "test-tools"
                    tools.mkdir()
                    (tools / "mktemp").symlink_to("/usr/bin/mktemp")
                    probe = home / "mock-actions"
                    gh = tools / "gh"
                    gh.write_text(
                        '#!/bin/bash\ncase "$1:$2" in\n'
                        '  api:*releases/tags/*) printf "%s\\n" "$MOCK_RELEASE"; exit "$MOCK_RELEASE_STATUS" ;;\n'
                        '  api:*git/ref/tags/*) printf "%s\\n" "$MOCK_COMMIT"; exit "$MOCK_COMMIT_STATUS" ;;\n'
                        '  release:download|attestation:verify) printf "%s\\n" "$1" >> "$PROBE_FILE" ;;\n'
                        '  *) exit 99 ;;\nesac\n', encoding="utf-8",
                    )
                    gh.chmod(0o700)
                    result = subprocess.run(["/bin/bash"], input=script, text=True,
                                            capture_output=True, check=False, cwd=home,
                                            env={"HOME": directory, "TMPDIR": directory,
                                                 "PATH": str(tools), "LC_ALL": "C",
                                                 "MOCK_RELEASE": release, "MOCK_COMMIT": commit,
                                                 "MOCK_RELEASE_STATUS": release_status,
                                                 "MOCK_COMMIT_STATUS": commit_status,
                                                 "PROBE_FILE": str(probe)})
                    self.assertEqual(result.returncode, 0 if accepted else 1, result.stderr)
                    self.assertEqual(probe.exists(), accepted, "invalid identity must stop before download")
                    if accepted:
                        self.assertIn("release\n", probe.read_text(encoding="utf-8"))

    def test_package_installation_stops_without_a_verified_version(self):
        for name in ["guides/installation.md", "guides/installation.zh-CN.md"]:
            with self.subTest(document=name), tempfile.TemporaryDirectory() as directory:
                home = Path(directory)
                # No credentials, archives, programs or commands are available.
                result = subprocess.run(["/bin/bash"], input=self.package_installation_script(name),
                                        text=True, capture_output=True, check=False, cwd=home,
                                        env={"HOME": directory, "PATH": directory, "LC_ALL": "C"})
                self.assertEqual(result.returncode, 1)
                self.assertIn("Use the version verified in the previous step", result.stderr)
                self.assertFalse((home / ".local").exists())

    def test_package_installation_stops_before_touching_an_existing_version(self):
        for name in ["guides/installation.md", "guides/installation.zh-CN.md"]:
            with self.subTest(document=name), tempfile.TemporaryDirectory() as directory:
                home = Path(directory)
                destination = home / ".local/share/lattice/versions/v1.2.3-aarch64-apple-darwin"
                destination.mkdir(parents=True)
                previous = destination / "lattice"
                previous.write_text("previous program", encoding="utf-8")
                tools = home / "test-tools"
                tools.mkdir()
                # Only mkdir is reachable by name; no extraction or program run
                # can succeed even if the documentation's fail-fast is removed.
                (tools / "mkdir").symlink_to("/bin/mkdir")
                result = subprocess.run(["/bin/bash"], input=self.package_installation_script(name),
                                        text=True, capture_output=True, check=False, cwd=home,
                                        env={"HOME": directory, "PATH": str(tools),
                                             "LC_ALL": "C", "version": "1.2.3"})
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout + result.stderr, "")
                self.assertEqual(previous.read_text(encoding="utf-8"), "previous program")

    def test_installed_version_requires_success_and_expected_output(self):
        for name in ["guides/installation.md", "guides/installation.zh-CN.md"]:
            lines = self.package_installation_script(name).splitlines()
            starts = [index for index, line in enumerate(lines) if "--version" in line]
            self.assertEqual(len(starts), 1)
            # Exercise only the post-extraction checks against a local mock,
            # never tar or an actual distributed program.
            script = "\n".join(lines[starts[0]:]) + "\n"
            for output, status, accepted in [("v1.2.3", "1", False),
                                              ("wrong-version", "0", False),
                                              ("v1.2.3", "0", True)]:
                with self.subTest(document=name, output=output, status=status), tempfile.TemporaryDirectory() as directory:
                    home = Path(directory)
                    program = home / "lattice"
                    program.write_text(
                        '#!/bin/bash\ncase "$1" in\n'
                        '  --version) printf "%s\\n" "$MOCK_VERSION"; exit "$MOCK_STATUS" ;;\n'
                        '  --help) printf "help-called\\n" ;;\n'
                        '  *) exit 99 ;;\nesac\n', encoding="utf-8",
                    )
                    program.chmod(0o700)
                    result = subprocess.run(["/bin/bash"], input=script, text=True,
                                            capture_output=True, check=False, cwd=home,
                                            env={"HOME": directory, "PATH": directory,
                                                 "LC_ALL": "C", "destination": directory,
                                                 "version": "1.2.3", "MOCK_VERSION": output,
                                                 "MOCK_STATUS": status})
                    self.assertEqual(result.returncode, 0 if accepted else 1, result.stderr)
                    self.assertEqual(result.stdout, "help-called\n" if accepted else "")

    def test_standalone_installation_conditionals_explicitly_abort(self):
        for name in ["guides/installation.md", "guides/installation.zh-CN.md"]:
            checks = [line.strip() for block in blocks(name) for line in block.splitlines()
                      if line.strip().startswith("[[")]
            self.assertTrue(checks, "exercise the installation identity and destination checks")
            for line in checks:
                with self.subTest(document=name, conditional=line):
                    self.assertTrue(line.endswith("|| exit 1"), "do not rely on implicit errexit for [[ ]]")

    def test_bilingual_installation_and_verification_commands_do_not_drift(self):
        for english_name, chinese_name in [("guides/installation.md", "guides/installation.zh-CN.md"),
                                           ("records/signed-preview-acceptance.md", "records/signed-preview-acceptance.zh-CN.md")]:
            with self.subTest(document=english_name):
                english = [without_comments(block) for block in blocks(english_name)]
                chinese = [without_comments(block) for block in blocks(chinese_name)]
                self.assertEqual(english, chinese)


if __name__ == "__main__":
    unittest.main()
