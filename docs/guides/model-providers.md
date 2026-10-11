# Model providers in setup

[Documentation](../README.md) · [简体中文](model-providers.zh-CN.md)

Setup offers connection presets, not accounts or a bundled model catalog. Choose a provider and the service/region belonging to your API account. **Edit connection** lets you choose another supported format and edit its address. Switching formats follows the matching preset address unless you have entered a custom address; a custom address is preserved. Switching service or region clears credentials and dependent model information. There is no automatic provider or regional fallback.

## Formats and defaults

**Chat** means OpenAI Chat Completions, **Messages** means Anthropic Messages, and **Responses** means OpenAI Responses. A shared format name does not imply identical authentication, optional parameters or billing. The first format below is the **Lattice default for a new entry**, not a claim that every provider recommends it. Existing saved entries are not migrated.

The matrix records the official-documentation audit in #98, following #95. **No live-account or paid inference verification was performed.** “Only confirmed format” means only among these three adapters, not that the provider has no other API.

| Provider | Service / region | Formats offered, default first | Default rationale and official sources |
|---|---|---|---|
| OpenAI | Standard API | **Responses**, Chat | [Official migration recommendation](https://developers.openai.com/api/docs/guides/migrate-to-responses) |
| Anthropic | Standard API | **Messages**, Chat | Native API; the [Chat compatibility guide](https://platform.claude.com/docs/en/api/openai-sdk) recommends native integration for production and documents limitations |
| DeepSeek | Standard API | **Chat**, Messages, Responses | Lattice retains its existing default; [Messages](https://api-docs.deepseek.com/guides/anthropic_api/) and [Responses](https://api-docs.deepseek.com/guides/responses_api/) are alternatives, not evidence of an overall recommended default |
| Alibaba Cloud Model Studio / Qwen | Beijing, Singapore, Virginia | **Chat**, Messages, Responses | Lattice retains Chat. [Addresses](https://www.alibabacloud.com/help/en/model-studio/base-url), [Messages](https://www.alibabacloud.com/help/en/model-studio/anthropic-api-messages), [Responses](https://www.alibabacloud.com/help/en/model-studio/qwen-api-via-openai-responses) |
| Moonshot / Kimi | China, global | **Chat**, Messages, Responses | Lattice retains Chat; the official overviews list the alternatives without an overall preference: [China](https://platform.kimi.com/docs/api/overview), [global](https://platform.kimi.ai/docs/api/overview) |
| Zhipu / Z.AI / GLM | BigModel China; Z.AI global | China: **Chat**, Responses; global: **Chat** | Chat remains the standard-account default. [China Responses](https://docs.bigmodel.cn/cn/guide/develop/responses/introduction), [account-dependent Messages billing](https://zcode.z.ai/en/docs/configuration); see restrictions below |
| MiniMax | China, global | **Messages**, Chat, Responses | [Official Messages recommendation](https://platform.minimax.io/docs/guides/text-generation); [China Messages](https://platform.minimax.cn/docs/api-reference/text-anthropic-api), [Responses](https://platform.minimax.io/docs/api-reference/responses-create) |
| Volcengine Ark | Beijing standard API | **Responses**, Chat, Messages | [Official Responses recommendation](https://docs.volcengine.com/docs/ark/responses-api-text-generation?lang=zh); [Messages](https://docs.volcengine.com/docs/ark/messages-api?lang=zh), [Responses request types](https://github.com/volcengine/ark-runtime-python/tree/main/src/arkruntime/types/responses) |
| SiliconFlow | China, global | **Chat**, Messages | [Official Chat recommendation and regional equivalents](https://github.com/siliconflow/siliconflow-open-source-tasks); [Messages](https://docs.siliconflow.cn/docs/api/messages-post) |
| OpenRouter | Standard API | **Chat**, Messages, Responses | Lattice retains Chat; [Messages reference](https://openrouter.ai/docs/api/api-reference/anthropic-messages/create-a-message), [Responses reference](https://openrouter.ai/docs/api/api-reference/responses/create-a-response) do not establish an overall preferred format |
| xAI | Global; US regional | Global: **Responses**, Chat; US: **Responses** | [Official Responses recommendation](https://docs.x.ai/developers/model-capabilities/text/comparison), [regions](https://docs.x.ai/developers/advanced-api-usage/regions); deprecated Messages is not offered, and US Chat was not independently confirmed |
| Google Gemini | Developer API | **Chat** | [Only confirmed format among these adapters](https://ai.google.dev/gemini-api/docs/openai). Google's native Gemini API recommendation is not a recommendation for this compatibility route |
| Mistral | Standard, EU, US regional | **Chat** | Only confirmed format among these adapters; [regions](https://docs.mistral.ai/inference/regional-inference). Mistral Conversations is not OpenAI Responses |
| Groq | Standard API | **Chat**, Responses | Lattice retains Chat; [Responses is documented as beta](https://console.groq.com/docs/responses-api) |
| Together AI | Standard API | **Chat** | [Only confirmed format among these adapters](https://docs.together.ai/docs/inference/openai-compatibility) |

Qwen presets retain the documented shared `dashscope` addresses. Alibaba recommends workspace-specific domains for production; enter your workspace's documented address in **Edit connection**, rather than guessing a workspace ID. Regional keys and models remain separate.

GLM Messages is deliberately **not a general standard-account preset**. Its `/api/anthropic` route has coding-plan/allowlist and historical-account billing restrictions, even after a plan expires. Use the provider's account-specific instructions with Custom configuration. China Responses uses `/api/v1`, not the Chat `/api/paas/v4` base. A corresponding global Responses endpoint was not confirmed and is not inferred.

## Addresses, authentication and model lists

The review displays the exact base before saving or testing. Choosing another format changes more than its label: for example, MiniMax and Kimi Messages use `/anthropic`, Qwen uses `/apps/anthropic`, Ark uses `/api/compatible`, and OpenRouter uses `/api`. The adapter appends `/v1/messages`. SiliconFlow Messages starts from the host root, not a base already ending in `/v1`.

Known MiniMax, Kimi, SiliconFlow and OpenRouter Messages routes use Bearer authentication; Qwen and Ark use `x-api-key`. Native Anthropic and the existing DeepSeek Messages route keep their `x-api-key` behavior. Setup's probe and runtime share this choice. Chat and Responses use Bearer authentication. A catalog request follows its own documented route and authentication, not a path guessed from the generation format: Anthropic's Chat option still uses the native paginated model catalog.

Model discovery remains an explicit action. OpenAI, Anthropic, DeepSeek, Gemini, Kimi, MiniMax, SiliconFlow, OpenRouter, xAI, Mistral, Groq and Together offer integrated list requests; manual entry is always available. MiniMax uses its documented [OpenAI-format catalog](https://platform.minimax.io/docs/api-reference/models/openai/list-models). Qwen, GLM and Ark remain manual because no suitable listing route has been integrated and verified. Together's top-level array is handled separately from the common `data` array. A listed model does not establish image, tool or thinking capabilities.

## Compatibility limits

- Optional OpenAI fields are not inherited indiscriminately. For example, Groq does not accept the encrypted-reasoning include or prompt cache key. OpenRouter and Ark retain their documented encrypted include and cache key; Qwen and GLM do not receive an unverified encrypted include. Kimi keeps supported web-search includes while omitting the encrypted-reasoning include.
- MiniMax and Kimi's Responses request schemas do not declare `store`, so both runtime and the connection probe omit it. A response example containing `store:false` is **not a retention guarantee**. Other configured Responses routes retain the explicit `store:false` request. Carrying full local history does not establish a provider's data-retention policy.
- Thinking controls depend on the endpoint **and the model's configured effort levels**. Undeclared controls, including an unconfirmed disable request, produce a notice and use the service default; thinking may remain enabled during summarization too. SiliconFlow Messages' token-budget interface is not silently replaced with an unsupported effort field or an invented token budget. Custom gateways retain their existing mapping and require their own check.
- Known compatible Responses services use Lattice's ordinary text summarization, not the separate Codex trigger-compaction protocol. Supporting `/responses` alone does not prove support for that compaction extension.
- Messages keeps actual thinking and signatures through tool history without inventing empty thinking blocks for the newly supported compatible endpoints. Responses retains original output items for replay and exposes plaintext reasoning, including GLM's single-object content shape. These local properties do not prove that every hosted model accepts every historical item.

MiniMax's Responses documentation leaves a specific gap: its reasoning output uses `content`, while its input schema describes `summary`. Lattice does not silently rewrite one into the other. Its complete reasoning round trip and exact SSE terminal behavior still require live verification; use the recommended Messages route when that uncertainty is unacceptable.

## Subscription accounts and verification

These presets target standard API access. A chat subscription is not an API credential. Kimi Code, Alibaba coding plans, GLM coding plans and MiniMax subscriptions can use different credentials, endpoints or permitted clients. Follow the provider's current instructions using **Custom configuration**; do not move a key between services by guessing a hostname.

Local tests pin the per-region protocol/default matrix and alternate POST paths, save every offered route in both languages without requests, isolate regional credentials, and exercise synthetic authenticated HTTP tool round trips. New Messages fixtures check endpoint-specific headers and signature replay; Responses fixtures check optional fields, plaintext reasoning, call IDs and returned tool results. Earlier Chat fixtures cover [Gemini signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures), [OpenRouter reasoning](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens), [Mistral typed content](https://docs.mistral.ai/studio/conversations/reasoning), [Together reasoning](https://docs.together.ai/docs/inference/chat/reasoning) and [MiniMax inline thinking](https://platform.minimax.io/docs/api-reference/text-openai-api).

Synthetic replies prove only the behavior exercised by those fixtures. A short connection test is not a full compatibility exam. Issue #98 records documentation evidence and local validation separately; live model/account acceptance remains outstanding. No credential acquisition, paid request, installation or release is implied.

See [model configuration](model-configuration.md) and [first use](getting-started.md).
