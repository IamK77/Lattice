"""Archive validation rejects misleading metadata, traversal, and wrong binaries."""
import hashlib
import io
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from package_release import CONTENTS, SOURCE_INPUTS, TARGETS, architecture, check_binary, checksum, inventory, package, verify_archive


class PackageTests(unittest.TestCase):
    def elf(self):
        header = bytearray(32)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", header, 18, 62)
        return bytes(header)

    def macho(self):
        header = bytearray(32)
        header[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", header, 4, 0x0100000c)
        struct.pack_into("<I", header, 12, 2)
        return bytes(header)

    def test_architecture_is_measured_from_the_file(self):
        architecture(self.elf(), TARGETS[0])
        macho = self.macho()
        architecture(macho, TARGETS[1])
        for header, target in [(macho, TARGETS[0]), (self.elf(), TARGETS[1]), (b"#!/bin/sh\n", TARGETS[0]), (b"", TARGETS[0])]:
            with self.subTest(target=target, header=header):
                with self.assertRaises(ValueError):
                    architecture(header, target)
        with self.assertRaises(ValueError):
            architecture(macho, "unknown-target")

    def test_binary_smoke_uses_isolated_home_and_accepts_help_on_stderr(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "lattice"
            binary.write_bytes(self.elf())
            results = [subprocess.CompletedProcess([], 0, "v0.1.0\n", ""), subprocess.CompletedProcess([], 0, "", "lattice help\n")]
            with patch("package_release.subprocess.run", side_effect=results) as run:
                check_binary(binary, "0.1.0", TARGETS[0])
                self.assertEqual(run.call_count, 2)
                environment = run.call_args.kwargs["env"]
                self.assertEqual(set(environment), {"PATH", "HOME", "LATTICE_HOME", "LANG"})
                self.assertEqual(environment["HOME"], environment["LATTICE_HOME"])
            with patch("package_release.subprocess.run", return_value=subprocess.CompletedProcess([], 0, "v0.1.0-dev\n", "")):
                with self.assertRaisesRegex(ValueError, "version differs"):
                    check_binary(binary, "0.1.0", TARGETS[0])

    def make_archive(self, path, *, extra=None, identity=None, link=False, mode=0o755, duplicate=False, target=TARGETS[0]):
        prefix = f"lattice-v0.1.0-{target}/"
        graph = {"schema": 1, "root": "lattice@0.1.0", "packages": [
            {"id": "lattice@0.1.0", "source": "git-commit:" + "a" * 40, "dependencies": []}]}
        payloads = {name: b"text" for name in CONTENTS - {"BUILD-INFO.json"}}
        payloads["lattice"] = self.elf() if target == TARGETS[0] else self.macho()
        payloads["DEPENDENCIES.json"] = json.dumps(graph).encode()
        build = {"schema": 1, "version": "0.1.0", "commit": "a" * 40, "target": target, "preview": True,
                 "rustc": "rustc fixture", "inputs": {name: "0" * 64 for name in SOURCE_INPUTS},
                 "files": {name: hashlib.sha256(data).hexdigest() for name, data in payloads.items()}}
        build.update(identity or {})
        payloads["BUILD-INFO.json"] = json.dumps(build).encode()
        with tarfile.open(path, "w:gz") as tar:
            for name in [*sorted(CONTENTS), *([extra] if extra else []), *(["LICENSE"] if duplicate else [])]:
                data = payloads.get(name, b"extra")
                member = tarfile.TarInfo(prefix + name)
                member.mode = mode if name == "lattice" else 0o644
                if link and name == "lattice":
                    member.type = tarfile.SYMTYPE
                    member.linkname = "/outside"
                else:
                    member.size = len(data)
                tar.addfile(member, io.BytesIO(data))

    def test_archive_checks_exact_members_identity_mode_and_extracted_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "package.tar.gz"
            self.make_archive(archive)
            with patch("package_release.check_binary") as smoke:
                verify_archive(archive, "0.1.0", "a" * 40, TARGETS[0], True)
                smoke.assert_called_once()
                self.assertEqual(smoke.call_args.args[1:], ("0.1.0", TARGETS[0]))
            for change in [{"extra": "../../escape"}, {"identity": {"commit": "b" * 40}},
                           {"identity": {"preview": False}}, {"identity": {"preview": 1}},
                           {"identity": {"schema": True}}, {"identity": {"schema": 1.0}},
                           {"link": True}, {"mode": 0o644}, {"duplicate": True}]:
                with self.subTest(change=change):
                    self.make_archive(archive, **change)
                    with patch("package_release.check_binary") as smoke:
                        with self.assertRaises(ValueError):
                            verify_archive(archive, "0.1.0", "a" * 40, TARGETS[0], True)
                        smoke.assert_not_called()

    def test_failed_packaging_never_leaves_a_final_archive_and_can_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "assets").mkdir()
            for name in ["LICENSE", "NOTICE", "assets/README.md"]:
                (root / name).write_text("Synthetic attribution.\n")
            (root / "Cargo.toml").write_text('[package]\nname = "lattice"\nversion = "0.1.0"\n')
            (root / "Cargo.lock").write_text('version = 4\n[[package]]\nname = "lattice"\nversion = "0.1.0"\n')
            binary = root / "lattice"
            binary.write_bytes(self.elf())
            licenses = root / "licenses.html"
            licenses.write_text("<section>Apache License</section>")
            system = root / "system.txt"
            system.write_text("Synthetic native dependency inspection.\n")
            output = root / "output"
            metadata = {"packages": [{"id": "root", "name": "lattice", "version": "0.1.0", "source": None, "license": "Apache-2.0"}],
                        "resolve": {"root": "root", "nodes": [{"id": "root", "dependencies": []}]}}
            for name in ["about.toml", "about.hbs"]:
                (root / name).write_text("Synthetic license configuration.")
            dependencies = root / "DEPENDENCIES.json"
            dependencies.write_text(json.dumps(inventory(metadata, {"package": [{"name": "lattice", "version": "0.1.0"}]}, "a" * 40)))
            sources = {"lattice": binary, "THIRD-PARTY-LICENSES.html": licenses, "SYSTEM-DEPENDENCIES.txt": system,
                       "DEPENDENCIES.json": dependencies, "LICENSE": root / "LICENSE", "NOTICE": root / "NOTICE", "ASSET-ATTRIBUTION.md": root / "assets/README.md"}
            receipt = {"schema": 1, "version": "0.1.0", "commit": "a" * 40, "target": TARGETS[0], "preview": True,
                       "rustc": "rustc build-time fixture", "inputs": {name: checksum(root / name) for name in SOURCE_INPUTS},
                       "files": {name: checksum(path) for name, path in sources.items()}}
            arguments = (root, binary, licenses, system, output, "0.1.0", "a" * 40, TARGETS[0], True)
            extra = {"receipt": receipt, "dependencies": dependencies}
            with patch("package_release.git", side_effect=lambda root, *args: "a" * 40 if args[0] == "rev-parse" else "" if args[0] == "status" else "12345"), \
                 patch("package_release.check_binary"):
                for path, message in [(binary, "Artifact inputs"), (licenses, "Artifact inputs"), (root / "Cargo.lock", "Source inputs")]:
                    original = path.read_bytes()
                    path.write_bytes(original + b"changed")
                    with self.assertRaisesRegex(ValueError, message):
                        package(*arguments, **extra)
                    path.write_bytes(original)
                for operation in ["package_release.tarfile.TarFile.addfile", "package_release.verify_archive"]:
                    with self.subTest(operation=operation):
                        with patch(operation, side_effect=OSError("Injected packaging failure")):
                            with self.assertRaisesRegex(OSError, "Injected"):
                                package(*arguments, **extra)
                        self.assertEqual(list(output.iterdir()), [])
                archive = package(*arguments, **extra)
                original = archive.read_bytes()
                with self.assertRaisesRegex(ValueError, "overwrite"):
                    package(*arguments, **extra)
                self.assertEqual(archive.read_bytes(), original)

    def test_inventory_has_explicit_scope_checksums_and_no_local_paths(self):
        local = "path+file:///private/workspace#lattice@0.1.0"
        dependency = "registry+https://example.invalid#index@1.0.0"
        metadata = {"packages": [{"id": local, "name": "lattice", "version": "0.1.0", "license": "Apache-2.0", "source": None},
                                 {"id": dependency, "name": "index", "version": "1.0.0", "license": "MIT", "source": "registry+https://example.invalid"}],
                    "resolve": {"root": local, "nodes": [{"id": local, "dependencies": [dependency]}, {"id": dependency, "dependencies": []}]}}
        locked = {"package": [{"name": "index", "version": "1.0.0", "source": "registry+https://example.invalid", "checksum": "abc"}]}
        result = inventory(metadata, locked, "a" * 40)
        self.assertNotIn("/private", json.dumps(result))
        self.assertIn("not a binary-derived SBOM", result["scope"])
        self.assertEqual(result["packages"][0]["checksum"], "abc")
        self.assertEqual(result["packages"][1]["dependencies"], ["index@1.0.0"])
        self.assertEqual(result["packages"][1]["source"], "git-commit:" + "a" * 40)
        metadata["packages"][1]["source"] = None
        with self.assertRaisesRegex(ValueError, "explicit source provenance"):
            inventory(metadata, locked, "a" * 40)
        metadata["packages"][1]["source"] = "registry+https://example.invalid"
        root = metadata["resolve"].pop("root")
        with self.assertRaisesRegex(ValueError, "single root"):
            inventory(metadata, locked, "a" * 40)
        metadata["resolve"]["root"] = root
        metadata["packages"].append(dict(metadata["packages"][1], id="different-source"))
        with self.assertRaisesRegex(ValueError, "Ambiguous"):
            inventory(metadata, locked, "a" * 40)


if __name__ == "__main__":
    unittest.main()
