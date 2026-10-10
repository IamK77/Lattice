# Your first conversation with Lattice

[Home](../../README.md) · [Documentation](../README.md) · [简体中文](getting-started.zh-CN.md)

Let Lattice help you understand a project, then work together on a small change. This guide takes you through starting the program, connecting a model, asking a question and returning to the conversation later.

Have a model-service account and API key ready. Your model service charges for usage. Lattice can read files, change code and run commands, including accessing files outside the project directory. Start with a practice project and save your existing work first.

## 1. Build Lattice

**Already have the program?** If you just built it using the landing page, run `export LATTICE_BIN="$PWD/target/release/lattice"` from the Lattice source directory. For an existing installation elsewhere, use `export LATTICE_BIN="/path/to/lattice"` with its actual full path. Then go straight to [step 2](#2-connect-a-model).

For a first build, follow the steps below. Source builds use macOS or Linux with Rust/Cargo and a C toolchain. Linux also needs OpenSSL development libraries and `pkg-config` (`libssl-dev` and `pkg-config` on Debian/Ubuntu). See [Installation and upgrades](installation.md) for other installation options.

Run these commands in a terminal. They use **Bash**; enter `bash` first if you use another shell:

```bash
git clone https://github.com/IamK77/Lattice.git
cd Lattice
cargo build --locked --release --bin lattice
export LATTICE_BIN="$PWD/target/release/lattice"
"$LATTICE_BIN" --version
```

Once you see a version number, keep using this terminal. `LATTICE_BIN` remembers the program's location so you can launch it from your project. Set it again when opening a new terminal. If Lattice is already installed, you can use that executable.

## 2. Connect a model

Replace the path below with your project directory, then start Lattice:

```bash
cd /path/to/your/project
"$LATTICE_BIN"
```

### Guided setup

Lattice opens setup when you first connect a model. Choose **Language / 语言** on its first page to change the setup language. With a usable configuration, it opens the conversation directly.

1. **Connect a service.** Choose your model service, check its address and enter your API key. If the key is already in an environment variable, enter the variable's name instead.
2. **Choose a model.** Select from the service's list or enter the model name yourself. Manual entry also works when the list cannot be fetched.
3. **Review and save.** Check the model and service address. If asked for capacity, use the service's published values; [Model configuration](model-configuration.md#capabilities-and-capacity) explains them. Other options are under **More settings and details**.
4. **Start the conversation.** You can first run a connection test to see whether the service answers. It sends a short question, billed under the service's pricing rules. You can also skip it.

Press Esc to go back, or to exit from the first page. See [Returning and saving](model-configuration.md#language-navigation-and-saving) for more controls.

**Looking after your key:** Prefer an environment-variable reference to reduce copies of the key in configuration files. Tools can read both files and environment variables; keys they read enter conversation records and may be sent to the model service with the conversation. Check for keys before committing code or sharing records.

### Manual configuration (optional)

If you prefer editing a configuration file, see [Model configuration](model-configuration.md#manual-configuration).

<a id="3-start-in-your-project"></a>

## 3. Ask your first question

Enter `/model` and press Enter to view the selected model. Press Esc to close the model panel, then type the question below and press Enter to send it:

> Help me understand this project: what does it do, where is the main code, and how are tests run? For this step, read the files and tell me what you find.

After reading the answer, choose a small task, such as adding a test or explaining an error. Ask Lattice to show the changes and test results so you can review them.

When you are finished for now, enter `/exit` and press Enter. You can return to this conversation later.

## 4. Return to a conversation

In a new terminal, set the program path and return to your project. Replace both example paths below with yours; a source build is at `target/release/lattice` inside the repository from step 1:

```bash
export LATTICE_BIN="/path/to/Lattice/target/release/lattice"
cd /path/to/your/project
"$LATTICE_BIN" -c
```

Lattice continues the most recent conversation. Make the model key available as before. See [Troubleshooting](troubleshooting.md) for help finding other conversations.

If the previous session stopped during an edit or command, ask Lattice to check progress before continuing. Files already written and requests already sent remain in effect; the restored conversation retains the records of those operations.

## 5. Add your project conventions

Put project guidance in `AGENTS.md` or `CLAUDE.md`. If a file already exists, add to its existing contents. For example:

```text
Explain the intended change before editing.
Keep each change focused on one task.
Run the relevant tests and tell me the results.
Discuss new dependencies with me before adding them.
```

At startup, Lattice searches upward from the current directory and reads the nearest directory containing recognized rules files. Start a new session after editing them. Project guidance tells the model how to work; file access is governed by the system and tool configuration.

## Optional tools

After your first conversation, prepare these tools as your tasks call for them:

- **Browser:** install Chrome/Chromium. `LATTICE_BROWSER` selects the browser executable.
- **Code navigation:** install your language's language server so Lattice can find definitions and references. The tool reports what is missing when needed.
- **Desktop:** on macOS, follow [desktop setup](desktop.md) to install the driver and grant system permissions.
- **Alternative client:** try [JavaScript/Ink](../../clients/ink/README.md).

## Data and permissions

These details help you choose which work to share with Lattice:

- **Conversation material.** Conversations, tool operations and attachments are stored locally and may be sent to your chosen model service. Browser and desktop screenshots are also retained and sent to the model.
- **Stopping work.** Existing changes and external sends remain in effect after cancellation. Programs started by a tool that run independently may continue; check files and running programs before retrying.
- **Installing extensions.** Choose trusted sources. Approving an extension's installation and approving one of its actions are separate choices. An installation grant for a URL or path remains valid after its contents change. Permanent grants default to `~/.lattice/trust.json`; edit that record to revoke them. See the [grant file format](../../schemas/trust_grants.json) for its fields.

## If setup does not work

Start with the matching symptom in [Troubleshooting](troubleshooting.md), such as a failed build, an unreachable model or a missing conversation. When asking for help, share the steps and an error message with keys removed, while keeping your original local configuration and records. [Support](../../SUPPORT.md) explains how to report problems and privately report security issues.

Return to [Documentation](../README.md) to choose your next task.
