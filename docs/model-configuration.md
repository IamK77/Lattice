# Model configuration

[Documentation](README.md) · [First conversation](getting-started.md) · [简体中文](model-configuration.zh-CN.md)

For people connecting a service or adjusting its settings. Use the [guided first-use path](getting-started.md#2-connect-a-model) if you do not need detailed configuration. You do not need to change components or runtime assemblies to connect a model.

## Choose the connection

The guide offers OpenAI, Anthropic and DeepSeek presets, custom configuration, and repair of existing entries. A preset suggests the API format and endpoint; check both before supplying credentials. It is not a guarantee that every model supports every feature.

You may explicitly fetch a service's model list, filter it, or enter an exact identifier manually. Failed or empty discovery does not block manual entry. Previously fetched names offer Tab completion without another request; long names remain searchable in the list even when inline completion would not fit. Fetching the list is not a generation request or a compatibility test.

Review the model, API format, endpoint, local name, default-preference action and main capabilities before saving. **More settings and details** exposes the full configuration, save location, capability sources, name, default and language settings. Leaving the default preference unchanged does not unset a previously chosen default.

The optional connection test sends a short request and **may be billed**. You can skip it. After failure, explicitly retry, edit, skip or exit; the guide does not retry automatically. Success does not establish compatibility with tools, reasoning history or every hosted feature.

## Capabilities and capacity

The guide uses available metadata and asks for missing or invalid capacity values. Confirm suggestions against the service you use:

- Image input is separate from hosted image generation. Hosted search and image generation are offered only for the Responses format. Unknown capabilities stay unchecked.
- Space toggles capability checkboxes and Enter confirms. Thinking checkboxes declare supported levels, not the current thinking strength. Common names appear weakest first; custom names retain their order. Advanced settings retain custom level names/order and usage-field mappings.
- Token limits accept exact integers and case-insensitive decimal suffixes: `1M = 1000k = 1000000`, `1.5M = 1500000`. These are not binary units: enter `1048576` if that is the exact documented limit. Fractional tokens and overflow are rejected, not rounded. The submitted value displays its expanded count; output must leave room for input.
- A reported input ceiling is not necessarily the total context window. Anthropic input limits are presented as a **conservative total-budget suggestion**, without adding output tokens. Confirm or edit the suggestion.

## Language, navigation and saving

Setup and credential repair support English and Simplified Chinese, independently of the main terminal interface. Select **Language / 语言** at home or under the new-model review's **More settings and details**; repair offers the option directly on review.

An explicit language choice is saved as `setupLanguage` (`en` or `zh-CN`) in preferences. Without a saved choice, detection tries `LC_ALL`, `LC_MESSAGES`, then `LANG`: Chinese locales use Simplified Chinese; otherwise English. Automatic detection does not save a choice. A failed preference write is reported; the language still applies for this run. Provider diagnostics are shown as original text.

Esc returns to the previous step or exits at home. Ctrl-C exits while waiting for input; during a network request, cancellation is handled after that bounded request finishes and does not resend it. Submitted fields remain in the draft, but unsubmitted text cancelled in a prompt is not saved. Returning home keeps new-model and credential-repair drafts. Changing the endpoint, API format or account refreshes dependent lists/capabilities; changing the model resets its capabilities. A new endpoint does not inherit the old key. Re-fetching the same model's information does not overwrite fields you already edited.

Cancelling before model save creates neither a model entry nor a conversation. Explicitly saved language preferences and earlier saved configuration remain. An existing usable configuration skips setup; background/noninteractive commands do not open it. If you asked to resume a conversation, setup does not replace that selection with a different conversation.

Do not delete a damaged model file to get past an error: invalid catalogs are not reset. Repair the indicated file and check again. If the model saves but the default preference cannot, the guide reports that and can continue with the model for this launch. Existing startup environment overrides can still take precedence next time; see [model troubleshooting](troubleshooting.md#no-usable-model-or-unexpected-provider).

## Manual configuration

The default model catalog is `~/.lattice/models.json`. Create the parent directory if needed:

```bash
mkdir -p "$HOME/.lattice"
```

Create or edit `models.json` in your editor. **Preserve existing entries.** `LATTICE_MODELS` can point to another catalog file. The following is a configuration example, not a usable account; replace the endpoint, model identifier and limits using the provider's documentation. The `.invalid` address deliberately cannot connect to a real service.

```json
{
  "models": {
    "my-model": {
      "adapter": "openai",
      "model": "your-model-id",
      "baseUrl": "https://api.example.invalid/v1",
      "apiKeyEnv": "LATTICE_MODEL_KEY",
      "profile": {
        "contextWindow": 32768,
        "maxOutputTokens": 4096
      }
    }
  }
}
```

| Adapter | Endpoint protocol |
| --- | --- |
| `openai` | Chat Completions |
| `responses` | Responses |
| `anthropic` | Anthropic Messages |

These select a protocol, not guaranteed support for every provider extension. Use the endpoint root, exact model ID, limits and settings appropriate to your service. Full field references: [model catalog](../schemas/model_catalog.json) and [model profile](../schemas/model_profile.json).

Supply credentials using your existing credential manager, or enter a key in **Bash** without putting its literal value in shell history:

```bash
printf 'API key: '
read -r -s LATTICE_MODEL_KEY
printf '\n'
export LATTICE_MODEL_KEY
```

Keep this terminal open when launching Lattice. `apiKeyEnv` names the variable; it does not contain the key itself. The alternative `apiKey` stores the literal key in the catalog. Prefer an environment-variable reference, and never commit a real key. See [First conversation](getting-started.md#2-connect-a-model) to launch with this configuration.

## Credential and terminal limitations

- A locally saved key is in an **agent-readable file**. Tool reads can place it into saved history and model input. Keeping a key out of shell history or using an environment variable does not hide it from tools.
- On Unix, setup creates its temporary credential file with owner-only access before writing the key. This is not isolation from the agent. Platforms without supported protected storage are not presented with a supposedly safe local-key option. Setup does not edit shell startup files.
- Setup requires interactive input, output and error streams and a terminal at least 40 columns by 14 rows. Its pages replace the previous task rather than accumulating questions; ordinary cancellation, errors and handoff restore the previous terminal screen. A forced kill or crash is not covered by that restoration guarantee.
- If you resize while waiting in an input field, the whole page does not immediately redraw; layout is recalculated between questions and detail pages. Long explanations have separate pages; retries replace the error rather than add a transcript.

For implementation, request records and model-free developer checks, see [setup development notes](setup-development.md). These are not additional configuration steps.
