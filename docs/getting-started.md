# Your first conversation with Lattice

[Home](../README.md) · [简体中文](getting-started.zh-CN.md)

This guide takes you from source code to a real conversation in your own project. You supply a model account and API key; model usage may be billed by your provider. No account or usable model configuration ships with the binary.

## 1. Build Lattice

For release status, tested platforms, artifact verification, installation, and rollback, start with [Installation and upgrades](installation.md). The source path below remains available before the first official release.

Use macOS or Linux with Rust/Cargo and a C compiler toolchain. On Linux, install your distribution's OpenSSL development libraries and `pkg-config` first; Debian/Ubuntu package names are `libssl-dev` and `pkg-config`. This guide does not provide a supported Windows path.

The shell commands below use **Bash**. If you use another shell, enter `bash` first. Keep the same terminal open through setup so that exported variables remain available.

```bash
git clone https://github.com/IamK77/Lattice.git
cd Lattice
cargo build --locked --release --bin lattice
export LATTICE_BIN="$PWD/target/release/lattice"
"$LATTICE_BIN" --version
```

`LATTICE_BIN` is a convenience variable for this guide, not a Lattice configuration setting. Its absolute path lets you launch the built program after moving to your own project. Set it again in a new terminal, or use the executable's full path.

## 2. Connect a model

### Guided setup

Move into your project and run `"$LATTICE_BIN"` in an interactive terminal. If its model configuration is incomplete, Lattice opens a **page-based guide before creating a conversation or opening the main TUI**:

1. **Connect a service.** Choose a provider (OpenAI, Anthropic, or DeepSeek), custom configuration, or an existing entry to repair. Provider presets fill in the recommended API format and address; the connection page shows both and offers editing before you enter a key. Custom services ask for format and address. Supply a masked local key or name an existing nonempty environment variable.
2. **Choose a model.** Explicitly fetch the service's model list and type to filter it, or enter an exact identifier manually. Previously fetched names are available as input suggestions with Tab completion, without another request. Failed or empty discovery never blocks manual entry. Discovery sends a GET, not a generation request, and does not establish format/tool compatibility.
3. **Review once.** A compact summary shows the model, readable API name (for example, OpenAI Chat Completions), endpoint, suggested local name, default-preference action, and essential capabilities. **More settings and details** contains the full configuration, save location and capability sources, as well as name, default and language settings. Complete capability metadata goes straight to review; only missing or invalid limits require input. Edit an individual section and return here. Leaving the default preference unchanged does not unset a previously selected default model.
4. **Save and begin.** Skip the optional connection test and enter the TUI, or explicitly send a short, potentially billable request. A failed test can be retried explicitly, corrected, skipped, or followed by exit; it is never retried automatically.

**Pages and terminal:** Each current task replaces the preceding page; retries replace the error rather than append a transcript. Long explanations have their own detail pages. Setup uses a temporary terminal screen, preserving the pre-launch screen on cancellation, ordinary errors, and handoff to the main TUI. All three standard streams must be interactive, with at least 40 columns and 14 rows. Layout is recalculated between questions and detail pages; resizing while a widget is waiting does not immediately redraw the whole page.

**Capability editing:** Space toggles preselected checkboxes and Enter confirms. Image input is separate from hosted image generation; hosted search and image generation are offered only for Responses. Unknown capabilities stay unchecked. Thinking-rung checkboxes declare supported values, not the current thinking strength: standard names are ordered weakest first; provider-specific names retain their declared order. Advanced settings contain custom rung names/ordering and usage-field mappings.

**Input assistance:** Invalid input stays in its prompt for correction. Very long names remain searchable in the fetched model list; inline completion omits names that would wrap rather than truncate the identifier being submitted. Token limits accept exact integers or decimal suffixes, case-insensitively: `1M = 1000k = 1000000`, `1.5M = 1500000`. The submitted answer displays the expanded count. These are not binary units: enter `1048576` if that is the service's exact limit. Fractional tokens and overflow are rejected without rounding; output must leave room for input.

