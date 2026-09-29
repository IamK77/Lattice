# Lattice

**An AI agent you can shape around the way you work.**

Work with code and files, run commands, and extend your agent with new tools—from your terminal. Choose a supported model provider, return to saved conversations, and inspect the record of what happened.

**English** · [简体中文](README.zh-CN.md)

[Get started](docs/getting-started.md) · [Make it yours](#make-it-yours) · [Contribute](CONTRIBUTING.md)

## Your work, your setup

**Choose your model.** Connect your own provider and switch between configured models with `/model`. You bring the account and API key; Lattice provides the workspace.

**Add capabilities when you need them.** Use the included file, search, and command tools, then add tools for your workflow. Optional browser, desktop, and code-navigation tools have additional requirements.

**Keep the work, not just the answer.** Return to saved conversations and review recorded requests and results. Resuming a conversation does not rerun its historical tool actions.

## Start with a real task

Here are a few things to ask after setup—not recorded results or promises about what every model will do:

| What you want to do | A starting prompt |
| --- | --- |
| Understand an unfamiliar project | “Explain how this repository is organized. Find the main entry points and relevant tests. Don't change any files yet.” |
| Make a focused change | “Investigate this failing test, propose a small fix, and run the affected tests. Tell me what you verified and what you didn't.” |
| Turn a repeated task into a tool | “Help me design a tool for this workflow. Explain what it will read, change, and run before installing anything.” |

You review the changes and results. Model output is not a substitute for verification.

## Get started

**The first official release is not published yet.** Start with the [installation and upgrade guide](docs/installation.md). Native packaging is tested on Ubuntu 24.04 x86-64 and macOS 15 Apple Silicon; older systems, Intel Mac packages, and Windows are not implied. A source build needs Rust/Cargo, a C toolchain, and, on Linux, OpenSSL development libraries and `pkg-config`. Real conversations require your own model account.

```sh
git clone https://github.com/IamK77/Lattice.git
cd Lattice
cargo build --locked --release --bin lattice
```

**Next: [connect a model and start your first conversation](docs/getting-started.md).** The guide covers credentials, launching in your project, and returning to a conversation. Building the program does not configure a model account.

## Make it yours

Start with the included setup. Customize one thing at a time:

- **Your model:** configure supported Chat Completions, Responses, or Anthropic Messages endpoints. Compatibility does not imply support for every provider extension. See the [model setup guide](docs/getting-started.md#2-connect-a-model).
- **Your project:** add conventions and testing instructions to your project's `AGENTS.md` or `CLAUDE.md`. Lattice reads project rules at startup; start a new session after changing them. See [project customization](docs/getting-started.md#5-add-your-project-conventions).
- **Your tools:** extend the agent with components rather than changing the entire application. External components are not restricted to Rust. Browse the [examples](examples/) and the [tool-building walkthrough (Chinese)](docs/演示-agent自造工具.md).

You do not need to understand the internal event protocol to use the terminal agent.

## Know what you are giving it access to

Lattice's tools can read and change files, run programs, and access the network. **It is not a sandbox.** Use a workspace and permissions you are comfortable granting, and review changes before relying on them.

Conversations, tool requests, results, and attachments are stored locally and may be sent to your selected model provider. Secrets read by tools are not guaranteed to be redacted. Browser and desktop screenshots may also be retained and sent to the model. Cancellation does not undo an action that has already happened.

Read [data, permissions, and recovery limits](docs/getting-started.md#data-and-permissions) before using sensitive files or accounts.

## Explore further

- [Getting started](docs/getting-started.md) — from building to your first conversation.
- [Contributing](CONTRIBUTING.md) — changes, checks, and reporting problems.
- [JavaScript terminal client (Chinese)](clients/ink/README.md) — an alternative client and daemon connection.
- [Architecture overview (Chinese)](docs/架构总览.md) and [design decisions (Chinese)](docs/架构决策.md) — for developers extending the runtime.
- [Contracts (Chinese)](docs/contracts/) and [JSON schemas](schemas/) — component and event interfaces.
- [Desktop setup (Chinese)](docs/桌面操作.md) — the optional macOS desktop integration.

The English entry path is available here; deeper architecture and integration documentation is currently in Chinese.

## License

Licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for project notices and [assets](assets/README.md) for third-party asset attribution. Dependencies and optional external components retain their own licenses.

For help, see [Support](SUPPORT.md). Report vulnerabilities through the private channel in [Security](SECURITY.md). Participation follows the [Code of Conduct](CODE_OF_CONDUCT.md); project stewardship is documented in [Maintainers](MAINTAINERS.md).
