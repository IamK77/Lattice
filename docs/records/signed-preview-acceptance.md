# Signed preview acceptance

[中文](signed-preview-acceptance.zh-CN.md) · [Maintainer checklist](../maintenance/maintainer-handbook.md)

This is a point-in-time result for the source below, not approval to publish or
proof that later commits have been signed. The record and acceptance test are
added after the measured build; do not rebuild endlessly just to include the
record in its own source identity.

## Exact inputs and runs

- Repository: `IamK77/Lattice`; candidate PR: [#31](https://github.com/IamK77/Lattice/pull/31).
- Candidate version: `0.1.0`; ref: `refs/heads/release/next`.
- Signed source: `aab7d1e06f236f9d986bdca50db455ddd0b4daa0`.
- Development snapshot: `690c36d7ca964b697332a05622d44a7593b271bf`.
- Candidate preparation: `36540069785`; required CI: `36540101596`; native package checks: `36540101829`.
- [Signing-preview run 36540553467](https://github.com/IamK77/Lattice/actions/runs/36540553467): identity, both native builds, and preview succeeded. Publication and synchronization jobs were skipped.
- Downloaded artifact: `release-preview`, ID `11020561940`, from that exact run. Actions retention is 14 days; expiry is not permission to substitute rebuilt bytes.
- Independent consumer check: macOS ARM64, GitHub CLI `2.93.0`, Python `3.12.13`.

The first candidate's CI run `36538643456` exposed tests that copied the real
candidate changelog into an allegedly unpublished fixture. [PR #32](https://github.com/IamK77/Lattice/pull/32)
replaced those inputs with fixed test data. The new isolation regression failed
before the fix and passed afterward. Production duplicate-version checks and the
real changelog were not weakened. The refreshed candidate's checks passed.

## Downloaded file identities

| File | SHA-256 |
|---|---|
| `lattice-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` | `73632ebe55887f0b6a3c7ae52b3f7c638d4f79778d112f34d1aed423b0f69f14` |
| `lattice-v0.1.0-aarch64-apple-darwin.tar.gz` | `befaf12f21bb85895f5613e0cf5c8af7d2b3e732c8107a7c80521f91a545646c` |
| `SHA256SUMS` | `4ac47eecb9e3c904920861d7a1f99e300423ebe8e67437c77c026789c690fada` |
| `release-manifest.json` | `5a6b0037dbae151d80364f885355f14720ad9bbb6f20a5d73aa3b7c24400277a` |
| `provenance.json` | `8860738fa0750bf7dc901083c44104b56f1aa0b25178de8d1718198fa479f19a` |

## Repeatable acceptance

Authenticate `gh` with access to the repository's Actions artifacts. Use an empty
download destination, and obtain the expected commit/ref from the reviewed run,
not from the downloaded manifest. From the repository root:

```bash
assets="$PWD/target/signed-preview-36540553467"
gh run download 36540553467 --repo IamK77/Lattice \
  --name release-preview --dir "$assets"
python3.12 scripts/accept_signed_preview.py \
  --directory "$assets" --version 0.1.0 \
  --commit aab7d1e06f236f9d986bdca50db455ddd0b4daa0 \
  --source-ref refs/heads/release/next
```

This opt-in acceptance suite passed against the actual downloaded bytes:

- Both archives, checksums, and manifest verify against the exact repository,
  commit, candidate ref, `release.yml` signer workflow, and hosted signing runner.
- Archive contents/build identities and exact checksum/manifest contents agree.
- Changing one byte in a temporary archive copy is rejected.
- A wrong source commit is rejected with a `SourceRepositoryDigest` mismatch.
- Claiming the preview came from `main` is rejected with a `SourceRepositoryRef` mismatch.
- Each negative test is surrounded by successful verification of the original;
  expected rejection diagnostics are checked, and original bytes remain unchanged.

The suite needs downloaded signed assets and network access to verification
infrastructure. It is deliberately not part of ordinary unit-test discovery.
A changed CLI diagnostic requires inspection, not loosening the test to accept
any nonzero exit as proof of cryptographic rejection.

## Scope

This accepts acquisition, source identity, integrity, and negative verification
for this preview. Native build jobs also executed extracted version/help checks.
The consumer suite only inspects data; it never executes a distributed binary.
It does not accept installation, distinct-build upgrade/rollback, real providers,
reproducible builds, Apple notarization, or an actual immutable publication.

The candidate remains open without auto-merge, no release/tag was created, and
`RELEASE_AUTOMATION_ENABLED` remains unset at this acceptance stage.
