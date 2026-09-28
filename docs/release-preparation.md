# Preparing a release

[简体中文](发布准备.md) · [Version policy](versioning.md)

Preparation proposes a release; it does not create a tag, publish a GitHub
Release, or upload to a package registry. The automation is off by default.
Keep `RELEASE_AUTOMATION_ENABLED` unset until the publication pipeline has passed
its dry run. Enabling that repository variable is a maintainer operation, not a
substitute for reviewing a release PR.

## Normal path

1. Merge reviewed changes into `develop` using the normal protected-branch flow.
2. With automation enabled, **Prepare release** computes a candidate from that
   immutable source commit and opens or refreshes `release/next` targeting `main`.
3. Review the proposed version, change notes, compatibility/migration guidance,
   and CI. GitHub may require approval before repository-bot PR checks run.
4. Merge the release PR only when ready to publish. Never enable automatic merge
   on this PR. The publication pipeline, rather than this preparer, owns the
   approved merge commit's tag and artifacts.

The workflow requires the repository setting that allows Actions to create PRs.
It uses the short-lived repository token, with contents and pull-request write
permissions; no personal access token is stored as a secret. It never writes
straight to `main` or `develop`, force-pushes, or bypasses their required checks.

## Version and change notes

Only stable `X.Y.Z` releases are automatically prepared. Prerelease channels need
an explicit future policy; development build identifiers remain supported.
The first proposal uses Cargo's selected version. After a published release,
`feat` suggests a minor increment, `fix`/`perf`/`revert` a patch, and an incompatible
change a major increment (minor before 1.0). The highest applicable increment wins.
Maintenance alone does not request a release.

To override the proposal, change the Cargo package version on the source branch
through a normal reviewed PR. It must be above the last published stable version.
Provide reviewable notes under `## [Unreleased]` in `CHANGELOG.md`; an explicit
version override does not manufacture notes for a maintenance-only release.

The preparer synchronizes Cargo, its local lockfile entry, the private frontend
package and lockfile, and the Changelog. Locked dependency versions are retained.
Curated notes and historical sections survive; user-facing non-merge commits add
candidate notes. The generated `.release-candidate.json` records the source,
previous publication, and proposed version. It is an audit record, not an approval.

## Updates, interruptions, and deliberate edits

- Repeated preparation of the same source is idempotent. Updates retain ancestry
  and use fast-forward ref updates, including recovery after PR creation failed.
- Candidate code or generated-file edits stop regeneration rather than being
  overwritten. Put intended changes on the source branch for another review.
- Manual PR titles and text outside the generated description markers are kept.
  Do not remove those markers. Avoid editing the description during a running
  preparation job; GitHub metadata edits are not a transactional editor session.
- Closing a PR without merging pauses its candidate line. Reopen it and rerun
  **Prepare release**, or let the next source push trigger preparation.
- If an explicit version override is withdrawn and no releasable changes remain,
  the stale PR is closed with an explanation; outside review notes are retained.
- A draft release blocks a new preparation. Finish or explicitly resolve the
  interrupted publication first. A tag or published version must never be moved.

For recovery, manually run **Prepare release** with `source=develop`. This is a
preparation retry, not a publication button. Failures are visible rather than
silently retried. Do not solve them by bypassing branch protection or force-pushing.

## Hotfix isolation and synchronization

A reviewed `hotfix/*` merged into `main` prepares `release/hotfix-next` from
`main`, not from unfinished development. Manual recovery uses `source=main`.
The two candidate lines are separate. Before preparing a normal release again,
synchronize the stable branch into `develop` through its protected PR flow.
A source that does not contain current `main`, or a candidate whose recorded
source diverged from the current source history, is rejected for review.
