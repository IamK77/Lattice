# Maintainer handbook

[中文](maintainer-handbook.zh-CN.md) · [Maintainers](../../MAINTAINERS.md)

This is the operational checklist, not another copy of the release implementation. The project currently has one maintainer; branch protection is not a claim of independent human review. Never bypass it to make a release pass.

## Regular maintenance

- Route work through [the contribution workflow](workflow.md); use English Conventional Commits and keep `CHANGELOG.md` Unreleased notes useful to users.
- Review grouped dependency updates and [supply-chain checks](supply-chain.md). Advisory exceptions have owners, reasons, exact versions, and expiry; expiration is a review deadline, not permission to extend automatically.
- Keep public documentation, schemas, examples, and supported-platform statements aligned with actual tests. Update the platform scope only after recording a real result.
- Triage ordinary reports through [Support](../../SUPPORT.md) and vulnerabilities through [Security](../../SECURITY.md). The security policy covers the latest stable release, not indefinite backports or an independent development support line.
- Do not upload uninspected history, environment output, model catalogs, or assembly exports as debugging evidence.

## First-rollout checklist

These are required actions, **not a statement that rollout has already completed**:

1. Merge the reviewed setup into `main` through `release/workflow-bootstrap`, not a canonical publishing branch. Do not create a release tag.
2. Verify main and develop protections: native required checks, strict current-base requirement, resolved conversations, administrator enforcement, and no force-push or deletion. Expand required checks only after the default branch contains the corresponding jobs.
3. Enable Actions PR creation and repository native auto-merge. Keep the default Actions token read-only; individual workflows declare their narrower needs. Do not add a personal access token secret to work around event restrictions.
4. Enable immutable releases with an administrator credential and read the setting back. This applies to future publications. The workflow token cannot perform this administration-only preflight.
5. Dispatch **Synchronize development** on `main`, inspect the bot PR, approve native checks if GitHub requires it, and confirm protected synchronization completes. Do not enable auto-merge on a release candidate.
6. With `RELEASE_AUTOMATION_ENABLED` still unset, explicitly dispatch **Prepare release** with `source=develop`. Inspect the real bot candidate, generated version/changelog, and canonical-record checks.
7. Dispatch **Release** on that candidate branch with its exact SHA as `expected_sha`. This is signing preview only. Record the run, candidate ref/commit, all checks, and resulting distribution hashes.
8. Complete the acquisition/install/switching exercise below. Fix discrepancies before activation; do not claim an unperformed provider or compatibility test passed.
9. Enable `RELEASE_AUTOMATION_ENABLED=true`, read back the setting and protections, and leave the first canonical release PR open for human merge approval. Do not merge it merely to finish setup.

## Preview acceptance record

The [signed-preview acceptance record](../records/signed-preview-acceptance.md) covers actual acquisition, source/integrity verification, and negative checks for one pinned candidate. Installation and distinct-build upgrade/rollback remain separate acceptance work.

Retain a reviewed record in the repository with concrete run IDs, source identities, hashes, commands, and results. Avoid numeric test totals as permanent claims; they drift.

| Exercise | Evidence required |
|---|---|
| Both native packages | Actual hosted Linux/macOS build, attribution, extracted version/help checks |
| Signed distribution | Four successful subject verifications against exact repository, commit, ref and workflow |
| Acquisition | Download the actual five-file preview artifact, not locally reconstructed substitutes |
| Installation | Use dedicated HOME and project directories, preserve attribution, check selected executable path/version |
| Credential-free startup | Run a scripted/headless interface check and label it as deterministic, not a real model |
| Program switching | Retain an old preview/development executable, stage the new one, switch the command pointer, and demonstrate restoring the old pointer |
| Data boundary | Use only dedicated test history; demonstrate backup/restore separately from program switching |
| Real provider | Record separately if explicitly performed; never infer it from help/version or scripted output |

There are not yet two official releases to establish an official upgrade matrix. A development/preview switching exercise demonstrates that procedure, not arbitrary cross-version storage compatibility. Do not touch the runtime, model credentials, or user history carrying the maintainer's current conversation.

### Repeatable runtime check

After separately verifying and extracting both packages, run `python3.12 scripts/rehearse_runtime.py /absolute/old/lattice /absolute/new/lattice`. This is not an installer or signature verifier. It preserves the supplied executables and attribution, creates its own temporary HOME/project/socket, and never inherits model credentials. It tests four clean process lifetimes: old, new, old again with the newer test history, and old after restoring the pre-upgrade backup. The report checks file names and bytes, not arbitrary filesystem metadata.

The Node probe correlates the unique input, model request, successful result, reply and sibling turn boundary. Restarts must carry the preceding **expanded input** into new model material; merely replaying a raw input is insufficient. Each daemon must acknowledge startup, finish a new scripted turn and stop normally. CI passes the same binary twice to test this machinery; `distinctBinaries: false` explicitly does **not** demonstrate a cross-build upgrade. The helper removes only its own temporary test tree when finished.

## Normal release and hotfix

Use [candidate preparation](release-preparation.md), [version policy](versioning.md), [artifact contents](release-artifacts.md), and [publication/recovery rules](release-publication.md). Development changes refresh an unmerged candidate; merging the canonical candidate to main is the publication approval. No separate version-edit/tag/publish ritual is required in the normal path. Explicit version overrides still go through review.

Hotfix work goes through reviewed `hotfix/*` → `main`, then a separate main-sourced `release/hotfix-next` candidate. Do not bring unfinished develop work into a hotfix. Return stable history to develop through the protected sync PR.

## When a workflow stops

Read the exact failing job before rerunning. A failed request may have succeeded remotely. Never use a force-pushed tag, overwritten release asset, administrator merge bypass, or custom replacement status to conceal failure.

- Candidate creation: inspect existing PR/ref state; closed-without-merge candidates pause until reopened.
- Native checks: fix the source through a PR, including expired dependency exceptions only after review.
- Signing: check event SHA equals build SHA and dispatch uses the correct candidate ref. A checkout does not change OIDC source identity.
- Draft upload: retain the original signed distribution. Same-run recovery uses `sealed-release-<commit>` for 14 days; changed rebuilt bytes are not substitutes.
- Publication response lost: outcome is unknown; query before acting. A later confirmation failure means publication already occurred.
- Synchronization: retry the sync job or dispatch its main-only recovery workflow. Respect human PR ownership and native checks; conflicts need review, not automatic resolution.

There is deliberately no automatic deletion or destructive cleanup path. Repository immutability is checked after publication; a failure there is an incident to investigate, not permission to pretend publication did not happen.
