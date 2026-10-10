# Installation, upgrades, and rollback

[Documentation](../README.md) · [中文](installation.zh-CN.md) · [First conversation](getting-started.md) · [Troubleshooting](troubleshooting.md)

## Availability and platform scope

**The first official release has not shipped yet.** Use a source build today, or a specifically identified signing-preview run supplied by a maintainer. Commands for named GitHub Releases below become applicable only after one exists. A preview is not a stable release, even when it prints `vX.Y.Z`.

Choose one route; the other routes are not prerequisites:

| What you have | Next step |
| --- | --- |
| A source checkout | [Build it](#source-build-available-now), then follow [First conversation](getting-started.md). You do not need release-download commands. |
| An explicitly supplied signing-preview run | Follow [preview verification](#verify-a-signing-preview); do not apply official-tag assumptions. |
| An existing named official release | [Verify that release](#verify-a-named-official-release-before-running-it) before [installing the package](#user-level-installation). |
| An existing installation to replace | Read [upgrade and rollback precautions](#upgrade-without-losing-the-previous-program) first. |

### Platform scope

| Package target | Tested build/startup environment |
|---|---|
| `x86_64-unknown-linux-gnu` | Ubuntu 24.04, x86-64 |
| `aarch64-apple-darwin` | macOS 15, Apple Silicon |

Other system versions and architectures are untested; a native Windows package is currently unavailable. Packages contain the program, attribution, licenses, build records and dependency reports; see [artifact contents](../maintenance/release-artifacts.md). macOS packages currently lack Apple Developer ID signing and notarization.

Install the browser, desktop driver, language servers and Ink client as needed. Configure your model account at first startup.

## Source build available now

Use Rust 1.89 or newer, Cargo and a C toolchain. Linux also needs OpenSSL development headers and `pkg-config`.

```bash
git clone https://github.com/IamK77/Lattice.git
cd Lattice
# For a repeatable build, inspect and select a specific reviewed commit first.
cargo build --locked --release --bin lattice
./target/release/lattice --version
```

The version output identifies this as a development build. Keep the source licenses and attribution files, then follow [First conversation](getting-started.md) to set the program path and connect a model.

## Verify a named official release before running it

These are **Bash** examples. Install GitHub CLI separately and authenticate as required. Choose an explicit published version, not an unreviewed `latest` download. Run in a fresh directory and stop on any failed check:

```bash
set -euo pipefail
export GH_HOST=github.com
version='X.Y.Z'  # Replace with an existing reviewed release.
work="$(mktemp -d)"
cd "$work"
repo=IamK77/Lattice
release_ok="$(gh api "repos/$repo/releases/tags/v$version" --jq '.immutable == true and .draft == false and .prerelease == false')" || exit 1
[[ "$release_ok" == true ]] || exit 1
commit="$(gh api "repos/$repo/git/ref/tags/v$version" --jq '.object | select(.type == "commit") | .sha')" || exit 1
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || exit 1
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

The release workflow uses lightweight tags. The script stops on other tag types; ask the maintainer to confirm the corresponding commit. Next run `sha256sum --check SHA256SUMS` on Linux or `shasum -a 256 --check SHA256SUMS` on macOS and confirm that both archives pass. See [artifact contents](../maintenance/release-artifacts.md) for build-environment and signing-job records.

## Verify a signing preview

Get the preview's run ID, candidate commit, candidate branch, version and verification instructions from the maintainer. Download both archives, `SHA256SUMS`, `release-manifest.json` and `provenance.json` from that run's `release-preview` Actions artifact. Ask the maintainer to fill in missing information before continuing.

Verify attestations and checksums against that preview's candidate commit and branch. Previews use the candidate branch identity; official releases use `refs/heads/main` and a release tag. Verify each through its respective route. The [signed-preview acceptance record](../records/signed-preview-acceptance.md) gives a complete example; check each new preview's own files and identity.

## User-level installation

This step is for a verified package, not a source checkout. Stay in its download directory and set `version` to the version you verified; a preview must have passed its own verification first. Choose the target from the table for your actual machine. Keep the complete extracted directory, including its attribution.

```bash
set -euo pipefail
: "${version:?Use the version verified in the previous step}"
target='aarch64-apple-darwin'  # Or x86_64-unknown-linux-gnu on the tested Linux target.
root="$HOME/.local/share/lattice/versions"
destination="$root/v$version-$target"
mkdir -p "$root" "$HOME/.local/bin"
[[ ! -e "$destination" && ! -L "$destination" ]] || exit 1
mkdir "$destination"
tar -xzf "lattice-v$version-$target.tar.gz" --strip-components=1 -C "$destination"
installed_version="$("$destination/lattice" --version)" || exit 1
[[ "$installed_version" == "v$version" ]] || exit 1
"$destination/lattice" --help
```

For a **first installation only**, create `~/.local/bin/lattice` as a symlink to `"$destination/lattice"` with `ln -s`. Do not replace an existing file or link without inspecting it. Add `~/.local/bin` to your shell's PATH, then inspect `command -v lattice`, `ls -l "$HOME/.local/bin/lattice"`, and `lattice --version`. A different installation earlier in PATH can otherwise keep winning.

Connect a model and start in your project directory using [First conversation](getting-started.md). Follow the corresponding tool guide when you need a browser or desktop integration.

## Upgrade without losing the previous program

1. Read the change notes and compatibility warnings. There is no self-update command.
2. Stop sessions and the daemon normally before backing up data or changing the selected program. Do not replace the runtime carrying your current agent conversation from inside that conversation.
3. Back up the **complete** `~/.lattice` data tree to a private location, excluding only transient socket files. Also include externally configured model, preference, baseline, and overlay files, and attachment/document directories referenced outside that tree. A `.ledger` is a directory; copying only JSONL is not a complete backup. Inspect backup completeness and protect its permissions: it may contain keys and unredacted history. Do not dump environment variables to create a backup.
4. Verify and extract the new version into a **new** version directory. Do not overwrite the old one. Run the new program's version/help checks before switching.
5. Inspect the existing command link and record its old target. If it is an unmanaged file, a link to a directory, or points outside your versioned installation, stop and use that installation's own management method.
6. Create a fresh temporary symlink beside `~/.local/bin/lattice`, pointing to the new executable, then rename it over the existing **symlink**. Keeping both links on the same filesystem permits an atomic pointer change; retain the old version directory and your recorded old target. Do not edit or truncate a running binary.
7. Recheck PATH and version. Start with dedicated test data before opening valuable existing history.

**Handle the program and data separately during rollback.** Switching the command link back restores the old program; data written by the newer version remains. Check release notes to see whether the old program can read that data. If an older format is needed, stop all sessions, set aside the newer data, then restore the complete pre-upgrade backup.

Uninstalling the command link and an explicitly selected program directory is separate from deleting user data. Do not remove `~/.lattice` as an installation cleanup step. Keep backups and version directories until you have deliberately decided they are no longer needed.
