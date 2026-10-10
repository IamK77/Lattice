# Contract reference

[简体中文](README.zh-CN.md) · [Development references](../development.md) · [User documentation](../README.md)

These contracts describe the data exchanged between components, not how to configure an ordinary model account. The six detailed notes are currently in Chinese. External components may use any implementation language.

## Choose the boundary you are implementing

| Task | Read | Machine-readable shape / implementation location |
| --- | --- | --- |
| Read or emit an event | [1. Event envelope](01-envelope.zh-CN.md) | [envelope.json](../../schemas/envelope.json) · [event.rs](../../src/contracts/event.rs) |
| Handle a core request, result or control event | [2. Core event types](02-core-events.zh-CN.md) | [Payload schemas](../../schemas/payloads/) · [core_events.rs](../../src/contracts/core_events.rs) |
| Describe a component, ports and tools | [3. Component manifest](03-component-manifest.zh-CN.md) | [component_manifest.json](../../schemas/component_manifest.json) · [component.rs](../../src/contracts/component.rs) |
| Describe instances and explicit connections | [4. Assembly manifest](04-assembly-manifest.zh-CN.md) | [assembly_manifest.json](../../schemas/assembly_manifest.json) · [assembly.rs](../../src/contracts/assembly.rs) |
| Implement a replaceable role | [5. Standard port profiles](05-standard-interfaces.zh-CN.md) | Current standard profile catalog: [profile.rs](../../src/contracts/profile.rs); behavioral checks: [conformance.rs](../../src/conformance.rs) |
| Run a component in another process/language | [6. Process bridge](06-process-bridge.zh-CN.md) | Line-oriented protocol in the note; event contents use the envelope and payload contracts above |

For an external tool, start with the envelope, component manifest, relevant port profile and process bridge; then use the [component examples](../../examples/components/). An example is evidence of one integration, not a replacement for the whole contract.

## Normative rules and current implementation

- The human-readable contract explains meaning and behavioral obligations. Published JSON Schemas define the document shapes they cover; the table does not imply that every runtime behavior has a separate Schema.
- Rust types, validators and the standard profile catalog locate the current implementation. Rust is not an external-component requirement. The profile catalog currently lives in `profile.rs`, not a separately published complete profile Schema.
- [Schema conformance tests](../../tests/schema_canon.rs) compare the covered schemas, serialized types and examples. Behavioral conformance checks add evidence about execution. Neither proves provider compatibility, all possible inputs, operating-system isolation or every third-party implementation.
- A disagreement between a contract, a schema and implementation needs an explicit issue and resolution. Do not silently redefine the contract to match code, or change runtime behavior merely to satisfy stale prose.

The component note labels bundled tool-visibility and installation behavior separately from manifest fields. The assembly note describes the effective instances and connections delivered to the kernel, not a promise that one disk file contains all dynamic state.

## Product configuration is a separate layer

The chat product prepares a baseline, fills runtime positions and applies installation additions before passing an ordinary assembly to the kernel. [Product assembly configuration (Chinese)](../assembly-configuration.zh-CN.md), [product_assembly.json](../../schemas/product_assembly.json) and [assembly_overlay.json](../../schemas/assembly_overlay.json) describe that layer; they do not add kernel business concepts to the six contracts.

Model accounts and project instructions belong to the [configuration route](../README.md#configure-and-customize), not this reading prerequisite.
