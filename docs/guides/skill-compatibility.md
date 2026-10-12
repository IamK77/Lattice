# Skill compatibility

[中文](skill-compatibility.zh-CN.md)

A skill is a directory containing `SKILL.md` and optional resources. Installing that directory, loading instructions, and successfully performing a task are different operations. This page records the source review and first consumption-correctness slice of #59; it does **not** declare the complete skill-management project delivered.

## There is not one host-independent “latest skill format”

The primary-source review captured the following pages on 2026-10-12. These are observation dates, not claimed release dates. Mutable documentation is not proof of every installed client version's behavior.

| Layer | Primary source | What it defines |
| --- | --- | --- |
| Open Agent Skills format | [Specification](https://agentskills.io/specification) | A directory, YAML frontmatter, Markdown instructions and optional resources. Required `name`/`description`; optional `license`, `compatibility`, `metadata`, experimental string `allowed-tools`. |
| Claude Code host | [Skills](https://code.claude.com/docs/en/skills) | More permissive missing metadata, discovery scopes, invocation flags, argument substitution, dynamic execution and other host behavior. The current page also accepts YAML lists for `allowed-tools`. |
| Codex host | [Skills](https://developers.openai.com/codex/skills) and [OpenAI sidecar reference](https://github.com/openai/codex/blob/main/codex-rs/skills/src/assets/samples/skill-creator/references/openai_yaml.md) | Discovery and invocation conventions; optional `agents/openai.yaml` with interface metadata, implicit-invocation policy and external-tool dependencies. |
| Distribution | [Claude marketplaces](https://code.claude.com/docs/en/plugin-marketplaces), [OpenAI public plugins](https://github.com/openai/plugins) | Packages and indexes, potentially containing multiple skills and non-skill capabilities. These are not interchangeable with `SKILL.md`. |

The open specification still uses YAML plus Markdown, not a replacement format. `$ARGUMENTS`, Claude invocation flags and the OpenAI sidecar are not universal requirements of that base format. Conversely, recognizing the base format does not implement those host semantics.

## What the current implementation actually does

- Scans configured project roots; defaults are `.lattice/skills`, `.agents/skills`, `.claude/skills`, in that order. It does not automatically discover home/ancestor directories. Files somewhere on the machine are not evidence of installation in the active library.
- Validates names and descriptions, permits a missing name by taking the directory name, and requires a present name to match the directory. Names remain ASCII lowercase letters/digits/single hyphens. The specification's Unicode wording is ambiguous; this delivery does not silently broaden path/name rules.
- Accepts string and string-list `allowed-tools`. This is syntax compatibility only: neither form changes tool grants or the authorization path.
- Loads body/resources on demand with existing size and resource-path limits. Every successful explicit body load returns content, even if identical content appeared earlier in the ledger. Historical presence does not prove that compaction retained that content.
- Gives exact skill names precedence over longer prefix suggestions. Reserved application commands retain precedence. In the Rust TUI, Tab completes the name token without removing argument text, folded pastes or image markers; Enter on a skill prefix keeps the draft. In Ink, Tab accepts the first displayed skill suggestion when one exists, otherwise retains its conversation-switching behavior. Unknown command-shaped names retain drafts and show local feedback.
- Preserves a recognized skill's trailing argument whitespace; expansion substitutes the existing whole `$ARGUMENTS` form and changes only the message's text, keeping its image payload. Ink currently has a text-only send path; these tests do not establish Ink attachment support.

A reserved-name skill is not yet available through a separate structured selection; that belongs to the remaining catalog/invocation work. Existing aliases and ordinary non-command paths retain their frontend behavior.

## Known gaps — do not infer full compatibility

These remain open in #59:

- Human/model invocation controls (`disable-model-invocation`, `user-invocable`, OpenAI `policy.allow_implicit_invocation`) are not yet enforced. Do not rely on those declarations as current restrictions.
- Positional/named arguments, escaped placeholders and exact host parameter semantics are not implemented. The current whole-string replacement can misinterpret indexed or similarly named placeholders; do not use those templates until the dedicated argument parser lands.
- Host variables, dynamic commands, hooks, fork/agent context, model overrides and explicit external dependencies do not have the required compatibility diagnostics/enforcement yet. Successful parsing or installation is not proof that such a skill works here.
- Invalid/shadowed entries, unreadable roots and unsupported metadata do not yet have the shared diagnostic catalog or complete human-facing presentation required by #59.
- Lattice's schema is an implementation compatibility profile, not an official schema shipped by Agent Skills. It remains more permissive about `metadata` values and empty `compatibility` than the current open specification. The open specification requires string metadata values and nonempty compatibility when present; stricter validation/migration needs explicit follow-up coverage.
- The runtime consumer/installer remain split. Unified authorized prepare/commit installation, source/index adapters, preview/recovery and complete TUI/Ink management are later #59 slices. The source review does not implement a marketplace or activate any plugin capabilities.

The #59 handoff intentionally specifies some future behavior that differs from Claude, including errors for missing positional arguments. That is a Lattice contract choice, not claimed host parity. Known unmet runtime dependencies must remain explicit; merely finding a command in `PATH` is not version verification.

## Verification boundary

Regression tests cover exact names, explicit completion, whitespace/fold/image preservation, repeat loads, both supported `allowed-tools` syntaxes and invalid list members. Synthetic kernel tests inspect the expanded event referenced by the model request; real frontend handlers are exercised without provider credentials. Negative controls must fail against the old behavior. This is not real-account testing, full host emulation, human acceptance of the remaining management UI, or installation of a new binary.
