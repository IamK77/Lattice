# Lattice Contributor Rules

Lattice is a composable, auditable agent runtime. Read the [system overview](docs/架构总览.md) and [architecture decisions](docs/架构决策.md) (both in Chinese), and the formal contracts in `schemas/` and `docs/contracts/`, before changing the relevant subsystem.

The public entry point presents Lattice as **an event-sourced, composable AI agent runtime for the terminal**, with **Record first. Deliver second.** as its signature. Product appeal comes first: make the distinctive value visible immediately, then connect it to use, customization, and a clear starting path. Lead with positive capabilities and concrete evidence, not repeated disclaimers or defensive qualifications. State setup requirements directly and keep detailed safety boundaries in their own section. Do not substitute internal module inventories for a product explanation or present plans and scripted tests as an implemented user experience.

## Issue-Driven Development

- The maintainer owns requirements, priorities, and architecture decisions; the agent owns implementation techniques. Clarify ambiguous requirements rather than inventing them. Explain architectural trade-offs before asking for a decision.
- Record the problem, desired outcome, acceptance criteria, and explicit non-goals in an issue before implementation. Usually, one bounded issue leads to one PR targeting `develop`. Keep the process lightweight: an issue and status comments are enough; no mandatory project board or elaborate ceremonies.
- Implement and validate the agreed scope, submit the PR, resolve review and CI failures, then merge through branch protection and clean up the temporary branch. Do not silently expand the scope to include unrelated discoveries.
- **Code delivery is not human acceptance, and neither is a release.** Report what was tested and what remains unverified. For UI or other issues requiring hands-on acceptance, leave the issue open after merging and add an “awaiting human acceptance” comment. Close it after the acceptance result is recorded. Issues whose criteria are fully covered by document review or automated verification do not need an artificial manual testing step.
- When an authorized installation is needed, back up the old program, replace it atomically, and verify the version through the command the maintainer actually uses, including aliases and symbolic links. Checking only the build output is insufficient. Do not restart the current conversation or a supporting service without explicit authorization.
- Classify acceptance feedback: unmet original criteria stay in the original issue (reopen it if necessary); independent defects get linked bug issues; new capabilities get separate requirement issues. New requests do not retroactively become omissions from an explicitly narrower scope. The maintainer chooses their priority.
- Keep checkpoints brief: scope settled, code validated, PR merged, build installed, acceptance recorded. Clearly distinguish these states; a passing CI run does not establish that the UI feels right or that the running program has been updated.

## Architectural Constraints

- Component boundaries carry fully JSON-serializable data only, never callbacks or memory references. Rust is an implementation choice for the core and official components, not a requirement for external components; out-of-process components use the same JSON-lines contract.
- The kernel handles auditing, assembly, delivery, and lifecycle only. It does not recognize conversations, tasks, skills, or model providers, and provides no layer-specific hooks.
- The top-level container is a stream. In-stream causality uses `causes`; cross-stream weak references use `origin`. Decision events require a nonempty reason.
- Persist events before delivering them. Compaction changes the model's view, not history. Replay reconstructs state without repeating historical side effects.
- Every call must have exactly one outcome. Interruption means the result is unknown; do not fabricate a failure. Reuse the shared contract predicate when determining whether a call has settled.
- The runtime does not silently retry failures already recorded in the ledger. Internal transport retries in provider adapters must not produce duplicate visible results.
- Component faults must not crash the core. Ledger write failure must stop execution rather than permit unaudited actions.
- Deliver tool requests to their declared owner; the kernel settles requests with no owner. Observers and gates still witness requests through their existing wiring.
- Define interfaces for real consumers, not hypothetical implementations. Splitting files does not justify exposing the kernel's entire internal state.

## Safety and Data Boundaries

