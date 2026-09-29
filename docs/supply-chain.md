# Dependency and CI maintenance

[中文](依赖与持续集成.md)

## What is checked

CI uses read-only repository access for candidate code and disables checkout's
persisted Git credentials. Actions are pinned to full commit IDs. Pinning fixes
an input; it is not an independent audit of that input.

- Linux: formatting, warning-free Clippy, the Rust suite, and the heartbeat example.
- macOS ARM64: executable/version/community/schema checks and the library without
  default features. This is not a claim that every integration is tested on macOS.
- Rust compatibility: a separate job checks the declared minimum toolchain;
  the main Rust jobs also exercise the current stable toolchain.
- Frontend: `npm ci --ignore-scripts` from the committed lockfile, Node tests,
  and a real daemon/client smoke using a scripted model, not paid credentials.
- Rust dependencies: cargo-deny checks the locked graph's known advisories,
  accepted licenses, and registry/Git sources. Unknown sources are rejected.
  `unsound = "all"` and `unmaintained = "all"` explicitly include indirect
  dependencies; enabling all Cargo features does not widen these advisory scopes.
- Frontend production dependencies: npm audit rejects high/critical findings.
  Lower severities remain visible in its output; this is not a zero-risk claim.
- Workflow YAML: actionlint checks syntax. Its optional external shellcheck and
  pyflakes integrations are disabled; this does not replace script tests.

`Cargo.lock` and `clients/ink/package-lock.json` are reviewed source inputs.
Never replace a failing locked install with an unlocked fallback. Do not commit
credentials or private registry URLs into either lockfile.

## Dependency updates

Dependabot proposes weekly grouped updates for Cargo, npm, and Actions, targeting
`develop`. These proposals still need review and successful checks; they are not
authorization to merge or release. Its `dependabot/` branches are a narrow routing
exception: only same-repository PRs authored by `dependabot[bot]` may use that
route, and never directly into `main`. A lookalike branch name is insufficient.

Keep action commit IDs and their version comments aligned when updating. Review
new permissions, install scripts, sources, licenses, and changes to test/release
logic as well as version numbers. Dependabot does not update arbitrary binary
URLs or versions embedded in shell commands: actionlint, cargo-deny, and later
packaging tools need explicit maintenance too.

Advisory databases change even when the lockfile does not. A previously green
revision can become red. Fix a finding or document a narrowly scoped exception
with its specific advisory, reason, owner, and review date; never disable an
entire advisory/license/source class merely to restore green checks.

The current maintenance exceptions are `bincode 1.3.3` (RUSTSEC-2025-0141)
and `yaml-rust 0.4.5` (RUSTSEC-2024-0320), introduced by syntect. Their upstream migrations remain open work, not
resolved findings. [Exception records](../advisory-exceptions.json) name the
maintainer and expire on October 28, 2026. CI rejects an expired exception, a
changed dependency version, or an ignored advisory without a matching record.
Do not extend the date without reviewing the upstream state and recording why.

## Boundaries

The license allowlist selects acceptable alternatives in dependency license
expressions; it does not relicense dependencies or constitute legal advice.
NOTICE and asset attribution must accompany distributed copies where required.

Passing checks do not establish sandboxing, paid model compatibility, code-signing,
reproducible builds, or absence of vulnerabilities. Repository protections live
on GitHub and must be checked separately from this YAML. See [workflow](workflow.md)
for the protected-branch policy and [Security](../SECURITY.md) for private reports.
