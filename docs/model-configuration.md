# Model configuration

[Documentation](README.md) · [First conversation](getting-started.md) · [简体中文](model-configuration.zh-CN.md)

This reference explains how to connect a model service, enter capacity values and save settings. Start with [guided setup](getting-started.md#2-connect-a-model), then return here to look up individual options.

## Choose the connection

The following steps describe initial setup. With a usable model already configured, startup goes directly to the conversation. To change a saved address or capacity, find and edit the model file using [Manual configuration](#manual-configuration).

Choosing OpenAI, Anthropic or DeepSeek fills in a suggested API format and service address. Choose custom configuration for another service, or repair an existing configuration. Before entering a key, confirm that the address belongs to the service you intend to use.

Choose a model from the service's list or enter its name. If fetching the list fails, continue with manual entry. Previously fetched names are searchable and support Tab completion using the existing list. Long names can be selected through list search.

Before saving, check the service address, model name, API format and whether to make this the default model. **More settings and details** includes the save location, sources of capability information, local name and language. Keeping the previous default preserves that selection.

The **connection test** sends a short question to check whether the service answers, billed under its pricing rules. You can skip it. After a failure, setup offers retry, edit, skip and exit so you can choose the next step. Select capabilities such as image input using the service's documentation. Thinking levels record the strengths the model supports; choose the active strength during the conversation.

## Capabilities and capacity

### Enter capacity values

Capacity describes how much content fits in a model request, measured in tokens, the model's unit for content length. There are two main values:

- **Context window:** the combined limit for input and the model's response in one request.
- **Maximum output:** the limit for one response. Keep it below the context window to leave room for your question and conversation history.

Setup uses available model information and asks for missing values. Follow your service's documentation. You can enter integers or use `k` and `M`: `32k` means `32000`, and `1.5M` means `1500000`; suffixes are case-insensitive. Enter exact values such as `1048576` directly when specified by the service. Setup displays the expanded number and asks you to correct invalid input.

Some services report only an input limit. For Anthropic, setup uses that number as a conservative suggestion for total capacity, without adding output tokens. Confirm or adjust it using the service's documentation.

### Choose capabilities

Use Space to select options in the capability list and Enter to confirm. Leave image input, hosted search or image generation unchecked until you know whether the service supports them.

- **Image input** lets the model see pictures; **image generation** asks the service to create them. Hosted search and image generation use the Responses format.
- **Thinking levels** list the strengths the model supports. Configure the available choices here and select the strength when using the model. Common levels appear weakest first; custom levels retain their order.

## Language, navigation and saving

### Change language

Choose **Language / 语言** directly on the setup home page. On the new-model review page, find it under **More settings and details**. Credential repair puts the option on its review page. It applies to setup and credential-repair pages.

Your choice is saved as `setupLanguage`, with value `en` or `zh-CN`. Initially, setup checks `LC_ALL`, `LC_MESSAGES` and `LANG` in that order: Chinese locales use Simplified Chinese, and others use English. Once you choose a language, the saved choice takes precedence. If saving fails, setup uses your choice for this run and displays the save error. Service diagnostics retain their original wording to help with troubleshooting.

### Go back or exit

Esc goes back, or exits from the first page. Ctrl-C also exits while waiting for input. Cancellation during a network request is handled after the current request finishes, without sending it again.

Confirmed fields stay in this setup session's draft, including when returning home. Cancelling a text field discards its unconfirmed text. Exiting before saving the model leaves the draft unsaved; a language choice saved before exiting remains in place.

### Change connection details

After changing the address, API format or account, fetch the list again and confirm capabilities. Changing the model also requires confirming its capabilities. Enter a key again for a new address; the old key stays with the old address. Fetching information again for the same model preserves fields you have edited yourself.

### Resolve save errors

If a configuration file cannot be read, back it up and repair it using the reported error. Lattice keeps the original file to preserve existing accounts. If the model saves but the default selection fails to save, setup explains what happened and can still use that model for this conversation. If the next launch selects another model, check environment variables and defaults using [model troubleshooting](troubleshooting.md#no-usable-model-or-unexpected-provider).

Startup opens the conversation directly when a usable configuration exists; background and noninteractive commands also skip setup. When resuming a conversation, completing setup returns to the conversation you selected. Exiting setup before saving the model ends that launch.

## Manual configuration

The default model configuration file is `~/.lattice/models.json`. Create its parent directory if needed:

```bash
mkdir -p "$HOME/.lattice"
```

Open or create `models.json` in an editor. Add models to the existing contents when a configuration is already present. `LATTICE_MODELS` selects another configuration file. This example uses a placeholder address and model; replace the address, model and capacity with your service's values:

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

`adapter` selects the API format. Choose the row that matches your service's documentation:

| `adapter` value | API format |
| --- | --- |
| `openai` | Chat Completions |
| `responses` | Responses |
| `anthropic` | Anthropic Messages |

See [model configuration fields](../schemas/model_catalog.json) and [model profile fields](../schemas/model_profile.json) for other options.

Supply the key using your credential manager, or run these commands in **Bash** and enter the key when prompted. Input is hidden and stays out of shell command history:

```bash
printf 'API key: '
read -r -s LATTICE_MODEL_KEY
printf '\n'
export LATTICE_MODEL_KEY
```

Keep this terminal open and launch Lattice using [First conversation](getting-started.md#2-connect-a-model).

<a id="credential-and-terminal-limitations"></a>

## Key storage and terminal use

### Store a key

`apiKeyEnv` records an environment-variable name in the configuration; `apiKey` records the key itself. Prefer environment variables to reduce copies of keys in files.

Tools can read either location. Keys read from files or command output are saved with tool results in conversation records and may be sent to the model service. Check for keys and other sensitive material before committing configuration or sharing records.

On Unix, setup writes keys with owner-only file permissions; the running agent can read the file too. Platforms that cannot set those permissions use the environment-variable method. Setup leaves your shell startup files in place.

### Use the terminal

Run setup in an interactive terminal at least 40 columns wide and 14 rows high. Each step updates the current page, and long explanations have separate pages. After resizing, moving to the next page recalculates the layout.

Normal exits restore the previous terminal screen. If the display is left in an unusual state after a forced stop or crash, open a new terminal.

See [setup development notes](setup-development.md) for implementation and tests.
