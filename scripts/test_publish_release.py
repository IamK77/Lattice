"""Publication state transitions preserve approval, proof, and existing assets."""
from copy import deepcopy
import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from github_release import GitHub, release_for_version
from package_release import TARGETS
from publish_release import asset_names, download_asset, finalize, stage, verify_provenance
from release_manifest import PROVENANCE, archive_names, describe
import test_package_release as archives
import test_validate_release as approvals


class ReleaseAPI:
    repository = "IamK77/Lattice"
    token = "test-only-token"

    def __init__(self, pr):
        self.pr = pr
        self.repository = pr["head"]["repo"]["full_name"]
        self.tag = None
        self.release = None
        self.assets = {}
        self.bytes = {}
        self.mutations = []
        self.fail_after_upload = None
        self.enforce_immutable = True

    def releases(self):
        return [deepcopy(self.release)] if self.release else []

    def pages(self, path):
        assert path == "/releases/7/assets"
        return deepcopy(list(self.assets.values()))

    def request(self, method, path, data=None, missing_ok=False):
        if method == "GET":
            if path == "/pulls/1":
                return deepcopy(self.pr)
            if path == "/git/ref/tags/v0.1.0":
                return {"object": {"type": "commit", "sha": self.tag}} if self.tag else None
            if path == "/releases/tags/v0.1.0":
                return deepcopy(self.release) if self.release and not self.release["draft"] else None
            if path == "/releases/7":
                return deepcopy(self.release)
        self.mutations.append((method, path, deepcopy(data)))
        if method == "POST" and path == "/git/refs":
            assert self.tag is None
            self.tag = data["sha"]
            return {"object": {"type": "commit", "sha": self.tag}}
        if method == "POST" and path == "/releases":
            assert self.release is None
            self.release = dict(data, id=7, immutable=False)
            return deepcopy(self.release)
        if method == "PATCH" and path == "/releases/7":
            self.release.update(data, immutable=self.enforce_immutable)
            return deepcopy(self.release)
        raise AssertionError((method, path, data))

    def upload_asset(self, release_id, path):
        assert release_id == 7
        assert path.name not in self.assets
        content = path.read_bytes()
        asset = {"id": len(self.assets) + 1, "name": path.name, "state": "uploaded",
                 "size": len(content), "digest": "sha256:" + hashlib.sha256(content).hexdigest()}
        self.assets[path.name] = asset
        self.bytes[asset["id"]] = content
        self.mutations.append(("UPLOAD", path.name, None))
        if len(self.assets) == self.fail_after_upload:
            self.fail_after_upload = None
            raise OSError("Injected lost upload response")
        return deepcopy(asset)

    def download(self, command, *, stdout, **kwargs):
        identifier = int(command[2].rsplit("/", 1)[1])
        stdout.write(self.bytes[identifier])
        return subprocess.CompletedProcess(command, 0)


