# Lattice documentation

[Product home](../README.md) · [简体中文](README.zh-CN.md)

Choose the task you are doing. Using Lattice on a software project does not require learning how its runtime is implemented.

## Use Lattice

| I want to… | Start here |
| --- | --- |
| Obtain a program, verify a download, upgrade or roll back | [Installation and upgrades](guides/installation.md) |
| Connect my account and ask a first question | [First conversation](guides/getting-started.md) |
| Continue earlier work | [Resume a conversation](guides/getting-started.md#4-return-to-a-conversation) |
| Understand what files and data I am giving it access to | [Data and permissions](guides/getting-started.md#data-and-permissions) |
| Solve a problem or ask for help | [Troubleshooting](guides/troubleshooting.md) · [Support](../SUPPORT.md) |
| Report a vulnerability without exposing secrets | [Private security reporting](../SECURITY.md) |

## Configure and customize

Choose what you want to adjust: connect a model, add project guidance, or configure optional tools such as the browser and desktop.

- [Model configuration](guides/model-configuration.md): service connections, manual model files, capabilities, capacity, language and credentials.
- [Project conventions](guides/getting-started.md#5-add-your-project-conventions): guide the agent with your project's rules.
- [Optional tools](guides/getting-started.md#optional-tools): browser and code-navigation prerequisites.
- [Desktop setup](guides/desktop.md): optional macOS driver and permissions.
- [JavaScript/Ink client](../clients/ink/README.md): alternative client startup and controls.

To write your own tools or change how Lattice runs, use Develop and maintain below.

## Develop and maintain

The [development reference map](development/README.md) explains document ownership and distinguishes current implementation, historical probes and acceptance records.

| Task | Reference |
| --- | --- |
| Understand the runtime and its design boundaries | [Overview (Chinese)](development/architecture-overview.zh-CN.md) · [Decisions (Chinese)](development/architecture-decisions.zh-CN.md) |
| Write a component in your language | [Examples](../examples/) · [Tool-building walkthrough (Chinese)](development/tool-building-walkthrough.zh-CN.md) |
| Assemble a runtime or migrate a custom assembly | [Assembly configuration (Chinese)](development/assembly-configuration.zh-CN.md) · [Capability migration](development/capability-split-migration.md) |
| Implement the component interfaces | [Contract reference](contracts/README.md) · [JSON schemas](../schemas/) |
| Work on the first-run frontend | [Setup implementation and verification](development/setup-development.md) |
| Implement or test an optional integration | [Ink protocol and tests (Chinese)](../clients/ink/development.zh-CN.md) · [Desktop backend and tests (Chinese)](development/desktop-development.zh-CN.md) |
| Contribute a focused change | [Contributing](../CONTRIBUTING.md) · [Branch and PR workflow](maintenance/workflow.md) |
| Maintain dependencies, prepare or publish a release | [Maintainers](../MAINTAINERS.md) · [Maintainer handbook](maintenance/maintainer-handbook.md) |

A complete runtime assembly is not the same file as an installation overlay; the assembly reference explains their separate roles. Neither is needed to configure an ordinary model account.

For a model-free component demonstration, run from the source checkout:

```bash
cargo run --locked --example heartbeat
```

This example uses preset replies to show events passing between components. Connect a real model with [First conversation](guides/getting-started.md); see [setup development notes](development/setup-development.md#developer-checks-without-a-provider) for developer checks.

Browse the directories: [use and configuration](guides/README.md), [development](development/README.md), [contracts](contracts/README.md), [maintenance](maintenance/README.md), and [historical records](records/README.md). Ink documentation stays beside its client.

Deep design documents are mainly Chinese, with their language identified in the English navigation. Filenames use English basenames and `.zh-CN.md` for Chinese documents. The [path migration map](path-migrations.json) records old names and their current destinations; the two earlier first-use entry pages retain chapter links to the moved guides.
