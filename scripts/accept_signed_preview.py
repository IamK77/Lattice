"""Opt-in acceptance tests against downloaded, genuinely signed preview assets.

No production binary is executed. Expected identity comes from command-line
arguments, not from the downloaded manifest. Normal unit discovery does not run
this network-dependent acceptance suite.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile
import unittest

from release_git import sha
from release_manifest import CHECKSUMS, MANIFEST, PROVENANCE, archive_names, describe, digest
from release_plan import Version


class SignedPreviewAcceptance(unittest.TestCase):
    directory = None
    version = None
    commit = None
    source_ref = None

    def verify(self, path, *, commit=None, source_ref=None):
        return subprocess.run([
            "gh", "attestation", "verify", str(path),
            "--bundle", str(self.directory / PROVENANCE), "--repo", "IamK77/Lattice",
            "--source-digest", commit or self.commit,
            "--source-ref", source_ref or self.source_ref,
            "--signer-workflow", "IamK77/Lattice/.github/workflows/release.yml",
            "--deny-self-hosted-runners",
        ], capture_output=True, text=True, timeout=120)

    def accepted(self, path):
        result = self.verify(path)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def rejected(self, result, reason="verifying with issuer"):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        # Do not count a missing command or an unrelated transport error as a
        # successful negative test. Recheck the original as a positive control.
        self.assertIn(reason, result.stderr, result.stdout + result.stderr)

    def test_original_subjects_and_embedded_build_identity(self):
        names = [*archive_names(self.version), CHECKSUMS, MANIFEST, PROVENANCE]
        self.assertEqual({p.name for p in self.directory.iterdir()}, set(names))
        before = {name: digest(self.directory / name) for name in names}
        for name in names[:-1]:
            with self.subTest(subject=name):
                self.accepted(self.directory / name)
        # All files already exist, so describe checks exact contents instead of
        # creating a missing manifest/checksum. It never executes an archive.
        record = describe(self.directory, self.version, self.commit, True)
        self.assertTrue(record["preview"])
        self.assertEqual(before, {name: digest(self.directory / name) for name in names})

    def test_one_changed_byte_is_rejected(self):
        original = self.directory / archive_names(self.version)[0]
        self.accepted(original)
        before = digest(original)
        with tempfile.TemporaryDirectory(prefix="lattice-provenance-negative-") as temporary:
            changed = Path(temporary) / original.name
            data = bytearray(original.read_bytes())
            data[len(data) // 2] ^= 1
            changed.write_bytes(data)
            self.assertNotEqual(digest(changed), before)
            self.rejected(self.verify(changed))
        self.assertEqual(digest(original), before)
        self.accepted(original)

    def test_wrong_source_commit_is_rejected(self):
        original = self.directory / MANIFEST
        self.accepted(original)
        wrong = "0" * 40 if self.commit != "0" * 40 else "1" * 40
        self.rejected(self.verify(original, commit=wrong),
                      f"expected SourceRepositoryDigest to be {wrong}, got {self.commit}")
        self.accepted(original)

    def test_wrong_source_branch_is_rejected(self):
        original = self.directory / MANIFEST
        self.accepted(original)
        self.rejected(self.verify(original, source_ref="refs/heads/main"),
                      f"expected SourceRepositoryRef to be refs/heads/main, got {self.source_ref}")
        self.accepted(original)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--source-ref", required=True,
                        choices=("refs/heads/release/next", "refs/heads/release/hotfix-next"))
    args = parser.parse_args()
    SignedPreviewAcceptance.directory = args.directory.resolve(strict=True)
    SignedPreviewAcceptance.version = str(Version.read(args.version))
    SignedPreviewAcceptance.commit = sha(args.commit)
    SignedPreviewAcceptance.source_ref = args.source_ref
    unittest.main(argv=[__file__], verbosity=2)