class PublicationTests(unittest.TestCase):
    def setUp(self):
        approvals.ApprovalTests.setUp(self)
        self.api = ReleaseAPI(self.pr)
        output = tempfile.TemporaryDirectory()
        self.addCleanup(output.cleanup)
        self.output = Path(output.name)
        self.verifier = Mock()
        self.make_archives(False)

    def make_archives(self, preview):
        maker = archives.PackageTests()
        for target, name in zip(TARGETS, archive_names("0.1.0")):
            maker.make_archive(self.output / name, target=target, commit=self.commit, identity={"preview": preview})
        (self.output / PROVENANCE).write_bytes(b"Synthetic proof; tests inject the cryptographic verifier.")

    def finish(self, event=None, event_name="pull_request"):
        return finalize(self.root, self.output, event or self.event, event_name, self.api, self.verifier)

    def test_publish_orders_tag_draft_proof_assets_and_publication_and_reruns_read_only(self):
        with patch("package_release.check_binary") as execution:
            result = self.finish()
            execution.assert_not_called()
        self.assertEqual(result["outcome"], "published")
        self.assertEqual(self.api.mutations[0][1], "/git/refs")
        self.assertTrue(self.api.mutations[1][2]["draft"])
        self.assertEqual(self.api.mutations[2][:2], ("UPLOAD", PROVENANCE))
        self.assertEqual(self.api.mutations[-1][:2], ("PATCH", "/releases/7"))
        self.assertEqual(set(self.api.assets), set(asset_names("0.1.0")))
        self.assertEqual(self.api.tag, self.commit)
        self.assertTrue(self.api.release["immutable"])
        count = len(self.api.mutations)
        self.assertEqual(self.finish()["outcome"], "verified-existing-publication")
        self.assertEqual(len(self.api.mutations), count)
        self.assertEqual(self.verifier.call_count, 2)

    def test_preview_verifies_but_never_writes_even_with_publish_input(self):
        self.make_archives(True)
        result = self.finish({"inputs": {"expected_sha": self.commit, "publish": "true"}}, "workflow_dispatch")
        self.assertEqual(result["outcome"], "verified-preview")
        self.assertEqual(self.api.mutations, [])
        self.verifier.assert_called_once()

    def test_proof_failure_or_missing_approval_happens_before_any_mutation(self):
        self.verifier.side_effect = ValueError("Untrusted provenance")
        with self.assertRaisesRegex(ValueError, "Untrusted"):
            self.finish()
        self.assertEqual(self.api.mutations, [])
        self.verifier.reset_mock(side_effect=True)
        self.api.pr["merged"] = False
        with self.assertRaisesRegex(ValueError, "merged release PR"):
            self.finish()
        self.assertEqual(self.api.mutations, [])
        self.verifier.assert_not_called()

    def test_lost_upload_response_resumes_without_reuploading_or_resigning(self):
        self.api.fail_after_upload = 2
        with self.assertRaisesRegex(OSError, "lost upload"):
            self.finish()
        self.assertTrue(self.api.release["draft"])
        original = deepcopy(self.api.assets)
        (self.output / PROVENANCE).unlink()
        with patch("publish_release.subprocess", Mock(run=Mock(side_effect=self.api.download))):
            prepared = stage(self.root, self.output, self.event, "pull_request", self.api, self.verifier)
        self.assertTrue(prepared["bundle_reused"])
        self.assertEqual(self.finish()["outcome"], "published")
        for name, asset in original.items():
            self.assertEqual(self.api.assets[name], asset)
        uploaded = [call[1] for call in self.api.mutations if call[0] == "UPLOAD"]
        self.assertEqual(len(uploaded), len(set(uploaded)))

    def test_rebuilt_bytes_cannot_replace_the_retained_original_signed_distribution(self):
        self.api.fail_after_upload = 2
        with self.assertRaises(OSError):
            self.finish()
        original = {path.name: path.read_bytes() for path in self.output.iterdir()}
        def bound_proof(api, directory, identity):
            for name in asset_names(identity["version"]):
                if (directory / name).read_bytes() != original[name]:
                    raise ValueError("Proof covers original bytes, not a rebuilt substitute")
        with tempfile.TemporaryDirectory() as retry:
            directory = Path(retry)
            maker = archives.PackageTests()
            for target, name in zip(TARGETS, archive_names("0.1.0")):
                maker.make_archive(directory / name, target=target, commit=self.commit,
                                   identity={"preview": False}, payload_changes={"SYSTEM-DEPENDENCIES.txt": b"new loader address"})
            with patch("publish_release.subprocess", Mock(run=Mock(side_effect=self.api.download))):
                with self.assertRaisesRegex(ValueError, "rebuilt substitute"):
                    stage(self.root, directory, self.event, "pull_request", self.api, bound_proof)
        with tempfile.TemporaryDirectory() as restored:
            directory = Path(restored)
            for name, content in original.items():
                (directory / name).write_bytes(content)
            result = stage(self.root, directory, self.event, "pull_request", self.api, bound_proof)
            self.assertTrue(result["bundle_reused"])
            self.assertEqual(finalize(self.root, directory, self.event, "pull_request", self.api, bound_proof)["outcome"], "published")

    def test_changed_existing_asset_unknown_asset_or_foreign_draft_stops_without_replacement(self):
        self.api.fail_after_upload = 1
        with self.assertRaises(OSError):
            self.finish()
        baseline = deepcopy(self.api.assets)
        for mutation, message in [(lambda: self.api.assets[PROVENANCE].update(digest="sha256:" + "0" * 64), "differs"),
                                  (lambda: self.api.assets[PROVENANCE].update(name="unknown.txt"), "Unexpected"),
                                  (lambda: self.api.assets[PROVENANCE].update(state="starter"), "Incomplete"),
                                  (lambda: self.api.release.update(target_commitish="b" * 40), "different source")]:
            with self.subTest(message=message):
                self.api.assets = deepcopy(baseline)
                self.api.release["target_commitish"] = self.commit
                mutation()
                count = len(self.api.mutations)
                with self.assertRaisesRegex(ValueError, message):
                    self.finish()
                self.assertEqual(len(self.api.mutations), count)

    def test_published_recovery_downloads_exact_assets_before_verification(self):
        self.finish()
        count = len(self.api.mutations)
        with tempfile.TemporaryDirectory() as fresh:
            with patch("publish_release.subprocess", Mock(run=Mock(side_effect=self.api.download))):
                result = stage(self.root, Path(fresh), self.event, "pull_request", self.api, self.verifier)
            self.assertTrue(result["published"])
            self.assertTrue(result["bundle_reused"])
            self.assertEqual({path.name for path in Path(fresh).iterdir()}, set(asset_names("0.1.0")))
        self.assertEqual(len(self.api.mutations), count)

    def test_immutability_postcondition_reports_that_publication_already_happened(self):
        self.api.enforce_immutable = False
        with self.assertRaisesRegex(RuntimeError, "Publication already occurred"):
            self.finish()
        self.assertFalse(self.api.release["draft"])
        self.assertFalse(self.api.release["immutable"])
        self.assertEqual(self.api.mutations[-1][0], "PATCH")

    def test_tag_collision_and_missing_immutable_assets_cannot_be_repaired_by_overwriting(self):
        self.api.tag = "b" * 40
        with self.assertRaisesRegex(ValueError, "never retag"):
            self.finish()
        self.assertEqual(self.api.mutations, [])
        self.api.tag = None
        self.finish()
        self.api.assets.pop(PROVENANCE)
        count = len(self.api.mutations)
        with self.assertRaisesRegex(ValueError, "incomplete asset set"):
            self.finish()
        self.assertEqual(len(self.api.mutations), count)

    def test_publication_response_loss_is_unknown_and_postpublication_read_failure_is_explicit(self):
        original = self.api.request
        def lose_response(method, path, data=None, **kwargs):
            result = original(method, path, data, **kwargs)
            if method == "PATCH":
                raise OSError("Injected lost publication response")
            return result
        self.api.request = lose_response
        with self.assertRaisesRegex(RuntimeError, "outcome is unknown"):
            self.finish()
        self.assertFalse(self.api.release["draft"])
        self.api.request = original
        self.assertEqual(self.finish()["outcome"], "verified-existing-publication")

    def test_first_read_after_successful_publication_cannot_be_described_as_no_publication(self):
        original = self.api.request
        def fail_confirmation(method, path, data=None, **kwargs):
            if method == "GET" and path == "/releases/7" and self.api.release and not self.api.release["draft"]:
                raise OSError("Injected confirmation outage")
            return original(method, path, data, **kwargs)
        self.api.request = fail_confirmation
        with self.assertRaisesRegex(RuntimeError, "Publication already occurred"):
            self.finish()
        self.assertFalse(self.api.release["draft"])

    def test_concurrent_draft_rename_or_identifier_change_blocks_publication(self):
        original = self.api.request
        for changed in [{"tag_name": "v9.9.9"}, {"id": 99}]:
            with self.subTest(changed=changed):
                def change_draft(method, path, data=None, **kwargs):
                    result = original(method, path, data, **kwargs)
                    return dict(result, **changed) if method == "GET" and path == "/releases/7" else result
                self.api.request = change_draft
                with self.assertRaisesRegex(ValueError, "changed concurrently"):
                    self.finish()
                self.assertTrue(self.api.release["draft"])
                self.assertFalse(any(call[0] == "PATCH" for call in self.api.mutations))
        self.api.request = original
        self.assertEqual(self.finish()["outcome"], "published")

    def test_verification_can_finish_before_any_publication_mutation(self):
        result = finalize(self.root, self.output, self.event, "pull_request", self.api, self.verifier, verify_only=True)
        self.assertEqual(result["outcome"], "verified-distribution")
        self.assertEqual(self.api.mutations, [])
        self.assertEqual({path.name for path in self.output.iterdir()}, set(asset_names("0.1.0")))
        self.verifier.assert_called_once()

    def test_verifier_checks_all_four_subjects_against_exact_source_workflow_and_hosted_runner(self):
        identity = {"version": "0.1.0", "commit": self.commit}
        describe(self.output, "0.1.0", self.commit, False)
        with patch("publish_release.subprocess.run") as run:
            verify_provenance(self.api, self.output, identity)
        self.assertEqual(run.call_count, 4)
        for call in run.call_args_list:
            command = call.args[0]
            self.assertEqual(command[command.index("--source-digest") + 1], self.commit)
            self.assertEqual(command[command.index("--signer-workflow") + 1], f"{self.api.repository}/.github/workflows/release.yml")
            self.assertIn("--deny-self-hosted-runners", command)
            self.assertTrue(call.kwargs["check"])

    def test_corrupted_download_never_acquires_a_final_filename(self):
        self.finish()
        asset = self.api.assets[PROVENANCE]
        self.api.bytes[asset["id"]] = b"corrupted"
        with tempfile.TemporaryDirectory() as fresh:
            destination = Path(fresh) / PROVENANCE
            with patch("publish_release.subprocess", Mock(run=Mock(side_effect=self.api.download))):
                with self.assertRaisesRegex(ValueError, "does not match"):
                    download_asset(self.api, asset, destination)
            self.assertFalse(destination.exists())


