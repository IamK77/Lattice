# File and skill capability split

This is an unreleased breaking change. The old combined providers are removed,
not retained as compatibility aliases. Existing ledgers are not rewritten.

## What changes

| Removed provider | Replacement | Operations |
| --- | --- | --- |
| `fs-tools` | `fs-reader` | `Read`, `Ls` |
| `fs-tools` | `fs-writer` | `Write`, `Edit` |
| `skill-library` | `skill-consumer` | `LoadSkill`, slash expansion, listing and refresh |
| `skill-library` | `skill-installer` | `InstallSkill` |

The standard assembly and worker include both file providers and both skill
providers. Explorer includes the reader, search tools and skill consumer;
researcher includes web tools and the skill consumer. Neither read-only expert
has an installation or user-file mutation tool. Their deferred tool catalogs
are filtered against the declarations of their actual assembled instances.

This is an operation boundary, **not an operating-system sandbox**. Model
requests still use the network; runtime ledgers, caches and process machinery
can still write runtime-owned data. Read and network access can disclose data.
Provider-native capabilities and arbitrary third-party components need their own
review. No global effects-policy enforcement or new core permission mechanism
is introduced here; sandboxing remains separate work.

## Migrate an assembly or overlay

Export a fresh standard assembly with `lattice assembly` and compare it with
your custom assembly. Lattice does not silently translate or rewrite old files.

1. Replace an old `fs-tools` instance with `fs-reader`. If writes are intended,
   add a separate `fs-writer` instance. Copy the applicable `root`, size-limit and
   `exclusive` configuration to the corresponding instances. The standard
   instance names are `fs` and `fs-write`; instance names remain your choice.
2. Replace `skill-library` with `skill-consumer`. Keep its `dirs`, `maxBytes`,
   prompt and expansion/refresh wiring. Add `skill-installer` only if installation
   is intended; give it `installDir` or `dirs` (the first directory is the default
   destination). Ensure the consumer scans that destination. The standard
   instance names are `skills` and `skill-installer`.
3. Wire each provider's `execute` to the existing request path **after all
   applicable gates**, and its `outcome` to the loop's tool-result input. Keep
   the installer behind the admission/trust gate. Do not bypass a custom gate
   while migrating. The installer now explicitly declares its source reads as
   well; prior grants with the narrower declared surface may require renewed
   approval. Do not copy or widen grants to suppress that review.
4. Keep `skills.changed → skills.refresh` for filesystem notifications. Add
   `skill-installer.changed → skills.refresh` for successful installations.
   **Do not wire this notification to the loop input**: it refreshes the menu,
   not a model turn. The consumer owns `skill.listing`; the installer owns
   `skill.installed`. Neither requires the other's event declaration to boot.
5. Keep `ui.user → skills.input → skills.expanded → loop.input` (as two wires)
   wherever slash expansion is used. Remove stale component entries and deferred
   tool names from custom catalogs. Revalidate the complete assembly.

An installation is immediately loadable from the configured scan directory.
The explicit notification refreshes the menu even when the destination did not
exist when the consumer started watching. Resident prompt adoption still follows
the context manager's cache rules. On restart the consumer rescans disk; it does
not replay old installations. A historical installation event does not require
an installer in the reopened assembly.

## Embedded and process consumers

Rust callers replace `fs_tools::{NAME, manifest, FsTools, tool_decls}` with
`READER`, `reader_manifest`, `FsReader`, `read_decls` and/or `WRITER`,
`writer_manifest`, `FsWriter`, `write_decls`. Both use `from_config`.

Replace `skill_library::{NAME, manifest, SkillLibrary}` with `CONSUMER`,
`consumer_manifest`, `SkillConsumer` and/or `INSTALLER`, `installer_manifest`,
`SkillInstaller`. Factories must be registered under the new component names.
There is no combined constructor or full-capability default alias.

Process entries become:

```text
lattice component fs-reader
lattice component fs-writer
lattice component skill-consumer
lattice component skill-installer
```

The old process commands are rejected. The bridge and event-envelope contracts
are unchanged. Update stored process manifests/overlays explicitly; do not edit
historical ledger events to rename their sources.
