# Troubleshooting

[中文](troubleshooting.zh-CN.md) · [Installation](installation.md) · [Support](../SUPPORT.md)

Start with the smallest observation. Do not reset by deleting `~/.lattice`, overwrite an existing model catalog, dump the environment, or upload a complete ledger.

## Wrong program or unsupported platform

Compare `command -v lattice`, the command symlink, and `lattice --version`. Optimized development builds are not official releases. For a package that cannot start, record the operating system and architecture and compare the [tested platform table](installation.md). A missing dynamic library or symbol on an older system is not resolved by renaming the archive or disabling verification. Inspect `SYSTEM-DEPENDENCIES.txt`; compatibility with older systems is not promised. Use a supported environment or review a source build.

Linux source builds need development headers, not just runtime OpenSSL libraries, and `pkg-config`. Optional Browser, Desktop, language servers, and the Ink client have separate prerequisites described in [First conversation](getting-started.md). Do not disable operating-system security checks merely because a download fails to run; verify provenance and investigate the exact diagnostic first.

## No usable model or unexpected provider

Check the catalog path and variable **names**, not secret values. `LATTICE_MODELS` selects the catalog; an empty value disables the default catalog. Preserve existing entries when editing.

Any one of `LATTICE_ADAPTER`, `LATTICE_MODEL`, `LATTICE_BASE_URL`, or `LATTICE_API_KEY_ENV` selects the environment-configuration route. It is not a per-field merge with the saved model. If you intended to use the catalog and saved choice, remove those four overrides from this launch. Otherwise configure the environment route completely.

Without those overrides, the saved reachable model wins, then the first catalog entry with available credentials. A saved entry whose credentials disappeared can fall back with a warning; a deleted entry silently falls back. There is no supplied account or free default model. Review `/model` before sending sensitive work to a provider.

For provider rejection, check the endpoint root, model ID, limits, and supported protocol/settings. To omit the thinking parameter entirely:

```bash
LATTICE_THINKING= "$LATTICE_BIN"
```

Unset means “saved preference, otherwise high”; `off` explicitly sends disabled thinking. Neither is the same as omitting the parameter.

`LATTICE_SCRIPTED` is a test-mode switch based on presence. `LATTICE_SCRIPTED=0` still enables it; remove the variable to return to a real model. Scripted mode is not a sandbox and can still load configured assembly additions.

## A conversation is missing or damaged

Resume is scoped to the current project directory. Older records without a project identity remain visible across projects. Check which project directory you launched from before assuming data is lost.

New terminal histories are directories under `~/.lattice/ledgers/`; older `tui/` and `chat/` locations remain discoverable, while daemon streams use `~/.lattice/streams/`. Keep referenced document and attachment directories with the history. `LATTICE_HOME` is not a general data-root setting: paths use `HOME` and their individual override variables.

For a bounded, credential-free, read-only observation, use `lattice --recover PATH --offset BYTES --bytes COUNT`. For a segmented ledger, `lattice --verify-ledger DIRECTORY` performs the full read-only verification. Even their output can contain sensitive material: review it locally before sharing. Normal conversation recovery is not a read-only diagnostic; it can repair incomplete tails and settle interrupted calls.

Do not use `compact`, `tidy`, or `--migrate-ledger` as generic checks. They can write, move, or delete data. Migration requires an offline source and retains original files; references to external attachments still need care. See [installation and rollback](installation.md) before changing a data format.

## A tool or extension behaves unexpectedly

The workspace and declared tool effects are not an operating-system sandbox. Stop and inspect an interrupted action before repeating it: it may already have partially happened. Check the selected baseline and installation overlay separately; `assembly.json` is normally an overlay, not the full baseline. Avoid sharing an assembly export without inspection because it can include sensitive configuration and prompts.

A missing language server is reported, not installed automatically. Browser needs Chrome/Chromium; Desktop needs its external driver and macOS permissions. A successful CLI startup does not validate those integrations.

## Useful support information

Provide the exact build identity, operating system/architecture, how it was installed, the smallest reproduction, expected versus actual behavior, and a short inspected error excerpt. For release verification failures, include the release or preview-run identity and which check failed, never credentials. Security-sensitive reports belong in the [private reporting route](../SECURITY.md), not a public issue.
