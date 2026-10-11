# 配置向导中的模型服务商

[文档导航](../README.zh-CN.md) · [English](model-providers.md)

向导提供的是连接预设，不是账号或随软件赠送的模型目录。先选服务商，再选与你的 API 账号对应的服务或地区。通过**修改连接**可以选择另一种已支持的格式、修改地址。切换格式时，预设地址会跟着切换；你自己填写的地址则保留。切换服务或地区会清掉凭据和依赖它的模型资料。不会自动回退到别的服务商或地区。

## 格式与默认选择

**Chat** 指 OpenAI Chat Completions，**Messages** 指 Anthropic Messages，**Responses** 指 OpenAI Responses。格式同名，不代表鉴权、选填参数和计费方式相同。下表排在第一位的是**新建条目时的 Lattice 默认项**，不冒充每家服务商的官方推荐。已有条目不会自动迁移。

本表记录 #95 之后、#98 中逐家核对官方资料的结果。**没有使用真实账号或发起付费推理验证。**“只确认此格式”仅指这三种适配件之内，不代表服务商没有其他接口。

| 服务商 | 服务或地区 | 可选格式，默认在前 | 默认依据与官方资料 |
|---|---|---|---|
| OpenAI | 标准 API | **Responses**、Chat | [官方迁移建议](https://developers.openai.com/api/docs/guides/migrate-to-responses) |
| Anthropic | 标准 API | **Messages**、Chat | 原生接口；[Chat 兼容指南](https://platform.claude.com/docs/en/api/openai-sdk)建议生产环境使用原生接口，并列明兼容限制 |
| DeepSeek | 标准 API | **Chat**、Messages、Responses | Lattice 保留原默认项；官方的 [Messages](https://api-docs.deepseek.com/guides/anthropic_api/) 和 [Responses](https://api-docs.deepseek.com/guides/responses_api/) 文档证明可选格式，不证明全局优先顺序 |
| 阿里云百炼／通义千问 | 北京、新加坡、弗吉尼亚 | **Chat**、Messages、Responses | Lattice 保留 Chat。[地址](https://www.alibabacloud.com/help/en/model-studio/base-url)、[Messages](https://www.alibabacloud.com/help/en/model-studio/anthropic-api-messages)、[Responses](https://www.alibabacloud.com/help/en/model-studio/qwen-api-via-openai-responses) |
| 月之暗面／Kimi | 中国站、全球站 | **Chat**、Messages、Responses | Lattice 保留 Chat；官方总览列明各格式，没有给出统一优先项：[中国站](https://platform.kimi.com/docs/api/overview)、[全球站](https://platform.kimi.ai/docs/api/overview) |
| 智谱／Z.AI／GLM | 智谱中国站、Z.AI 全球站 | 中国站：**Chat**、Responses；全球站：**Chat** | 标准账号默认仍用 Chat。[中国站 Responses](https://docs.bigmodel.cn/cn/guide/develop/responses/introduction)、[Messages 的账号计费限制](https://zcode.z.ai/en/docs/configuration)，限制详见下文 |
| MiniMax | 中国站、全球站 | **Messages**、Chat、Responses | [官方推荐 Messages](https://platform.minimax.io/docs/guides/text-generation)；[中国站 Messages](https://platform.minimax.cn/docs/api-reference/text-anthropic-api)、[Responses](https://platform.minimax.io/docs/api-reference/responses-create) |
| 火山方舟 | 北京标准 API | **Responses**、Chat、Messages | [官方推荐 Responses](https://docs.volcengine.com/docs/ark/responses-api-text-generation?lang=zh)；[Messages](https://docs.volcengine.com/docs/ark/messages-api?lang=zh)、[Responses 请求类型](https://github.com/volcengine/ark-runtime-python/tree/main/src/arkruntime/types/responses) |
| 硅基流动／SiliconFlow | 中国站、全球站 | **Chat**、Messages | [官方推荐 Chat，并说明地区等价入口](https://github.com/siliconflow/siliconflow-open-source-tasks)；[Messages](https://docs.siliconflow.cn/docs/api/messages-post) |
| OpenRouter | 标准 API | **Chat**、Messages、Responses | Lattice 保留 Chat；[Messages 文档](https://openrouter.ai/docs/api/api-reference/anthropic-messages/create-a-message)、[Responses 文档](https://openrouter.ai/docs/api/api-reference/responses/create-a-response)没有确立统一首选格式 |
| xAI | 全球入口、美国区域入口 | 全球：**Responses**、Chat；美国区域：**Responses** | [官方推荐 Responses](https://docs.x.ai/developers/model-capabilities/text/comparison)、[地区说明](https://docs.x.ai/developers/advanced-api-usage/regions)；不提供已弃用的 Messages，也没有把未经单独确认的美国区域 Chat 加进去 |
| Google Gemini | 开发者 API | **Chat** | [这三种适配件中只确认此格式](https://ai.google.dev/gemini-api/docs/openai)。Google 对原生 Gemini API 的推荐，不等于推荐这个兼容入口 |
| Mistral | 标准、欧盟、美国区域入口 | **Chat** | 这三种适配件中只确认此格式；[地区说明](https://docs.mistral.ai/inference/regional-inference)。Mistral Conversations 不是 OpenAI Responses |
| Groq | 标准 API | **Chat**、Responses | Lattice 保留 Chat；[Responses 文档标为测试版](https://console.groq.com/docs/responses-api) |
| Together AI | 标准 API | **Chat** | [这三种适配件中只确认此格式](https://docs.together.ai/docs/inference/openai-compatibility) |

百炼预设保留官方仍支持的共享 `dashscope` 地址。阿里云建议生产环境使用工作空间专属域名；请在**修改连接**中填写你自己的工作空间地址，不猜工作空间编号。各地区的密钥和模型仍然独立。

GLM Messages 故意**不作为普通标准账号预设**。其 `/api/anthropic` 路由受编程套餐、白名单和账号历史计费规则影响，套餐过期也不意味着恢复普通余额计费。请根据账号对应的官方说明使用自定义配置。中国站 Responses 使用 `/api/v1`，不是 Chat 的 `/api/paas/v4`。本轮没有确认相应的全球站 Responses 地址，不通过换域名猜一个出来。

## 地址、鉴权与模型列表

保存或测试前，总览显示准确地址。换格式不只是换标签：MiniMax、Kimi 的 Messages 基址带 `/anthropic`，百炼带 `/apps/anthropic`，方舟带 `/api/compatible`，OpenRouter 带 `/api`。适配件随后拼接 `/v1/messages`。硅基流动 Messages 从域名根开始，不能再预先带一个 `/v1`。

已识别的 MiniMax、Kimi、硅基流动和 OpenRouter Messages 使用 Bearer 鉴权；百炼和方舟使用 `x-api-key`。原生 Anthropic 和既有的 DeepSeek Messages 保持 `x-api-key`。向导的连接测试和运行时共用这一判断。Chat、Responses 使用 Bearer。模型列表按它自己的已确认路径和鉴权获取，不从生成接口格式猜路径：例如 Anthropic 的 Chat 选项仍使用原生、分页的模型目录。

获取模型列表仍须明确选择。OpenAI、Anthropic、DeepSeek、Gemini、Kimi、MiniMax、硅基流动、OpenRouter、xAI、Mistral、Groq、Together 已接入列表，也始终可以手填。MiniMax 使用其已记录的 [OpenAI 格式目录](https://platform.minimax.io/docs/api-reference/models/openai/list-models)。百炼、GLM、方舟尚未接入并验证适用的列表路线，仍然手填。Together 的顶层数组与常见的 `data` 数组分别处理。出现在列表中，不证明模型支持图片、工具或思考。

## 兼容边界

- 不把 OpenAI 的选填字段整包照搬。例如 Groq 不接受获取加密推理的 include 或提示缓存键。OpenRouter、方舟保留文档已确认的加密 include 和缓存键；百炼、GLM 不发送未经确认的加密 include。Kimi 保留已支持的网页搜索 include，只去掉加密推理这一项。
- MiniMax、Kimi 的 Responses 请求结构没有声明 `store`，所以运行时和连接测试都不发送它。回复示例里的 `store:false` **不等于数据保留承诺**。其他已配置的 Responses 路线仍显式请求 `store:false`。本地携带完整历史，也不证明服务端不保留数据。
- 思考控制取决于**入口和型号配置中的档位**。没有声明的控制，包括不能确认的关闭操作，会提示采用服务默认行为；后台生成纪要时也可能继续思考。硅基流动 Messages 的 token 预算接口，不会被悄悄替换成不支持的 effort 字段，也不会凭空猜一个预算。自定义代理保持原有映射，需要另行验证。
- 已识别的兼容 Responses 服务使用 Lattice 的普通文字纪要，不套用 Codex 专用的触发式原生压缩。支持 `/responses` 本身不证明支持该扩展。
- 新支持的兼容 Messages 入口会保留真实思考和签名，不凭空补一个空思考块。Responses 保留原始输出项用于回传，并把明文思考呈现出来，包括 GLM 使用单个对象表示内容的形式。这些本地性质不等于每家托管的每个型号都能接收所有历史项。

MiniMax Responses 文档还有一个明确缺口：输出思考使用 `content`，输入结构却描述 `summary`。Lattice 不把两者悄悄改写成同一种东西。其完整思考回传和准确的流式结束行为仍需真实端点验证；不能接受这一不确定性时，使用官方推荐的 Messages 路线。

## 订阅账号与验证

这些预设针对标准 API。聊天会员不等于 API 凭据。Kimi Code、百炼编程套餐、GLM 编程套餐和 MiniMax 订阅可能使用不同凭据、地址或允许的客户端。请按服务商当前说明使用**自定义配置**，不要猜域名、把密钥挪到另一套服务。

本地测试固定了各地区的格式和默认项、替代格式的 POST 路径；逐个选项在中英文向导中完成不发请求的保存；检查地区凭据隔离，并用合成回复跑带鉴权的 HTTP 工具往返。新增 Messages 测试检查各入口的请求头与签名回传；Responses 测试检查选填字段、明文思考、调用编号和工具返回。既有 Chat 测试覆盖 [Gemini 签名](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures)、[OpenRouter 推理](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)、[Mistral 分块内容](https://docs.mistral.ai/studio/conversations/reasoning)、[Together 思考](https://docs.together.ai/docs/inference/chat/reasoning)和 [MiniMax 内联思考](https://platform.minimax.io/docs/api-reference/text-openai-api)。

合成回复只证明测试实际覆盖的行为。短连接测试不等于完整兼容性考试。#98 分别记录官方资料证据与本地验证，真实型号、真实账号的验收仍未完成。本页不意味着取得了凭据、发起付费请求、安装程序或发布版本。

参见[模型配置](model-configuration.zh-CN.md)和[首次使用](getting-started.zh-CN.md)。
