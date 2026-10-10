# Setup implementation and verification

[Documentation](../README.md#develop-and-maintain) · [Contributor workflow](../../CONTRIBUTING.md) · [User configuration reference](../guides/model-configuration.md)

For contributors working on first-run setup, not a prerequisite for configuring an account. User choices and observable limitations belong in [model configuration](../guides/model-configuration.md); architectural rationale remains in [architecture decisions (Chinese)](architecture-decisions.zh-CN.md#模型适配与上下文).

## Frontend lifecycle

Setup runs before creating the conversation or opening the main terminal UI. It maintains unsaved drafts; submitted fields survive backward navigation, but cancelled, unsubmitted input does not. Changing endpoint, format or account invalidates dependent discovery/capability data. Changing models rematches capabilities; re-fetching the same model must not overwrite manually edited fields. An endpoint change never carries the old credential forward. Repairing only a key must preserve unknown fields and unrelated capabilities.

A temporary screen isolates setup from the main UI. The outer guide owns page headers, detail pages and terminal lifecycle; the input widget owns an individual prompt. Each task replaces its predecessor, errors replace earlier errors, and long explanations use separate pages. Layout is recalculated between questions/details rather than promising a full redraw while a widget blocks. Ordinary exits and unwinding restore the pre-launch screen; forced termination and crashes are outside that guarantee. During synchronous bounded requests, cancellation input is handled after the request, not advertised as an immediate network abort. The setup screen is released before the main UI acquires the terminal.

Language is a frontend preference, not a model or main-UI setting. Explicit `setupLanguage` wins over locale detection; automatic detection does not write preferences. A failed preference write affects persistence, not the language already chosen for this run. Service diagnostics remain identified as original text, not translated as if they were Lattice's diagnosis.

## Discovery, probes and evidence

Model-list retrieval and short connection probes are explicit frontend operations, not chat messages. Each attempt has intent and outcome records under `setup-tests/` beside the catalog. These records do not store keys, request bodies or provider response bodies. An intent without an outcome means an unknown result, not permission to replay it.

Discovery supports bounded Anthropic pagination and limits response sizes, entry counts and total duration. It does not follow redirects or automatically retry. A GET list response is not proof of generation-format or tool compatibility. A short text probe cannot establish tool handling, reasoning-history compatibility or every hosted feature. The bundled DeepSeek template records its upstream references in [deepseek.json](../../src/bin/lattice/setup/deepseek.json); this is not a real-account acceptance result.

An Anthropic input ceiling is treated as a conservative total-budget suggestion, without adding the output ceiling. Missing image capability metadata remains disabled. Input image support, hosted image generation and hosted search are distinct fields; thinking choices declare a supported range rather than the current strength.

## Storage and startup integration

Setup preserves unreadable or invalid catalogs rather than resetting them. On Unix, the writer creates an owner-only temporary file before putting credentials in it. This does not isolate keys from the agent's tools. Unsupported protected storage is not silently treated as safe. Adapter configuration carries an environment-variable name, not a key value; tools can still read keys from files or the environment, and their results may persist.

Model save and default-preference save have separate outcomes: if the latter fails, report it without pretending the model was not saved. Setup does not edit shell startup files. Explicit startup environment overrides still take precedence later. An explicitly requested resumed conversation remains the target after configuration; cancellation before model save creates neither a model entry nor a conversation, but does not roll back preferences or configuration already saved explicitly.

Implementation entry: [setup.rs](../../src/bin/lattice/setup.rs). The source-specific preservation, discovery, input, screen and wizard tests live alongside it. Request transport and terminal tests do not stand in for real-account or human UI acceptance.

## Developer checks without a provider

For a scripted terminal-UI check, after building the binary from the source checkout:

```bash
LATTICE_SCRIPTED=1 ./target/release/lattice
```

The mode is selected by the variable's presence: `LATTICE_SCRIPTED=0` also enables it. Remove the variable to return to a real model. This is not a demonstration of model quality or a real setup/provider acceptance test. **It is not a sandbox and can still load configured assembly additions.** Use deliberate test configuration and do not point exploratory checks at valuable user history. See [contribution checks](../../CONTRIBUTING.md#make-a-focused-change) for selecting affected automated tests.

A separate [model-free component example](../README.md#develop-and-maintain) demonstrates the event flow, not the first-run user experience. Real provider requests and desktop/browser permission checks require explicit, separate acceptance; do not perform them merely to validate prose.
