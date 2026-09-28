# Contributing to Lattice

[Home](README.md) · [Getting started](docs/getting-started.md) · [完整贡献规则（中文）](CLAUDE.md)

Contributions should make Lattice easier to use, understand, or customize. For a new capability or a change to the architecture, discuss the user problem and intended behavior before implementing a broad redesign.

## Report a problem

Include the Lattice version, operating system, relevant configuration **without credentials**, reproduction steps, and expected versus observed behavior. Say whether the issue also occurs with optional integrations disabled, if you tested that.

Do not attach an entire conversation history or dump the environment. Tool results, screenshots, and local state may contain secrets. Use a minimal synthetic reproduction and review every excerpt before sharing it.

## Make a focused change

- Read the affected implementation and its contract first. Keep unrelated changes out of the patch.
- Add a regression test for new behavior or a bug fix. Show that the test detects the incorrect behavior, then restore the intended implementation precisely.
- Run the affected tests, not the full suite by default. Do not run multiple Cargo builds against the same target directory at once.
- Use observable synchronization in tests instead of sleeps. Report ignored tests and unverified integrations explicitly.
- Do not submit real credentials, personal conversation records, local runtime directories, or internal experiments. Use synthetic fixtures and preserve third-party attribution.

Typical checks, run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --test bridge --no-fail-fast
```

Replace `bridge` with the affected test target. The complete Rust suite is `cargo test --no-fail-fast`. Some tests start Python 3 subprocesses. Frontend checks run from `clients/ink` with Node.js (CI uses Node 22):

```sh
npm ci
node --test
```

Real model endpoints, browser integrations, and desktop permissions require separate explicit verification. A skipped check is not a passing check.

## Preserve the boundaries

Components communicate through serializable data. Rust is the implementation language of the core and bundled components, not a requirement for external components. Restore state from recorded history; do not re-execute historical side effects. Context compaction changes the model's view, not the audit record.

Each call has one outcome. An interruption is an unknown-effect ending, not proof of failure or permission to retry. Reuse the common settlement rules rather than adding a consumer-specific definition. Do not describe self-declared tool permissions or process isolation as a sandbox, and do not hide read errors by treating them as missing data.

Detailed architecture constraints and rationale are currently in Chinese: [contribution rules](CLAUDE.md), [overview](docs/架构总览.md), [design decisions](docs/架构决策.md), and [contracts](docs/contracts/). Machine-readable interfaces are in [schemas](schemas/).

## Branches and pull requests

Follow the [Git Flow workflow](docs/workflow.md) ([中文](docs/协作流程.md)): ordinary work starts on `feature/<name>` from `develop` and returns through a pull request. Only release and hotfix branches enter `main`; synchronize published work back into `develop`. Do not push directly to either long-lived branch.

Use merge commits, not squash or rebase merges. A pull request's English title and body become its default merge message; use a `chore:` or `chore(scope):` title and end the body with `Type: chore` so merging does not count the original changes twice. Both protected branches require the `workflow`, `check`, and `frontend` checks. See the workflow for release authorization and the single-maintainer review limitation.

Run the workflow regression tests when changing these checks:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

## Language and commits

Code comments, assertions, and diagnostics are in English. The public README and getting-started path have English and Simplified Chinese versions; keep paired instructions and examples aligned. Deeper documentation may remain in Chinese, with language labels on English entry links.

Write commit subjects and bodies in English. Follow Conventional Commits: `type: description` or `type(scope): description`, for example `feat: add model switching` or `fix(cli): handle missing credentials`. Allowed types are `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `style`, and `chore`. Keep a matching final trailer, such as `Type: docs`, separated from the body by a blank line: the existing version calculator reads this trailer, and CI requires it to match the subject prefix. Git builds need the complete public history; source archives without Git use the version in `Cargo.toml`.

Repository instructions do not authorize a push, release, program installation, service restart, or destructive action on a maintainer's behalf.

## License status

A project-wide license has not yet been selected. Do not assume a license grant from public visibility. Third-party sources and attribution remain in their own files, including [assets](assets/README.md).
