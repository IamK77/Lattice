# Versions and change history

`Cargo.toml` is the sole authority for the Rust product version. A release must
use the same version in the package manifest, lockfile package entry, tag
(`vX.Y.Z`), executable output, release notes, and artifact names. Private frontend
package metadata is synchronized when preparing a release; it is not published
to npm.

## Build identity

- An ordinary build reports `vX.Y.Z-dev+g<commit>` when Git metadata is available.
- An archive without its own Git repository reports `vX.Y.Z-dev`.
- A release build sets `LATTICE_RELEASE_VERSION=X.Y.Z`; the build fails if that
  value differs from the Cargo package version. Its executable reports `vX.Y.Z`.

Git identifies a development baseline, not a clean-worktree guarantee. A local
build can contain edits not represented by that commit. Release provenance and
checksums, not the version string alone, identify distributed official artifacts.
The release optimization profile does not by itself turn a local build into an
official release. Existing prerelease/build metadata remains valid in development
version strings.

## Commits and changelogs

Write English Conventional Commits: `feat: ...`, `fix(cli): ...`, `docs: ...`,
`ci: ...`, `build: ...`, and the other accepted types in the contribution guide.
Mark incompatible changes with `!` or a `BREAKING CHANGE:` footer and explain the
migration. There is no custom `Type:` trailer requirement. Existing historical
trailers are retained as history, but no longer determine versions.

Changes are summarized in [CHANGELOG.md](../CHANGELOG.md). Merge commits are not
counted as additional changes. Pull request and merge subjects can describe the
actual change; they do not all have to use `chore`. Release preparation itself
uses `chore(release): ...`.

A release candidate proposes a version and updates the changelog for review.
For an established release line, fixes/performance corrections suggest a patch,
new compatible features suggest a minor version, and incompatible changes
suggest a major version. Before 1.0, incompatible changes suggest a minor version.
Documentation or maintenance alone does not require a new binary release.
The initial release uses the version already selected in `Cargo.toml`.

The maintainer confirms timing and scope by merging the release PR into `main`.
Preparation is not publication. Do not invent historical releases, silently
retag a published version, or treat a source commit count as a release number.