class UploadTransportTests(unittest.TestCase):
    def test_complete_listing_finds_drafts_rejects_duplicates_and_propagates_page_failure(self):
        draft = {"tag_name": "v0.1.0", "draft": True}
        self.assertEqual(release_for_version([draft], "0.1.0"), draft)
        with self.assertRaisesRegex(ValueError, "Multiple"):
            release_for_version([draft, dict(draft, draft=False)], "0.1.0")
        api = GitHub("IamK77/Lattice", "test-only-token")
        api.request = Mock(side_effect=[[{"tag_name": f"v0.0.{index}", "draft": False} for index in range(100)], [draft]])
        self.assertEqual(release_for_version(api.releases(), "0.1.0"), draft)
        self.assertEqual(api.request.call_count, 2)
        api.request.side_effect = [[draft] * 100, OSError("Page unavailable")]
        with self.assertRaisesRegex(OSError, "Page unavailable"):
            api.releases()
        api.request.side_effect = [{"artifacts": [{"id": 12}]}]
        self.assertEqual(api.pages("/actions/runs/1/artifacts", list_key="artifacts"), [{"id": 12}])

    def test_upload_uses_fixed_github_host_and_does_not_follow_redirects(self):
        with tempfile.TemporaryDirectory() as root:
            asset = Path(root) / "proof.json"
            asset.write_bytes(b"proof")
            response = Mock()
            response.read.return_value = b'{"id": 9}'
            response.__enter__ = Mock(return_value=response)
            response.__exit__ = Mock(return_value=False)
            opener = Mock()
            opener.open.return_value = response
            api = GitHub("IamK77/Lattice", "test-only-token")
            with patch("github_release.build_opener", return_value=opener) as factory:
                self.assertEqual(api.upload_asset(7, asset), {"id": 9})
            request = opener.open.call_args.args[0]
            self.assertEqual(request.full_url, "https://uploads.github.com/repos/IamK77/Lattice/releases/7/assets?name=proof.json")
            self.assertEqual(request.data, b"proof")
            self.assertIsNone(factory.call_args.args[0].redirect_request(None, None, 302, None, None, "https://foreign.invalid"))
            for identifier in [True, 0, "7"]:
                with self.assertRaises(ValueError):
                    api.upload_asset(identifier, asset)


if __name__ == "__main__":
    unittest.main()
