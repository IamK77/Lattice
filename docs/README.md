# Lattice documentation

[Product home](../README.md) · [简体中文](README.zh-CN.md)

Choose the task you are doing. Using Lattice on a software project does not require learning how its runtime is implemented.

## Use Lattice

| I want to… | Start here |
| --- | --- |
| Obtain a program, verify a download, upgrade or roll back | [Installation and upgrades](installation.md) |
| Connect my account and ask a first question | [First conversation](getting-started.md) |
| Continue earlier work | [Resume a conversation](getting-started.md#4-return-to-a-conversation) |
| Understand what files and data I am giving it access to | [Data and permissions](getting-started.md#data-and-permissions) |
| Solve a problem or ask for help | [Troubleshooting](troubleshooting.md) · [Support](../SUPPORT.md) |
| Report a vulnerability without exposing secrets | [Private security reporting](../SECURITY.md) |

## Configure and customize

These are supported product settings and optional integrations, not instructions to modify the kernel.

- [Model configuration](model-configuration.md): service connections, manual model files, capabilities, capacity, language and credentials.
- [Project conventions](getting-started.md#5-add-your-project-conventions): guide the agent with your project's rules.
- [Optional tools](getting-started.md#optional-tools): browser and code-navigation prerequisites.
- [Desktop setup](desktop.md): optional macOS driver and permissions.
- [JavaScript/Ink client](../clients/ink/README.md): alternative client startup and controls.

Using an existing integration and writing a new component are different tasks. For the latter, use the development route below. Costs, data exposure and destructive-action warnings remain with the relevant operation; a technical reference is not a substitute for those warnings.

## Develop and maintain

The [development reference map](development.md) explains document ownership and distinguishes current implementation, historical probes and acceptance records.

| Task | Reference |
| --- | --- |
| Understand the runtime and its design boundaries | [Overview (Chinese)](architecture-overview.zh-CN.md) · [Decisions (Chinese)](architecture-decisions.zh-CN.md) |
| Write a component in your language | [Examples](../examples/) · [Tool-building walkthrough (Chinese)](tool-building-walkthrough.zh-CN.md) |
| Assemble a runtime or migrate a custom assembly | [Assembly configuration (Chinese)](assembly-configuration.zh-CN.md) · [Capability migration](capability-split-migration.md) |
| Implement the component interfaces | [Contract reference](contracts/README.md) · [JSON schemas](../schemas/) |
| Work on the first-run frontend | [Setup implementation and verification](setup-development.md) |
| Implement or test an optional integration | [Ink protocol and tests (Chinese)](../clients/ink/development.zh-CN.md) · [Desktop backend and tests (Chinese)](desktop-development.zh-CN.md) |
| Contribute a focused change | [Contributing](../CONTRIBUTING.md) · [Branch and PR workflow](workflow.md) |
| Maintain dependencies, prepare or publish a release | [Maintainers](../MAINTAINERS.md) · [Maintainer handbook](maintainer-handbook.md) |

A complete runtime assembly is not the same file as an installation overlay; the assembly reference explains their separate roles. Neither is needed to configure an ordinary model account.

For a model-free component demonstration, run from the source checkout:

```bash
cargo run --locked --example heartbeat
```

This uses scripted replies to demonstrate the event flow; it is not a real model conversation or evidence of provider compatibility. For scripted terminal checks and their limits, see [setup development notes](setup-development.md#developer-checks-without-a-provider).

Deep design references may remain Chinese and are labeled above. Filenames use English basenames; Chinese documentation uses `.zh-CN.md`. The [path migration map](path-migrations.json) records older names. Follow these current entry points rather than assuming a prototype or an old acceptance record describes the current product.
