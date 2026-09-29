"""Exercise two already verified executables without touching the user's home.

This is not an installer or signature verifier. Keep extracted packages and
attribution intact, verify their identity first, then pass their binary paths.
A same-binary run tests the probe, not cross-build compatibility.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import signal
import subprocess
import tempfile
import threading
import uuid

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def snapshot(directory):
    return {str(path.relative_to(directory)): digest(path)
            for path in sorted(directory.rglob("*")) if path.is_file()}


def select(link, target):
    staged = link.with_name("lattice.next")
    staged.symlink_to(target)
    os.replace(staged, link)
    if link.resolve() != target.resolve():
        raise ValueError("Executable pointer did not select the requested binary.")


def turn(binary, root, label, previous):
    socket = root / "daemon.sock"
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(root / "home"), "TMPDIR": str(root / "tmp"),
        "LATTICE_SCRIPTED": "1", "LATTICE_OVERLAY": "",
        "LATTICE_WORKSPACE": str(root / "project"),
        "LATTICE_SOCKET": str(socket),
    }
    version = subprocess.run([str(binary), "--version"], env=env, cwd=root / "project",
                             check=True, capture_output=True, text=True, timeout=30).stdout.strip()
    lines = queue.Queue()
    with (root / f"{label}.stderr").open("wb") as stderr:
        child = subprocess.Popen([str(binary), "serve"], env=env, cwd=root / "project",
                                 stdout=subprocess.PIPE, stderr=stderr, text=True)
        def read_lines():
            try:
                for line in child.stdout:
                    lines.put(line.rstrip("\n"))
            finally:
                lines.put(None)
        reader = threading.Thread(target=read_lines, daemon=True)
        reader.start()
        try:
            startup = [lines.get(timeout=30) for _ in range(3)]
            if startup != ["lattice daemon · scripted @ ", f"socket: {socket}",
                           "connect a client (clients/ink, or any language); Ctrl-C to stop"]:
                raise ValueError(f"Unexpected daemon startup: {startup!r}")
            command = ["node", str(ROOT / "clients/ink/scripts/release-smoke.js"), str(socket),
                       "release-rehearsal", f"rehearsal {label} {uuid.uuid4()}"]
            if previous:
                command.append(previous)
            completed = subprocess.run(command, env=env, cwd=root / "project",
                                       capture_output=True, text=True, timeout=40)
            if completed.returncode:
                raise ValueError(f"Turn probe failed: {completed.stderr}")
            evidence = json.loads(completed.stdout)
            child.send_signal(signal.SIGINT)
            if child.wait(timeout=30) != 0:
                raise ValueError("Daemon did not stop successfully.")
            reader.join(timeout=5)
            remaining = []
            while not lines.empty():
                remaining.append(lines.get_nowait())
            if reader.is_alive() or "stopped" not in remaining or socket.exists():
                raise ValueError("Daemon shutdown did not close its output and socket.")
            return {"phase": label, "version": version,
                    "binarySha256": digest(binary), **evidence}
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=30)
            child.stdout.close()
            reader.join(timeout=5)


def rehearse(old, new):
    old, new = old.resolve(strict=True), new.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="lattice-rehearsal-", dir="/tmp") as temp:
        root = Path(temp)
        for name in ("home", "tmp", "project", "bin"):
            (root / name).mkdir()
        link = root / "bin/lattice"
        select(link, old)
        first = turn(link, root, "old", None)
        data = root / "home/.lattice"
        before = snapshot(data)
        if not before or not (data / "streams/release-rehearsal.ledger").is_dir():
            raise ValueError("The probe did not create persistent test history.")
        backup = root / "backup"
        shutil.copytree(data, backup)
        if snapshot(backup) != before:
            raise ValueError("Stopped-state backup differs from its source.")
        select(link, new)
        upgraded = turn(link, root, "new", first["materialInput"])
        select(link, old)
        rolled_back = turn(link, root, "rollback", upgraded["materialInput"])
        # Preserve the newer test state; restoring a backup is a separate act.
        data.rename(root / "post-upgrade-data")
        shutil.copytree(backup, data)
        if snapshot(data) != before:
            raise ValueError("Restored backup differs from the stopped original.")
        restored = turn(link, root, "restored", first["materialInput"])
        return {"scope": "isolated scripted runtime and pointer/backup exercise",
                "distinctBinaries": digest(old) != digest(new),
                "backupFileCount": len(before), "backupFileContentsRestored": True,
                "phases": [first, upgraded, rolled_back, restored]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("old_binary", type=Path)
    parser.add_argument("new_binary", type=Path)
    args = parser.parse_args()
    print(json.dumps(rehearse(args.old_binary, args.new_binary), indent=2))