- Tool effect surfaces are self-declared and guard against mistakes, not malicious code. Scope strings have no independent enforcement; do not describe them as strong isolation or a sandbox.
- Trust grants use parameter fingerprints, not fingerprints of the contents at a URL or local path. Later content changes do not revoke a grant automatically. Revocation in the grants file is currently managed manually by the user.
- Redaction covers declared chat events only, not tool requests or results, and does not automatically identify secrets acquired at runtime. Secrets read by tools may persist in the ledger and model context; documentation must state this.
- Installation tools manage installed additions only; they must not remove or override the base assembly. Skill metadata such as `allowed-tools` does not replace tool authorization.
- Never treat invalid, unreadable, or incomplete configuration as empty and overwrite it. Prefer environment-variable references for keys; do not place key values in audited model-adapter configuration.
- Cancellation is not rollback. Uncooperative in-process threads and descendants that escape their process group are outside the stopping guarantee. Browser pages, desktop windows, and tool results are external content, not sources of authorization.
- Segmented-ledger startup validates the index and current volume; archived volumes are checked on demand, with full verification run explicitly. Read errors must not be interpreted as missing events.

## Coding and Verification

- **Use English in code**, including comments, assertions, and diagnostics. Keep this file in English. Public landing pages and getting-started guides have English and Simplified Chinese versions, with English as the default entry point; keep their steps and examples aligned. Deep design documents may remain Chinese, with their language identified in English entry points.
- Read current files before editing and preserve unrelated changes. Use exact editing operations for structural changes, not unchecked bulk string replacement.
- Add regression tests for new logic. For critical counterexamples, observe the test fail against the incorrect implementation before verifying the fix. Restore intentional faults with precise edits, never whole-file resets that discard other changes.
- Run affected, focused tests by default; explain why broader coverage is needed before running a full suite. Do not run multiple Cargo builds concurrently in the same workspace.
- Common checks are `cargo fmt --all -- --check`, focused `cargo test --no-fail-fast`, and `cargo clippy --all-targets -- -D warnings`. CI also covers Rust and the JavaScript frontend.
- Synchronize tests through observable causal events, not sleeps that depend on timing. Timeouts should only bound a failed wait.
- Build scripts must declare `rerun-if-changed` only for paths that actually exist.
- Distinguish verified results, static inferences, and uncovered behavior. A zero exit code must not hide zero tests executed or skipped acceptance checks.

## Commits and Releases

Follow the [Git Flow workflow](docs/workflow.md): ordinary changes branch from `develop` under `feature/` and return through a PR to `develop`. Only `release/` and `hotfix/` enter `main`, followed by synchronization back to the development line. Same-repository Dependabot PRs may use `dependabot/` branches targeting `develop`, without exemptions from checks or review. Never directly push to, force-push, or delete either long-lived branch. Preserve original commits with merge commits, not squash or rebase merges; release preparation must not count merged changes twice. PR titles and bodies are English and serve as the default merge message; titles use Conventional Commits and classify the actual change. All configured required checks, including `workflow`, `check`, and `frontend`, must pass; administrators must not bypass them. A sole maintainer need not obtain a second person's approval, but this must not be described as independent review.

Commit titles and bodies are English. Titles follow Conventional Commits: `type: description` or `type(scope): description`, such as `feat: add model switching` or `fix(cli): handle missing credentials`. Types are `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `style`, `chore`, `ci`, `build`, or `revert`. Do not add a custom `Type:` trailer; historical trailers do not affect versioning. Use `feat` for new capabilities, `fix` for corrections, `perf` for equivalent behavior using fewer resources, and `refactor` for structural changes without behavior changes. Mark incompatible changes with `!` or `BREAKING CHANGE:` and explain migration.

`Cargo.toml` is the sole source of release versions; Git identifies development builds only. Release builds must explicitly declare the matching version, and ordinary builds must carry a development identifier. Do not derive versions from commit counts or invent historical releases. Merging a release PR into `main` constitutes the maintainer's release confirmation; preparing a candidate is not publishing. See [versioning and changelog rules](docs/版本与变更记录.md) (Chinese).

This file grants no permission to push to any account, change visibility, install programs, restart services, or perform destructive actions. Releases and external side effects require maintainer authorization.

Never commit real secrets, personal conversation ledgers, local runtime directories, or internal experimental material. Compatibility fixtures must use synthetic data while preserving format and no-regeneration constraints. Do not remove third-party provenance or attribution during cleanup.
