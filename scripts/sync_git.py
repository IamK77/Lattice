"""Validate synchronization as conflict-free merges of protected histories."""
from pathlib import Path
import subprocess

from release_git import git, is_ancestor, sha


class SyncGit:
    def __init__(self, root, repository):
        self.root = Path(root)
        # This workflow is for the public upstream repository. Fetch missing
        # immutable objects without installing or forwarding a Git credential.
        if repository != "IamK77/Lattice":
            raise ValueError("Synchronization Git reads require the public upstream repository.")
        self.repository = repository

    def ensure(self, commit):
        sha(commit)
        found = subprocess.run(["git", "-C", str(self.root), "cat-file", "-e", f"{commit}^{{commit}}"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if found.returncode:
            subprocess.run(["git", "-C", str(self.root), "-c", "credential.helper=", "fetch",
                            "--no-tags", "--no-write-fetch-head",
                            f"https://github.com/{self.repository}.git", commit], check=True, timeout=120)

    def ancestor(self, older, newer):
        self.ensure(older)
        self.ensure(newer)
        return is_ancestor(self.root, older, newer)

    def merged_tree(self, base, head):
        self.ensure(base)
        self.ensure(head)
        result = subprocess.run(["git", "-C", str(self.root), "merge-tree", "--write-tree", sha(base), sha(head)],
                                capture_output=True, text=True, timeout=60)
        if result.returncode:
            raise ValueError("Synchronization has a merge conflict or Git failure; manual review is required.")
        return sha(result.stdout.splitlines()[0])

    def tree(self, commit):
        self.ensure(commit)
        return sha(git(self.root, "rev-parse", f"{sha(commit)}^{{tree}}"))

    def validate(self, tip, main, develop):
        for commit in (tip, main, develop):
            self.ensure(commit)
        commits = git(self.root, "rev-list", "--max-count=1001", tip, "--not", main, develop).splitlines()
        if len(commits) > 1000:
            raise ValueError("Synchronization history exceeds the inspection bound.")
        for commit in commits:
            parents = git(self.root, "show", "-s", "--format=%P", commit).split()
            if len(parents) != 2 or self.tree(commit) != self.merged_tree(*parents):
                raise ValueError("Synchronization branch contains manual changes; refusing to automate it.")
