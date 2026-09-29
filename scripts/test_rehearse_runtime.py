from pathlib import Path
import tempfile
import unittest

from rehearse_runtime import select, snapshot


class RehearsalStateTests(unittest.TestCase):
    def test_pointer_switch_and_rollback_preserve_both_executables(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            old, new, link = root / "old", root / "new", root / "lattice"
            old.write_bytes(b"old executable")
            new.write_bytes(b"new executable")
            for target in (old, new, old):
                select(link, target)
                self.assertEqual(link.resolve(), target.resolve())
                self.assertEqual(link.read_bytes(), target.read_bytes())
            self.assertEqual(old.read_bytes(), b"old executable")
            self.assertEqual(new.read_bytes(), b"new executable")
            self.assertFalse((root / "lattice.next").exists())

    def test_file_snapshot_detects_changed_added_and_missing_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path = root / "history"
            path.write_bytes(b"one")
            before = snapshot(root)
            path.write_bytes(b"two")
            self.assertNotEqual(snapshot(root), before)
            path.write_bytes(b"one")
            self.assertEqual(snapshot(root), before)
            (root / "extra").write_bytes(b"extra")
            self.assertNotEqual(snapshot(root), before)
            path.unlink()
            self.assertNotIn("history", snapshot(root))


if __name__ == "__main__":
    unittest.main()
