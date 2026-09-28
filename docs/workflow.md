# Git Flow contribution workflow

[中文](协作流程.md) · [Contributing](../CONTRIBUTING.md)

`main` is the stable public line; `develop` integrates upcoming work. Existing public commits remain unchanged. This workflow applies to new changes, not retroactive reconstruction of earlier history.

## Branches and pull requests

| Branch | Start from | Pull request into | Purpose |
| --- | --- | --- | --- |
| `feature/<name>` | `develop` | `develop` | Features, fixes for unreleased work, documentation, tests, and maintenance |
| `release/<name>` | `develop` | `main`, then synchronize into `develop` | Stabilize and publish an explicitly approved release |
| `hotfix/<name>` | `main` | `main`, then synchronize into `develop` | Repair the stable line without including unfinished development |
| `main` | — | `develop` | Bring the published line and its merge history back into development |

Do not push changes directly to `main` or `develop`. Do not force-push or delete either long-lived branch. A branch name is a routing convention, not proof of where its code originated: review the diff and ancestry as well.

For ordinary work:

```sh
git fetch origin
git switch develop
git pull --ff-only origin develop
git switch -c feature/short-description
# Edit and run focused tests.
git add <explicit-paths>
git commit
# Pushing requires maintainer authorization; fork contributors use their own remote.
git push -u origin feature/short-description
gh pr create --base develop
```

Use a descriptive Chinese commit subject with no `feat:`-style prefix. End each commit message with exactly one `Type: feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `style`, or `chore` trailer (always include the `Type: ` prefix). Choose the type by the change, not by the branch name.

## Merge without corrupting version history

Use **merge commits**, not squash or rebase merges. Feature commits retain their identity when the same work reaches both long-lived branches. `build.rs` counts `Type:` trailers across reachable history; duplicating or reclassifying the same change can affect version calculation.

Every merge commit must have a Chinese subject and end in **`Type: chore`**. The actual feature or fix is classified on its original commit, not again on the merge. This also applies to local branch-sync merges before pushing a working branch.

The pull request title and body become the default merge subject and body. Therefore:

- Give the pull request a Chinese title without a type prefix.
- Describe the change, evidence, and unverified parts; leave `Type: chore` as the final line of the description.
- Do not replace the prepared merge message with GitHub's generic English message or another feature/fix trailer.
- Merge only after required checks pass and the branch is current with its target. If an update introduces a merge commit, give it the same compliant subject and trailer.

The `workflow` check validates branch routing, pull request merge-message defaults, and every incoming commit. It does not validate the meaning of a Chinese sentence or decide whether a change deserves `feat` versus `fix`; that remains a review responsibility. It checks the real PR head, not GitHub's temporary test merge. Post-push checks also inspect the actual merge message.

## Protected branches

Both `main` and `develop` require a pull request, resolved review conversations, an up-to-date branch, and successful `workflow`, `check`, and `frontend` checks. Administrators are included; force pushes and branch deletion are disabled. With one maintainer, no second-person approval is required: this is an explicit limitation, not independent review. The maintainer must still inspect the diff and evidence before merging. Do not bypass protections when a check fails; investigate it.

GitHub branch protection and merge settings live on GitHub, not in a clone. A fork or a new repository must configure them separately. CI files and this document alone do not activate protection. Keep the default branch as `main`, so visitors see the stable entry point.

## Releases and hotfixes

Release timing, version/tag names, and publication require explicit maintainer approval. This workflow does not create releases or alter the current version scheme automatically.

1. Branch `release/<name>` from `develop`, or `hotfix/<name>` from `main`.
2. Make and test only the intended stabilization/fix changes; open a PR to `main`.
3. After all checks pass, merge with `Type: chore`. Verify the resulting stable commit before tagging or publishing an approved release.
4. Open a `main` → `develop` PR to retain the stable merge history and any fixes. If `develop` has advanced, do not update `main` with unfinished development merely to satisfy the up-to-date requirement. Instead, create a working `release/` or `hotfix/` branch from `main`, merge the current `develop` into that branch with a compliant merge message, resolve any conflicts, and PR it into `develop`. Do not push to a protected branch.
5. Delete a short-lived branch only after both lines contain the needed changes. Branch deletion is not automatic, because release/hotfix work must reach both lines.

Never rewrite already-published history merely to make earlier work look as though it followed this workflow.
