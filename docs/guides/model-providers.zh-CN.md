# 配置向导中的模型服务商

[文档导航](../README.zh-CN.md) · [English](model-providers.md)

配置时可以直接选择常见服务商；如果有多个入口，再选择与你的 API 账号对应的地区，填写密钥。地址可以修改，模型 ID 也可以手填。这些是连接预设，不是随软件赠送的账号或固定模型目录。

本页对应 #95 的实现。**本轮没有进行真实账号验收。** 官方资料用于核对地址和接口格式；本地合成测试只能证明所覆盖的行为。短连接测试成功，不等于完整的工具调用兼容性已经验证。

## 标准 API 预设

下文 Chat 指 OpenAI Chat Completions。新增预设先采用这一格式；即使服务商另有其他接口，也不把它们当成可以直接互换。地区由你明确选择，不按界面语言或密钥猜测。可用模型取决于账号和所选入口。

| 服务商 | 可选入口 | 模型选择 | 官方资料 |
|---|---|---|---|
| OpenAI | 标准 API | 明确选择获取列表，或手填 | [接口文档](https://developers.openai.com/api/reference/resources/models/methods/list) |
| Anthropic | 标准 API | 明确选择获取列表，或手填 | [模型列表](https://platform.claude.com/docs/en/api/models/list) |
| DeepSeek | 标准 API | 明确选择获取列表，或手填 | [模型列表](https://api-docs.deepseek.com/api/list-models/) |
| Google Gemini | 开发者 API 的 OpenAI 兼容入口 | 明确选择获取列表，或手填 | [兼容接口与签名](https://ai.google.dev/gemini-api/docs/openai) |
| 阿里云百炼／通义千问 | 北京、新加坡、弗吉尼亚 | 从服务控制台复制模型 ID | [地址说明](https://help.aliyun.com/zh/model-studio/base-url)、[原生模型列表](https://help.aliyun.com/zh/model-studio/list-models) |
| 月之暗面／Kimi | 中国站、全球站 | 明确选择获取列表，或手填 | [中国站](https://platform.kimi.com/docs/api/overview)、[全球站](https://platform.kimi.ai/docs/api/overview) |
| 智谱／Z.AI／GLM | 智谱中国站、Z.AI 全球站 | 从服务控制台复制模型 ID | [标准与套餐入口](https://zcode.z.ai/en/docs/configuration) |
| MiniMax | 中国站、全球站 | 从服务控制台复制模型 ID | [中国站](https://platform.minimax.cn/docs/api-reference/text-openai-api)、[全球站](https://platform.minimax.io/docs/api-reference/text-openai-api) |
| 火山方舟 | 北京标准 API | 从服务控制台复制模型或推理接入点 ID | [接入说明](https://docs.volcengine.com/docs/ark/integrate-third-party-tools?lang=zh) |
| 硅基流动／SiliconFlow | 中国站、全球站 | 明确选择获取列表，或手填 | [中国站模型列表](https://docs.siliconflow.cn/docs/api/models-get)、[全球站模型列表](https://docs.siliconflow.com/en/api-reference/models/get-model-list) |
| OpenRouter | 标准 API | 明确选择获取列表，或手填 | [接口说明](https://openrouter.ai/docs/api/reference/overview) |
| xAI | 全球入口、美国区域入口 | 明确选择获取列表，或手填 | [接口](https://docs.x.ai/developers/rest-api-reference/inference)、[区域](https://docs.x.ai/developers/faq/security) |
| Mistral | 标准入口、欧盟区域入口、美国区域入口 | 明确选择获取列表，或手填 | [区域推理](https://docs.mistral.ai/inference/regional-inference) |
| Groq | 标准 API | 明确选择获取列表，或手填 | [接口说明](https://console.groq.com/docs/api-reference) |
| Together AI | 标准 API | 明确选择获取列表，或手填 | [兼容性](https://docs.together.ai/docs/inference/openai-compatibility)、[模型列表](https://docs.together.ai/reference/models) |

保存或测试之前，总览会显示准确地址。切换服务或地区会清掉原密钥和依赖它的模型资料；返回相同选择则保留草稿。不会自动回退到另一个地区或服务商。

获取列表必须由你明确选择，使用当前入口对应的密钥。没有接入并核实列表接口的服务直接提供手填，不发送猜出来的请求。百炼的原生列表格式并不是 Chat 的 `data` 数组，本轮未接入它的专用接口；Together 的顶层数组则作为另一种列表格式处理。列表里有一个模型，不证明它支持工具、图片或思考；不明确的能力仍需确认。

## 订阅和编程套餐

这些预设用于标准 API。聊天会员不等于 API 凭据。编程套餐可能使用不同的密钥、地址，并限制可用客户端；请按服务商当前说明使用**自定义配置**，不要通过猜域名把密钥从一个地区或套餐搬到另一个。

尤其要区分：Kimi Code 与 Kimi 开放平台是两套系统；智谱／Z.AI 区分普通与编程套餐地址；百炼编程套餐使用专属凭据；MiniMax 订阅密钥与普通 API 密钥不能视作通用。火山的套餐命名和路径正在调整，本轮不提供猜测的套餐预设。

## 验证范围

本页不保证每家托管的所有模型都能使用。同样叫 Chat Completions，选填参数、思考块、工具调用附加字段仍可能不同。实现需要让适用的思考和签名走完工具调用、工具返回、下一次请求这一整轮；菜单里出现名字或短请求返回文字都不能代替这项检查。

本地检查包括逐个预设的双语保存且不发请求、地区凭据隔离、模型列表的准确路径。合成流式回复还走过流水和第二次带鉴权的 HTTP 请求，覆盖 [Gemini 签名](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures)、[OpenRouter 结构化推理](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)、[Mistral 分块内容](https://docs.mistral.ai/studio/conversations/reasoning)、[Together 思考字段](https://docs.together.ai/docs/inference/chat/reasoning)和 [MiniMax 内联思考](https://platform.minimax.io/docs/api-reference/text-openai-api)。这些测试不能代替真实账号，也没有覆盖每个托管型号。

思考控制并非各家通用。已识别的官方 Chat 入口不再直接套用 DeepSeek 的 `thinking` 字段，而按配置中的型号档位生成请求。没有配置所请求的控制能力时，包括无法确认关闭思考的情况，适配件会提示采用服务默认行为。此时后台生成纪要也不能保证关闭思考。自定义代理保留原有映射，需要单独检查兼容性。

官方资料核对和本地验证分别记录在 #95 中。本页不意味着已经发起付费请求、取得凭据、安装程序或发布版本。对于未支持的认证方式、原生接口或套餐特有行为，请使用有明确依据的自定义配置，或反馈具体缺口。

修改模型条目见[模型配置](model-configuration.zh-CN.md)，完整启动流程见[首次使用](getting-started.zh-CN.md)。
