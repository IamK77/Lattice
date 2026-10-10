# Installation, upgrades, and rollback

[中文](installation.zh-CN.md) · [First conversation](getting-started.md) · [Troubleshooting](troubleshooting.md)

## Availability and platform scope

**The first official release has not shipped yet.** Use a source build today, or a specifically identified signing-preview run supplied by a maintainer. Commands for named GitHub Releases below become applicable only after one exists. A preview is not a stable release, even when it prints `vX.Y.Z`.

| Package target | Build/startup checks exercised | Not established by those checks |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Ubuntu 24.04, x86-64 | Older distributions, musl, ARM Linux |
| `aarch64-apple-darwin` | macOS 15, Apple Silicon | Older macOS, Intel Macs |

There is no supported native Windows package. The archives contain the CLI, attribution, license texts, build records, and dependency reports; see [artifact contents](release-artifacts.md). Browser, Desktop, language servers, model credentials, and the optional Ink client are not installed by extracting the CLI. No Apple Developer ID signing or notarization is claimed.

## Source build available now

Use Rust/Cargo and a C toolchain. Linux builds also need OpenSSL development headers and `pkg-config`. The declared minimum Rust is 1.89; CI checks compilation at 1.89.0, not the entire test suite at that version.

```bash
git clone https://github.com/IamK77/Lattice.git
cd Lattice
# For a repeatable build, inspect and select a specific reviewed commit first.
cargo build --locked --release --bin lattice
./target/release/lattice --version
```

An optimized source build still reports a development identity. Do not set `LATTICE_RELEASE_VERSION` merely to make a local build look official. Keep the source checkout and its license/notice files together. Set `LATTICE_BIN` to the absolute binary path for the [configuration guide](getting-started.md); that name is a shell convenience, not a product setting.

## Verify a named official release before running it

These are **Bash** examples. Install GitHub CLI separately and authenticate as required. Choose an explicit published version, not an unreviewed `latest` download. Run in a fresh directory and stop on any failed check:

```bash
set -euo pipefail
export GH_HOST=github.com
version='X.Y.Z'  # Replace with an existing reviewed release.
work="$(mktemp -d)"
cd "$work"
repo=IamK77/Lattice
[[ "$(gh api "repos/$repo/releases/tags/v$version" --jq '.immutable == true and .draft == false and .prerelease == false')" == true ]]
commit="$(gh api "repos/$repo/git/ref/tags/v$version" --jq '.object | select(.type == "commit") | .sha')"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]]
gh release download "v$version" --repo "$repo" --dir "$work" \
  --pattern 'lattice-*.tar.gz' --pattern SHA256SUMS \
  --pattern release-manifest.json --pattern provenance.json
for asset in lattice-v"$version"-*.tar.gz SHA256SUMS release-manifest.json; do
  gh attestation verify "$asset" --bundle provenance.json \
    --repo "$repo" --source-digest "$commit" --source-ref refs/heads/main \
    --signer-workflow IamK77/Lattice/.github/workflows/release.yml \
    --deny-self-hosted-runners
done
```

The current pipeline creates lightweight tags; an unexpected tag type is a reason to inspect, not guess. On Linux run `sha256sum --check SHA256SUMS`; on macOS run `shasum -a 256 --check SHA256SUMS`. Both archive checks must pass. The signing-runner constraint does not independently prove every earlier build host; the workflow policy fixes those hosts separately.

For a preview, obtain all five files from the explicitly supplied `release-preview` Actions artifact. Verify against that run's exact candidate commit and candidate ref, not `refs/heads/main`. There is no release tag or immutable GitHub Release to query. Never relabel this as official-release verification.

## User-level installation

Choose the target from the table for your actual machine. Keep the complete extracted directory, including its attribution. After verification:

```bash
target='aarch64-apple-darwin'  # Or x86_64-unknown-linux-gnu on the tested Linux target.
root="$HOME/.local/share/lattice/versions"
destination="$root/v$version-$target"
mkdir -p "$root" "$HOME/.local/bin"
[[ ! -e "$destination" && ! -L "$destination" ]]
mkdir "$destination"
tar -xzf "lattice-v$version-$target.tar.gz" --strip-components=1 -C "$destination"
[[ "$("$destination/lattice" --version)" == "v$version" ]]
"$destination/lattice" --help
```

For a **first installation only**, create `~/.local/bin/lattice` as a symlink to `"$destination/lattice"` with `ln -s`. Do not replace an existing file or link without inspecting it. Add `~/.local/bin` to your shell's PATH, then inspect `command -v lattice`, `ls -l "$HOME/.local/bin/lattice"`, and `lattice --version`. A different installation earlier in PATH can otherwise keep winning.

Now configure a model and start in your project directory using [First conversation](getting-started.md). `--version` and `--help` passing does not prove a paid provider or optional integration works.

## Upgrade without losing the previous program

1. Read the change notes and compatibility warnings. There is no self-update command.
2. Stop sessions and the daemon normally before backing up data or changing the selected program. Do not replace the runtime carrying your current agent conversation from inside that conversation.
3. Back up the **complete** `~/.lattice` data tree to a private location, excluding only transient socket files. Also include externally configured model, preference, baseline, and overlay files, and attachment/document directories referenced outside that tree. A `.ledger` is a directory; copying only JSONL is not a complete backup. Inspect backup completeness and protect its permissions: it may contain keys and unredacted history. Do not dump environment variables to create a backup.
4. Verify and extract the new version into a **new** version directory. Do not overwrite the old one. Run the new program's version/help checks before switching.
5. Inspect the existing command link and record its old target. If it is an unmanaged file, a link to a directory, or points outside your versioned installation, stop and use that installation's own management method.
6. Create a fresh temporary symlink beside `~/.local/bin/lattice`, pointing to the new executable, then rename it over the existing **symlink**. Keeping both links on the same filesystem permits an atomic pointer change; retain the old version directory and your recorded old target. Do not edit or truncate a running binary.
7. Recheck PATH and version. Start with dedicated test data before opening valuable existing history.

**Rollback has two different parts.** Switching the command link back restores the old program. It does not undo data written by the new program. If formats changed, restore the pre-upgrade data backup with all sessions stopped and leave the newer data aside for inspection. No arbitrary cross-version downgrade guarantee is made. Pre-1.0 version policy and the stable bridge protocol are not blanket storage-compatibility promises.

Uninstalling the command link and an explicitly selected program directory is separate from deleting user data. Do not remove `~/.lattice` as an installation cleanup step. Keep backups and version directories until you have deliberately decided they are no longer needed.