**Language:** Setup and credential repair support English and Simplified Chinese, independently of the main TUI. The initial language follows `LC_ALL`, then `LC_MESSAGES`, then `LANG`; Chinese locales use Simplified Chinese, others use English. Use **Language / 语言** at home or under the new-model review's **More settings and details** to switch without losing the draft; credential repair offers it directly on review. An explicit choice is saved as `setupLanguage` (`en` or `zh-CN`) in preferences and takes precedence over locale detection; automatic detection writes nothing. If saving fails, the selected language still applies to this run and the guide reports the failure. Service diagnostics remain identified as original text.

An input ceiling is not necessarily a total context window. Anthropic's reported input ceiling is shown as a **conservative total budget suggestion**, without adding output tokens; confirm or edit it. Remote metadata never overwrites edits to the same selected model while navigating the draft. Provider format presets are based on documentation, not proof that every model or feature works through that format.

Esc returns to the previous step (and exits from home); Ctrl-C exits while waiting for input. During a network request, cancellation keys are handled after that bounded request finishes; cancellation does not resend it. Completed fields remain in the draft; text cancelled before submission is not committed. Cancelling before model save creates neither a model entry nor a conversation; an explicitly saved language preference and any already-saved configuration remain available. Returning home retains new-model and credential-repair drafts. Changing endpoint, format, or account invalidates dependent lists/capabilities; changing models resets capabilities. A changed endpoint never inherits its old credential. A usable existing configuration skips the guide. Background/noninteractive commands do not prompt. Your requested resumed conversation is retained throughout setup.

A local key is stored in an **agent-readable file**: file-tool reads can put it into persistent history and model input. On Unix the writer creates its temporary file owner-only before writing credentials. This is not a promise that the agent cannot read the key. Environment references remain available; setup does not edit shell startup files. Unsupported protected storage on other platforms is not presented as safe local-key storage.

Model discovery and connection tests are frontend operations, not chat messages. Each explicit attempt has a separate record under `setup-tests/` beside the catalog, containing request intent and outcome but no key, request body or provider response body. Discovery follows bounded Anthropic pagination and limits response sizes, entry counts, and duration; redirects and automatic retries are disabled. A record without a completed outcome means the result is unknown; it is not replayed. A successful short text probe does not validate tools, reasoning history or every provider feature. The bundled DeepSeek template is based on the official references recorded in `src/bin/lattice/setup/deepseek.json`; real-account acceptance is separate.

Invalid catalogs are never reset. Repair the indicated file and choose to check again. If the model saves but the default preference does not, the guide says so and offers continuing for this launch. Launch environment overrides still take precedence on later launches.

### Manual configuration (optional)

The default model catalog is `~/.lattice/models.json`. Create its parent directory if necessary:

```bash
mkdir -p "$HOME/.lattice"
```

Create or edit `models.json` in your editor. **Preserve existing entries if the file already exists.** You can instead use another catalog file by setting `LATTICE_MODELS` to its path.

This example describes the configuration shape, not a working provider account. Replace the endpoint, model ID, and model limits with values documented by your provider. The `.invalid` endpoint deliberately cannot be used as a real service.

```json
{
  "models": {
    "my-model": {
      "adapter": "openai",
      "model": "your-model-id",
      "baseUrl": "https://api.example.invalid/v1",
      "apiKeyEnv": "LATTICE_MODEL_KEY",
      "profile": {
        "contextWindow": 32768,
        "maxOutputTokens": 4096
      }
    }
  }
}
```

Choose the adapter that matches your endpoint:

| Adapter | Endpoint protocol |
| --- | --- |
| `openai` | Chat Completions |
| `responses` | Responses |
| `anthropic` | Anthropic Messages |

These are protocol choices, not a promise that every provider extension works. Obtain the exact endpoint root, model ID, limits, and any required profile settings from your provider. Configuration references: [model catalog](../schemas/model_catalog.json) and [model profile](../schemas/model_profile.json).

Supply the key through your existing credential-management setup, or enter it without placing its value in shell history:

```bash
printf 'API key: '
read -r -s LATTICE_MODEL_KEY
printf '\n'
export LATTICE_MODEL_KEY
```

This keeps the literal out of the command text; it does **not** make the environment or tool output secret from the agent. Never commit real keys. Prefer referencing an environment variable with `apiKeyEnv` over storing the value in the catalog.

## 3. Start in your project

Move to a project directory whose files you are comfortable giving the agent access to. Replace the example path before running:

```bash
cd /path/to/your/project
"$LATTICE_BIN"
```

