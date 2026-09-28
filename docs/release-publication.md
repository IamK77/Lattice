# Release publication

[中文](发布执行.md) · [Candidate preparation](release-preparation.md) · [Artifact contents](release-artifacts.md)

## Approval and authority

Merging the canonical `release/next` or `release/hotfix-next` PR into protected `main` authorizes publication. The `Release` workflow verifies the approved merge SHA, candidate contents, version history, and any existing tag again. A workflow-bootstrap PR does not publish. Registry publication remains out of scope.

Publication stays disabled until maintainers enable the `RELEASE_AUTOMATION_ENABLED` repository variable. This document describes the implemented workflow, not evidence that a release has already shipped. Native packaging has been exercised separately; the first end-to-end signing preview and production activation still require the rollout checklist.

The workflow event SHA, checked-out SHA, and approved build SHA must agree. GitHub's signing identity describes the event commit, not any arbitrary commit subsequently checked out. Manual previews must therefore be dispatched **on the candidate branch**, with its exact current SHA as `expected_sha`. Selecting `main` and supplying a different candidate SHA is rejected. Allowed preview refs are the corresponding canonical candidate branch and `main`; only `main` can authorize official publication.

## Jobs and published files

1. A read-only job validates identity and approval.
2. Separate read-only Linux and macOS jobs compile, inspect, archive, extract, and run startup checks. They have neither signing nor publication permission.
3. A separate job inspects downloaded archives as data. It never executes a distributed binary. It generates the two-platform manifest and checksum file.
4. GitHub signs provenance for four subjects: the two native archives, `SHA256SUMS`, and `release-manifest.json`. The portable bundle is saved as `provenance.json`.
5. The workflow verifies each subject against this repository, the exact commit, source ref, `release.yml` signing workflow, and a hosted signing runner.
6. For an approved merge only, it reserves an unmoved `vX.Y.Z` tag, creates a **draft**, uploads all five files, verifies GitHub's reported hashes, and publishes last. It then confirms immutability, the final tag, and the asset set.

The hosted-runner verification option identifies the **signing** job. The native build hosts are separately fixed by workflow policy; that option alone does not prove where every preceding job ran.

Preview signing has `contents: read`, not release-write permission. It still writes an attestation and an Actions artifact; “preview” does not mean zero remote writes. It never creates a tag or GitHub Release. The archive receipt and manifest both explicitly mark previews. A signed preview is not an official release.

## Portable verification

For an official release, verify each of the four subjects with:

```bash
gh attestation verify "$asset" --bundle provenance.json \
  --repo IamK77/Lattice --source-digest "$approved_commit" \
  --source-ref refs/heads/main \
  --signer-workflow IamK77/Lattice/.github/workflows/release.yml \
  --deny-self-hosted-runners
```

Use the exact reviewed commit, not an archive hash, for `--source-digest`. For a preview, use its actual candidate ref instead of `refs/heads/main`. Check both archive checksums too. The installation guide will describe acquiring and installing a verified artifact; neither a checksum alone nor an unsigned `BUILD-INFO.json` authenticates a release.

## Failure and retry rules

- **Before a tag or draft exists:** rerun after diagnosing the failing check. No version was published.
- **A request failed or its response was lost:** inspect the draft and tag first. The server may already have accepted the operation. The script performs no hidden retry, deletion, replacement, or retagging.
- **An asset already exists:** its name, completed state, size, and SHA-256 must match. Matching bytes are reused; differing, unexpected, duplicate, or incomplete assets stop publication.
- **A provenance bundle was uploaded:** staging downloads and verifies that original bundle. It does not replace it with a fresh signature, whose bytes would legitimately differ.
- **Original signed bytes are retained before any tag or draft mutation.** The complete five-file distribution is saved as `sealed-release-<commit>` in this Actions run, without overwrite permission. Both whole-run and failed-job retries locate that original artifact by run, commit, ID, and digest; whole-run retries skip rebuilding when it exists. Retention is 14 days. Byte-for-byte reproducible rebuilding is not promised. Expired or missing originals require investigation, not `--clobber`, a fresh signature over substituted bytes, or a force-pushed tag.
- **Already published:** a rerun downloads and re-verifies the immutable release. It does not rebuild, overwrite assets, or publish again. This also permits a later synchronization step to recover independently.
- **Publication succeeded but a final check failed:** the error explicitly says publication already occurred. Do not describe this as “nothing happened” or try to roll it back automatically.

## Repository rollout prerequisites

A repository administrator must enable immutable releases and read the setting back **before activation**. The Actions repository token cannot query that administration-only setting; the runtime instead checks the published release's `immutable` field. A disabled setting discovered after publication is an incident, not a reversible preflight failure.

Before setting the activation variable, land the workflow on `main` through a non-publishing bootstrap PR, complete the real signing-preview exercise, enable Actions PR creation, confirm branch checks and protected synchronization, and verify that no first release is being published unintentionally. The first canonical release PR remains a human approval gate.
