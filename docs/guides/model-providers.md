# Model providers in setup

[Documentation](../README.md) · [简体中文](model-providers.zh-CN.md)

Setup offers connection presets for common providers. Select the provider, choose the endpoint that belongs to your API account when prompted, and supply its API key. You can edit the address and enter a model ID manually. These are connection presets, not bundled accounts or a fixed model catalog.

The table describes the implementation under issue #95. **This work has not performed live-account verification.** Official documentation establishes the addresses and wire formats; local synthetic tests establish only the behavior those fixtures exercise. A successful short connection test is not a complete tool-call compatibility test.

## Standard API presets

Chat means OpenAI Chat Completions. The new presets use this format even when a provider also documents other APIs: those alternatives are not automatically interchangeable. Regions are explicit choices, not inferred from language or a key. Model availability depends on the selected account and endpoint.

| Provider | Endpoint choices | Model selection | Primary documentation |
|---|---|---|---|
| OpenAI | Standard API | Explicit list request or manual entry | [API reference](https://developers.openai.com/api/reference/resources/models/methods/list) |
| Anthropic | Standard API | Explicit list request or manual entry | [Models](https://platform.claude.com/docs/en/api/models/list) |
| DeepSeek | Standard API | Explicit list request or manual entry | [Models](https://api-docs.deepseek.com/api/list-models/) |
| Google Gemini | Developer API, OpenAI-compatible endpoint | Explicit list request or manual entry | [Compatibility and signatures](https://ai.google.dev/gemini-api/docs/openai) |
| Alibaba Cloud Model Studio / Qwen | Beijing, Singapore, Virginia | Manual ID from the service console | [Endpoints](https://help.aliyun.com/zh/model-studio/base-url), [native model list](https://help.aliyun.com/zh/model-studio/list-models) |
| Moonshot / Kimi | China, global | Explicit list request or manual entry | [China](https://platform.kimi.com/docs/api/overview), [global](https://platform.kimi.ai/docs/api/overview) |
| Zhipu / Z.AI / GLM | BigModel China, Z.AI global | Manual ID from the service console | [Standard and subscription endpoints](https://zcode.z.ai/en/docs/configuration) |
| MiniMax | China, global | Manual ID from the service console | [China](https://platform.minimax.cn/docs/api-reference/text-openai-api), [global](https://platform.minimax.io/docs/api-reference/text-openai-api) |
| Volcengine Ark | Beijing standard API | Manual model or inference-endpoint ID from the service console | [Integration](https://docs.volcengine.com/docs/ark/integrate-third-party-tools?lang=zh) |
| SiliconFlow | China, global | Explicit list request or manual entry | [China models](https://docs.siliconflow.cn/docs/api/models-get), [global models](https://docs.siliconflow.com/en/api-reference/models/get-model-list) |
| OpenRouter | Standard API | Explicit list request or manual entry | [API](https://openrouter.ai/docs/api/reference/overview) |
| xAI | Global, US regional | Explicit list request or manual entry | [API](https://docs.x.ai/developers/rest-api-reference/inference), [region](https://docs.x.ai/developers/faq/security) |
| Mistral | Standard, EU regional, US regional | Explicit list request or manual entry | [Regional inference](https://docs.mistral.ai/inference/regional-inference) |
| Groq | Standard API | Explicit list request or manual entry | [API](https://console.groq.com/docs/api-reference) |
| Together AI | Standard API | Explicit list request or manual entry | [Compatibility](https://docs.together.ai/docs/inference/openai-compatibility), [models](https://docs.together.ai/reference/models) |

The editable review shows the exact address before saving or testing. Switching service or region clears credentials and dependent model information; returning to the same selection preserves the draft. There is no automatic region or provider fallback.

Model discovery is an explicit request using the selected credential. For endpoints without an integrated, verified listing route, setup goes straight to manual model entry instead of sending a guessed request. Alibaba's native listing format is not the Chat `data` list; its dedicated API is not integrated here. Together's top-level array is a distinct supported listing shape. Listing a model does not establish tool, image or reasoning support; unknown capabilities remain for you to confirm.

## Subscription and coding-plan accounts

These presets target standard API access. A chat subscription is not an API credential. Coding plans can use a different key, address and set of permitted clients; use **Custom configuration** with the provider's current instructions. Do not move a key between regional services or plans by guessing a hostname.

In particular, Kimi Code and the Kimi open platform are separate systems; Zhipu/Z.AI distinguish standard and coding endpoints; Alibaba coding-plan credentials are distinct; MiniMax subscription keys and ordinary API keys should not be treated as interchangeable. The Volcengine documentation is changing plan naming and routes, so this delivery does not offer a guessed plan preset.

## Verification boundaries

This is not a guarantee that every model hosted by each provider works. Interfaces can differ in optional parameters, reasoning blocks and tool-call extensions, even when both are called Chat Completions. The implementation must preserve applicable reasoning and signatures through a complete tool round trip; merely displaying a provider name or receiving a short text reply does not establish that.

Local coverage includes saving every preset in both languages without requests, isolating regional credentials and pinning discovery paths. Synthetic streamed fixtures exercise the ledger and the second authenticated HTTP request for [Gemini signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures), [OpenRouter reasoning details](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens), [Mistral typed content](https://docs.mistral.ai/studio/conversations/reasoning), [Together reasoning fields](https://docs.together.ai/docs/inference/chat/reasoning) and [MiniMax inline thinking](https://platform.minimax.io/docs/api-reference/text-openai-api). These fixtures do not substitute for a live account or cover every hosted model.

Thinking controls are not uniform across services. Known official Chat endpoints no longer inherit DeepSeek's `thinking` field. Their configured model effort levels guide the request; if the requested control is not configured, including an unsupported disable request, the adapter reports that it is using the provider default. In that case even a background summarization request does not guarantee disabled thinking. Custom proxy endpoints retain their existing mapping and need their own compatibility check.

Source research and local validation are recorded separately in issue #95. No paid request, credential acquisition, installation or release is implied by this page. For unsupported authentication, native APIs or plan-specific behavior, keep using an explicitly documented custom connection or report the concrete gap.

See [model configuration](model-configuration.md) for editing entries and [first use](getting-started.md) for the complete startup path.