Use `/model` to inspect the selected model and switch between configured entries. Saved preferences and existing startup environment variables may affect the initial selection.

For a first request, ask for understanding rather than a change:

> Explain how this repository is organized. Identify the main entry points and relevant tests. Do not edit files or run project commands yet.

Then ask for a small, reviewable task. Inspect the diff and test results yourself. Instructions guide the agent; they are not an operating-system permission boundary.

## 4. Return to a conversation

From a terminal with the required model credentials available, use:

```bash
"$LATTICE_BIN" -c
```

This continues the most recent conversation visible to the current project directory. Legacy records without a project identity remain visible across projects. Named resume uses the same visibility rule. It does not rerun historical tool actions. An interrupted action may already have partly taken effect: check the recorded result and the actual files or processes before retrying. Cancellation is not rollback.

Use `"$LATTICE_BIN" --help` for other entry points and environment settings. Record files can contain sensitive data; inspect them before sharing a debugging report.

## 5. Add your project conventions

Put project-specific guidance in `AGENTS.md` or `CLAUDE.md`. Edit an existing file rather than overwriting it. For example:

```text
Explain the intended change before editing.
Keep changes focused on the task.
Run the affected tests and report their results.
Do not add dependencies without discussing the trade-off.
```

Lattice searches from the working directory upward for the nearest directory containing recognized rules files. Rules are loaded at startup, not continuously refreshed. Start a new session after editing them. They guide behavior; they do not sandbox tools or guarantee compliance.

To go further, browse the [component examples](../examples/). External components can be written in languages other than Rust. The [tool-building walkthrough](tool-building-walkthrough.zh-CN.md) and [assembly guide](assembly-configuration.zh-CN.md) are currently in Chinese. A complete assembly configuration and an installation overlay are different files; do not substitute one for the other.

## Optional tools

- **Browser:** requires Chrome/Chromium. `LATTICE_BROWSER` can select the executable.
- **Code navigation:** requires a local language server for the language being inspected. Missing servers are reported, not automatically installed.
- **Desktop:** the bundled adapter targets macOS and requires an external driver and system permissions. See [desktop setup (Chinese)](desktop.zh-CN.md).
- **Alternative frontend:** the [JavaScript/Ink client (Chinese)](../clients/ink/README.md) connects to the Unix socket daemon started with `"$LATTICE_BIN" serve`.

## Data and permissions

- **Not a sandbox.** Tools can read and modify files, execute programs, and use the network. The initial working directory is not a containment boundary.
- **Local storage is not local-only processing.** Conversations, tool requests and results, and attachments are persisted and may become input to your selected model provider. Browser and desktop screenshots are retained and sent to the model when used.
- **Do not rely on automatic redaction.** Tool requests and results are not redacted by the chat redactor. Secrets read by tools, and keys obtained during a session, may enter persistent history and model input.
- **Extensions need your trust.** Tool effects are self-declared. The trust gate guards against mistakes, not malicious implementations; authorization for a URL or local path does not pin its future contents. Revocation currently requires manually managing the grants file.
- **Review side effects.** External sending, credentials, and system permissions need your attention. Cancellation does not undo changes or guarantee that every descendant process has stopped.
- **Check before sharing.** History, model catalogs, installation overlays, and trust records normally live under `~/.lattice/`. Review these files before sharing or backing them up to another service.

See [design decisions (Chinese)](architecture-decisions.zh-CN.md) for the detailed boundaries.

## If setup does not work

- A Linux build cannot find OpenSSL: check that development headers and `pkg-config` are installed, not only the runtime library.
- No usable model is available: check the catalog path, adapter, and exported variable name. Do not paste the key into an issue.
- The provider rejects a request: check its endpoint root, model ID, limits, and supported settings. To omit the thinking parameter, launch with `LATTICE_THINKING= "$LATTICE_BIN"`. An unset variable uses the saved preference or `high`; `off` sends an explicit disabled setting rather than omitting the parameter.
- An optional tool is unavailable: check its requirements above; a successful core build does not install them.

For a **scripted UI check without model access**, launch `LATTICE_SCRIPTED=1 "$LATTICE_BIN"`. This is a deterministic test mode, not a real model demonstration. The model-free component example is `cargo run --example heartbeat`, run from the Lattice source directory.
