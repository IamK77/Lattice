# Development reference map

[简体中文](README.zh-CN.md) · [Documentation](../README.md) · [Contributing](../../CONTRIBUTING.md)

Use this route when changing Lattice, implementing a component or maintaining a release. Connecting a model, adding project instructions and using an existing integration belong to the [user/configuration routes](../README.md).

## Which document owns the question?

| Question | Reference and role |
| --- | --- |
| What is this system? | [Architecture overview (Chinese)](architecture-overview.zh-CN.md): the short mental model. |
| Why are its boundaries shaped this way? | [Architecture decisions (Chinese)](architecture-decisions.zh-CN.md): mechanisms and reasons. |
| What must contributors preserve? | [Contributor rules](../../CLAUDE.md), in English; [Contributing](../../CONTRIBUTING.md) and [workflow](../maintenance/workflow.md) describe the contribution path. |
| What data and behavior must components agree on? | [Contract reference](../contracts/README.md): six human-readable notes, corresponding schemas and current implementation locations. |
| How is the chat product assembled? | [Assembly configuration (Chinese)](assembly-configuration.zh-CN.md) and [capability migration](capability-split-migration.md): product baseline, runtime positions and installation additions, not extra kernel concepts. |
| How do I maintain and publish it? | [Maintainer entry](../../MAINTAINERS.md) and [handbook](../maintenance/maintainer-handbook.md): ownership, checks, dependency and release procedures. |

Contract meaning, machine-readable shape, current source and observed test evidence are related but not interchangeable. The [contract index](../contracts/README.md#normative-rules-and-current-implementation) explains the distinction, including the current standard profile catalog. A conflicting implementation is not permission to silently rewrite a contract.

## Current implementation references

- **Components:** [examples](../../examples/), [tool-building walkthrough (Chinese)](tool-building-walkthrough.zh-CN.md) and the [process bridge (Chinese)](../contracts/06-process-bridge.zh-CN.md). External components are not restricted to Rust. Read each example's purpose; test probes and scripted demonstrations are not necessarily ordinary installation recipes or side-effect-free programs.
- **Frontend and integrations:** [setup implementation](setup-development.md), [Ink protocol/tests (Chinese)](../../clients/ink/development.zh-CN.md) and [Desktop backend/tests (Chinese)](desktop-development.zh-CN.md). Their user guides remain separate.
- **Authorization:** [operation and interface permissions (Chinese)](design-action-authorization.zh-CN.md), distinct from installation trust and operating-system isolation.
- **Experts:** [definition, activation and execution snapshots (Chinese)](design-expert-execution.zh-CN.md). This is a current implementation reference, not an unimplemented proposal merely because its filename starts with `design`.

## Probes and acceptance records

Read each record's scope before using it as evidence:

| Record | What it establishes / does not establish |
| --- | --- |
| [Expert snapshot probe (Chinese)](../records/design-expert-snapshot-prototype.zh-CN.md) | A historical test-only architecture experiment. Its then-missing production work is not today's capability list. |
| [Expert management experience (Chinese)](../records/expert-management-prototype.zh-CN.md) | First-version operation and acceptance notes. Issue #45 records human acceptance and delivery on 2026-09-30 (UTC); it does not accept later changes or equivalent Ink forms. |
| [Signed-preview acceptance](../records/signed-preview-acceptance.md) | Verification of specifically named source, runs and signed bytes, with negative cases. It is not approval to publish, a current-candidate status page, or an installation/upgrade/provider acceptance result. |

Keep historical experiments and their counterexamples; label their stage and link their successors rather than rewriting the past as current functionality. Conversely, do not classify every design document as obsolete.

## Release work and verification

Follow [candidate preparation](../maintenance/release-preparation.md), [artifact contents](../maintenance/release-artifacts.md) and [publication](../maintenance/release-publication.md), starting from the maintainer handbook. Users acquiring or replacing a program should use [installation and rollback](../guides/installation.md), not reconstruct the release pipeline.

The publication guide describes mechanisms and prerequisites, not the live value of repository settings. A preview record is evidence only for its exact artifacts; check the current approved source, run and repository conditions for a new operation. Human acceptance, merging, installation and publication remain separate events.

Use the affected tests listed by the relevant implementation and contribution guides. A skipped integration test is not a pass; a scripted model is not provider compatibility; a local regression is not manual UX acceptance. This map adds navigation, not new runtime or protocol guarantees.
