# Your first conversation with Lattice

[Home](../README.md) · [Documentation](README.md) · [简体中文](getting-started.zh-CN.md)

This guide is for using the included terminal agent in your own project. You supply a model account and credentials; the provider may charge for usage. Lattice can read and change files, run programs, and send material to your selected provider. **It is not a sandbox.** Start in a project whose access you are comfortable granting.

## 1. Build Lattice

Check [Installation and upgrades](installation.md) for available installation sources, tested platforms, download verification, and rollback. For a source build, use macOS or Linux with Rust/Cargo and a C toolchain. Linux also needs OpenSSL development libraries and `pkg-config` (`libssl-dev` and `pkg-config` on Debian/Ubuntu). This guide does not provide a supported Windows path.

The commands below use **Bash**. Enter `bash` if you use another shell, and keep this terminal open through setup:

```bash
git clone https://github.com/IamK77/Lattice.git
cd Lattice
cargo build --locked --release --bin lattice
export LATTICE_BIN="$PWD/target/release/lattice"
"$LATTICE_BIN" --version
```

`LATTICE_BIN` is this guide's convenience variable, not a product setting. Its absolute path lets you launch after changing directories. In a new terminal, set it again or use the executable's full path. If you already installed Lattice, use your verified executable instead of building again.

## 2. Connect a model

Move into your project and start Lattice. Replace the example path:

```bash
cd /path/to/your/project
"$LATTICE_BIN"
```

### Guided setup

When you need to connect a model, setup runs before the conversation. An existing configuration that passes local checks skips it; skipping setup does not mean the account or connection has been verified. Use an interactive terminal at least 40 columns wide and 14 rows high.

1. **Connect a service.** Choose a provider or a custom service, check the address and API format, and supply a key or the name of an environment variable containing it. You can also repair an existing entry.
2. **Choose a model.** Fetch and filter the service's list, or enter its exact model name. An unavailable list does not prevent manual entry.
3. **Review and save.** Check the model, service address and capacity. Use **More settings and details** for other settings. If a capacity is unknown, consult the provider rather than guessing. Setup's **Language / 语言** option switches English/Simplified Chinese; it does not change the main interface's language.
4. **Start the conversation.** You can skip the optional connection test. Running it sends a short request that **may cost money**; it is not retried automatically. A successful test does not prove that every tool or model feature works.

Esc goes back, or exits from the first page; Ctrl-C exits while waiting for input. During a network request, cancellation is handled after that bounded request finishes. Cancelling before you save the model creates neither a model entry nor a conversation; settings already explicitly saved, including language, remain saved.

**Credentials:** A key saved in a local configuration file is readable by the agent. If a tool reads it, it may enter saved history and model input. An environment-variable reference avoids putting the literal key in that file, but does not hide the environment from tools. Do not commit real keys.

### Manual configuration (optional)

For a hand-written model file, custom endpoint, capacity or language settings, use [Model configuration](model-configuration.md). You do not need that reference to complete the guided path.

<a id="3-start-in-your-project"></a>

## 3. Ask your first question

With Lattice open in your project, use `/model` to inspect the selected model before sharing sensitive work. Start by asking for understanding rather than a change:

> Explain how this repository is organized. Identify the main entry points and relevant tests. Do not edit files or run project commands yet.

Then choose a small, reviewable task. Inspect the diff and test results yourself. Instructions guide the agent; they do not enforce operating-system isolation.

## 4. Return to a conversation

From your project directory, with the model credentials still available, run:

```bash
"$LATTICE_BIN" -c
```

This continues the most recent conversation visible to that project. Older records without a project identity may also appear. Recovery does not repeat historical tool actions, but an interrupted action may already have partly taken effect. Check the recorded result and actual files or processes before retrying. **Cancellation is not rollback.**

## 5. Add your project conventions

This is optional. Put project-specific guidance in `AGENTS.md` or `CLAUDE.md`; edit existing files rather than overwriting them. For example:

```text
Explain the intended change before editing.
Keep changes focused on the task.
Run the affected tests and report their results.
Do not add dependencies without discussing the trade-off.
```

Lattice searches upward from the working directory for the nearest directory with recognized rules files. Rules are read at startup, not continuously refreshed; start a new session after changing them. They guide behavior, not permissions, and cannot guarantee compliance.

## Optional tools

These are not prerequisites for your first conversation:

- **Browser:** requires Chrome/Chromium; `LATTICE_BROWSER` selects another executable.
- **Code navigation:** requires a local language server for that language; missing servers are reported, not automatically installed.
- **Desktop:** requires macOS, an external driver and system permissions. See [desktop setup (Chinese)](desktop.zh-CN.md).
- **Alternative client:** see [JavaScript/Ink](../clients/ink/README.md) (mixed English/Chinese) for its own startup and controls.

## Data and permissions

- **The working directory is not a boundary.** Tools can read and change files, execute programs and access the network.
- **Local storage does not mean local-only processing.** Conversations, tool requests/results and attachments are saved and may be sent to the selected provider. Browser and desktop screenshots are retained and sent to the model when used.
- **Do not rely on automatic secret removal.** Secrets read by tools, including keys obtained during a session, can enter persistent history and model input.
- **Only use extensions you trust.** Permission prompts are not a sandbox. Contents at an approved URL or path can change without a new prompt. Permanent installation trust is separate from permissions for individual actions; its revocation currently requires managing the grants file manually.
- **Check before sharing or retrying.** Local history and configuration under `~/.lattice/` may contain secrets. Cancellation neither undoes changes nor guarantees that every descendant process stopped. Review external sending, credential entry and system permissions carefully.

## If setup does not work

Use [Troubleshooting](troubleshooting.md) for build errors, unavailable models, rejected requests, missing conversations or tools. Do not delete your data to reset the application, paste keys, dump the environment, or upload an entire conversation to ask for help. [Support](../SUPPORT.md) explains what to include in a report and where to report security issues privately.

For the next task, choose [using, customizing or developing Lattice](README.md). You do not need the architecture or component contracts to use the terminal agent.
