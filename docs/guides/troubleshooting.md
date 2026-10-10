# Troubleshooting

[Documentation](../README.md) · [中文](troubleshooting.zh-CN.md) · [Installation](installation.md) · [Support](../../SUPPORT.md)

Find the symptom below. Start with the program version, configuration path and specific error. Keep `~/.lattice` and existing model configuration: they contain account settings and conversation records. When asking for help, share the relevant error excerpt with keys and sensitive project content removed.

## Wrong program or unsupported platform

Use `command -v lattice` and `lattice --version` to identify the running program, then check that the command symlink points to the intended version. Development builds identify themselves in the version output.

If a package cannot start, record the system version, processor architecture and complete error, then compare the [tested platform table](installation.md). For missing libraries or symbols, consult the package's `SYSTEM-DEPENDENCIES.txt`. Older systems may require a tested environment from the table or a source build. If the system blocks a downloaded package, verify its source using the installation guide, then investigate the specific error.

Linux source builds require OpenSSL development headers and `pkg-config`; install the development packages. See [Optional tools](getting-started.md#optional-tools) for browser, desktop, language-server and Ink setup.

## No usable model or unexpected provider

Use [Model configuration](model-configuration.md) to check the service address, API format, model name and key. If the configuration file cannot be read, back it up and repair it using the error message. Lattice preserves the original file to protect existing accounts.

### Saved configuration is missing or ignored

Check the configuration path first. `LATTICE_MODELS` selects the model configuration file; an empty value disables the default file. Preserve existing models when editing.

Next check whether `LATTICE_ADAPTER`, `LATTICE_MODEL`, `LATTICE_BASE_URL` or `LATTICE_API_KEY_ENV` is set. Any one of these switches startup to the complete environment-variable configuration. To use saved models, remove all four from this launch environment; to use environment configuration, fill it in completely. Inspect variable names while keeping key values local.

With saved configuration, Lattice first selects the previously chosen model with available credentials, then tries the first configured model with credentials. Missing credentials for the previous model produce a fallback warning; deleted models are skipped. Use `/model` to see the actual selection before sending more work.

### The provider rejects a request

Check the service's base address, exact model name, capacity, API format and related settings. If the error identifies an unsupported thinking parameter, omit it. Set `LATTICE_BIN` as shown in [First conversation](getting-started.md), then run:

```bash
LATTICE_THINKING= "$LATTICE_BIN"
```

The empty value omits the thinking parameter. The other forms work differently: leaving the variable unset uses the saved preference, or `high` when there is none; setting it to `off` explicitly requests disabled thinking.

### Replies look scripted instead of coming from a provider

Check for and remove `LATTICE_SCRIPTED`. Its presence enables test mode, including when set to `0`. Test mode still loads configured runtime extensions, and tools can still operate on local files.

## A conversation is missing or damaged

Return to the project directory where you created the conversation, set the program path using [First conversation](getting-started.md#4-return-to-a-conversation), and run `"$LATTICE_BIN" -c` to continue the most recent conversation.

To open another conversation, open `~/.lattice/ledgers/` in your file manager, locate its `.ledger` directory by date and copy its name. Replace the example name below with that name and run this from the original project directory:

```bash
"$LATTICE_BIN" --resume "conversation-name.ledger"
```

Lookup is scoped to the current project and includes older conversations without a recorded project. If the name is missing, the error lists several recent conversation names for the current project.

New terminal conversations are stored under `~/.lattice/ledgers/`; older `tui/` and `chat/` locations are also searched. The background service uses `~/.lattice/streams/`. Include referenced documents and attachments in backups. Check `HOME` and individual path settings when locating records; `LATTICE_HOME` does not move all of these locations together.

### Advanced: inspect history without changing it

If history reports an error, back up the complete records and referenced files first; see [upgrade and rollback](installation.md#upgrade-without-losing-the-previous-program). Then choose a read-only check:

- `lattice --recover PATH --offset BYTES --bytes COUNT`: read a selected range without loading model credentials.
- `lattice --verify-ledger DIRECTORY`: fully verify a segmented conversation ledger.

Keep inspection output local, extracting relevant excerpts and removing secrets before sharing. Normal conversation recovery handles incomplete tails and records previously interrupted calls; use the read-only commands above when you need to preserve files exactly during inspection.

`compact`, `tidy` and `--migrate-ledger` organize or migrate data and involve writes, moves or deletion. Before migration, stop sessions using the source file and prepare backups using [Installation and rollback](installation.md). Migration retains the source file; retain attachments stored outside the directory as well.

## A tool or extension behaves unexpectedly

Tools can access files outside the current project directory. After cancellation or interruption, inspect files, processes or the external service's actual result before deciding whether to retry.

Check the programs needed by the tool:

- **Code navigation:** install the language server identified by the tool's message.
- **Browser:** check that Chrome/Chromium is installed.
- **Desktop:** check the external driver and macOS permissions using [desktop setup](desktop.md).
- **Ink client:** check that the background service is running and both ends use the same connection path; see the [client guide](../../clients/ink/README.md).

If you customized the runtime, also check the full assembly and installed extensions. `assembly.json` normally records added extensions; see [assembly configuration (Chinese)](../development/assembly-configuration.zh-CN.md) for the full configuration. Inspect exports before sharing because they may contain keys and prompts.

## Useful support information

Include the program version, system and architecture, installation method, reproduction steps, expected and actual results, and an error excerpt with keys removed. For download-verification failures, include the version or preview-run identity and the failed step. Use the [private reporting route](../../SECURITY.md) for sensitive security issues.
