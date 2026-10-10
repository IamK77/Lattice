# Lattice

**An event-sourced, composable AI agent runtime for the terminal.**

### Record first. Deliver second.

Use it as a coding agent today. Reshape it around your workflow tomorrow. Models, tools, policies, context management—even the agent loop—are replaceable components.

Events enter an append-only causal history before they reach the next component. That history powers inspection, context, and recovery without rerunning historical tool actions.

**English** · [简体中文](README.zh-CN.md)

[Get started](docs/getting-started.md) · [Make it yours](#make-it-yours) · [Documentation](docs/README.md) · [Contribute](CONTRIBUTING.md)

## Why Lattice

- **Change the parts, keep your workflow.** Swap models, tools, policies, and loops through an explicit assembly. Start with the included terminal agent and take control of how it works.
- **History is the source of truth.** Resume from recorded events. Recovery reconstructs state instead of repeating historical tool actions.
- **Follow causes, not just timestamps.** Trace requests, results, and decisions through their causal links. Inspect how an answer came together and where an operation stopped.
- **Extend it in your language.** External components speak JSON over a line-oriented protocol. Build tools in Python, JavaScript, Rust, or another language that fits your work.

## The runtime in ten seconds

```text
                    Kernel
Component A ──→ Record event ──→ Deliver ──→ Component B
                     │
                     ▼
               Causal ledger
```

**The ledger is on the delivery path, not an afterthought.** The kernel records events and routes them along the assembly's connections. The model, loop, context manager, policies, tools, and interface live in replaceable components around it.

Explore the [architecture (Chinese)](docs/architecture-overview.zh-CN.md), inspect the [JSON contracts](schemas/), or start with the agent below.

## Get started

Build from source, connect your model, and launch in your project.

**Build requirements:** Rust/Cargo and a C toolchain; on Linux, also OpenSSL development libraries and `pkg-config`. Native packaging is tested on Ubuntu 24.04 x86-64 and macOS 15 Apple Silicon. See the [installation and upgrade guide](docs/installation.md) for platform setup and release status.

```sh
git clone https://github.com/IamK77/Lattice.git
cd Lattice
cargo build --locked --release --bin lattice
```

**Next: [connect your model account and start your first conversation](docs/getting-started.md).** The guide takes you through credentials, launching in your project, and returning to saved work.

## Put it to work

Start a conversation with a concrete task:

| What you want to do | A starting prompt |
| --- | --- |
| Understand an unfamiliar project | “Explain how this repository is organized. Find the main entry points and relevant tests.” |
| Make a focused change | “Investigate this failing test, make a small fix, and run the affected tests. Show me the changes and results.” |
| Build a tool for your workflow | “Help me turn this repeated task into a tool. Design it, test it, and walk me through installation.” |

Use `/model` to switch configured models. When you return, [continue your previous conversation](docs/getting-started.md#4-return-to-a-conversation).

## Make it yours

Start with the included setup. Customize one thing at a time.

### Configure existing capabilities

- **Your model:** connect Chat Completions, Responses, or Anthropic Messages endpoints and switch configured models with `/model`. See [model configuration](docs/model-configuration.md).
- **Your project:** add conventions and testing instructions to your project's `AGENTS.md` or `CLAUDE.md`. Lattice reads project rules at startup; start a new session after changing them. See [project customization](docs/getting-started.md#5-add-your-project-conventions).
- **Optional integrations:** check the [tools and client guides](docs/README.md#configure-and-customize) for their prerequisites and permissions.

### Develop components and assemblies

These are development tasks, not prerequisites for using the agent.

- **Your tools:** extend the agent with components rather than changing the entire application. External components are not restricted to Rust. Browse the [examples](examples/) and the [tool-building walkthrough (Chinese)](docs/tool-building-walkthrough.zh-CN.md).

- **Your runtime:** export the assembly with `lattice assembly`, then select your own with `LATTICE_ASSEMBLY`. Replace the components and connections that shape the agent's behavior. See [assembly configuration (Chinese)](docs/assembly-configuration.zh-CN.md).

<a id="know-what-you-are-giving-it-access-to"></a>

## File operations and data storage

Lattice's tools can read and change files, run programs and access the network, including files outside the current project directory. Save existing work before working on important files and review the changes afterward. Changes already made remain in place after cancellation.

Conversations, tool operations and attachments are stored locally and may be sent to your chosen model service. Keys and other secrets read by tools remain in the records and may travel with the conversation; browser and desktop screenshots are also retained and sent. Check records for sensitive content before sharing them.

See [Data and permissions](docs/getting-started.md#data-and-permissions) for more details.

## Explore further

The [documentation index](docs/README.md) has three task-based routes:

- [Use Lattice](docs/README.md#use-lattice) — install, start, resume, understand access and find help.
- [Configure and customize](docs/README.md#configure-and-customize) — model settings, project rules and existing integrations.
- [Develop and maintain](docs/README.md#develop-and-maintain) — components, assemblies, architecture, contracts, contribution and releases.

The English entry path is available here; deeper architecture and integration documentation is currently in Chinese.

## License

Licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for project notices and [assets](assets/README.md) for third-party asset attribution. Dependencies and optional external components retain their own licenses.

For help, see [Support](SUPPORT.md). Report vulnerabilities through the private channel in [Security](SECURITY.md). Participation follows the [Code of Conduct](CODE_OF_CONDUCT.md); project stewardship is documented in [Maintainers](MAINTAINERS.md).
