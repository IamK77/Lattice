# 契约二：核心事件类型

[契约导航](README.zh-CN.md) · [开发参考](../development/README.zh-CN.md)

核心自带的信纸分输入、模型、工具、控制四个家族，另有对外输出类。下表列主要事件与 payload 要点，机器可读的字段约束在 [schemas/payloads](../../schemas/payloads/)，当前 Rust 类型见 [core_events.rs](../../src/contracts/core_events.rs)。

## 输入类

| 类型 | 含义 | payload 要点 |
|---|---|---|
| `core.input.user_message` | 用户说了话 | `text`、`images`（可选：随这句话附上的图片，每项 `{file, mediaType, bytes?, name?}`。**只记引用不记字节**——流水是每行一个 JSON、逐行读的，一张内联的图会把几兆 base64 摊在一行上，而每个读它的人都得走过去。`file` 是流水旁边那个文档目录里的裸文件名、按内容取名，永远不是路径。缺席即没有，这也是有这个字段之前的全部记录说的话） |
| `core.input.external` | 外部系统注入信息 | `channel`（来源标识）、`data` |

## 模型类

| 类型 | 含义 | payload 要点 |
|---|---|---|
| `core.model.call_started` | 一次模型调用开始 | `model`、`input`（**引用清单 + 指纹**：材料零件共三种——指针 `{"event": id}`（取原文翻译）、内联 `{"inline": 消息}`（原样插入，已是某方言的成品）、**摘要件** `{"digest": {"of": id, "text": 一行便条}}`（呈现便条而非原文，便条须含可回查的事件编号；由上下文关卡产出，各方言适配件自行渲染——取舍不懂方言、方言不懂取舍）。三种零件均按统一规则参与 sha256 指纹（指针哈希编号、摘要件哈希其内容、内联哈希消息；正本实现在 model_common）。指针避免重复抄原文，但每次请求仍记录完整材料清单，不能据此保证整本流水线性增长；审计解引用即还原"模型当时看到了什么"——摘要视图不落独立事件，随本事件入账）、`tools`（工具声明清单）、`system`（可选：本次调用实际使用的系统提示词全文——由上下文关卡拼装填入，适配件优先采用、缺席时退回自身配置。它入账使"模型当时被告知了什么"完整可审计） |
| `core.model.call_completed` | 模型调用结束 | `status: ok/error/cancelled`（取消也是普通完成态）、`text`、`toolCalls`（工具调用请求，每项带供应商签发的 `id`）、`error`（**判断字段对象**，见下）、`usage`（含缓存写入/命中统计）、**`reasoning`**（推理内容，见下）、`purpose`（可选：从请求原样带回，标明这次调用服务的是后台用途而非对话——让只拿到这一条事件、没有流水可回溯因果的观察者也能分辨） |

### 推理内容（`reasoning`）

思考型号在给出答案前先产出的思维链，以**有序零件清单**入账，一枚零件对应供应商的一个块。

| 字段 | 含义 |
|---|---|
| `kind: "text"` | 人和 agent 都读得懂的思考文字，放在 `text` 里 |
| `kind: "hidden"` | 供应商加密封存的块（如 Anthropic 的涂黑思考）：必须携带，读不了 |
| `opaque` | 可选。这一块里**只有产它的那家方言看得懂**的东西，原样保管、核心从不解释。Anthropic 的逐块签名放这儿 |

三条规矩：

1. **签名住在它所签的那一块里面**，不另开平行清单。否则回传时想把签名配回原文只能靠位置对齐，中间少一块就全错位。
2. **回传与否是方言的事，核心不参与**。核心只保存零件；适配件决定如何编码。当前适配策略是在工具调用轮保留推理，即使没有可读正文，也发送该方言的空推理表示，而不是省略整个字段或块。合法的空正文可能来自关闭思考、不提供思考的模型，或中途切换到只能读取封存推理的方言。

   OpenAI 兼容的 DeepSeek 工具轮使用 `reasoning_content`；Anthropic 格式的工具轮将思考块放在内容前部，有原始签名时带回签名，没有可读块时保留空块。非工具轮不因这项策略强制附加推理。这里只描述当前适配行为，不把某个兼容端点的接受规则当作所有服务的共同保证。

   **跨方言兼容性有边界**：可读正文可以迁移，目标方言没有对应位置的签名不迁移，也不伪造新签名。DeepSeek 的 Anthropic 格式端点与 Anthropic 官方端点不能视为同一实现；官方端点对无签名块的接受行为尚无完整验证，封存的 `redacted_thinking` 也不是所有兼容端点都支持。跨端点切换可能因此被拒，不能保证已有推理历史总能无损续接。
3. **推理与它那一轮的工具调用同生共死**。这条不需要额外执法：上下文关卡做取舍的单位是"指向某条事件的指针"，只能整条留下或整条换成摘要件，没有能力伸进事件里单独摘掉推理。

考卷只判形状不判内容：产不产推理不影响合格，但**可读零件必须真带文字、封存零件必须真带得回去的东西**。当前[信纸 Schema](../../schemas/payloads/model_call_completed.json)没有按 `kind` 分别声明 `text` 或 `opaque` 必填；不能只凭通过该 Schema 就认定这两条满足。

## 工具类

| 类型 | 含义 | payload 要点 |
|---|---|---|
| `core.tool.exec_started` | 工具执行开始 | `call`（源调用 id，随行全程）、`tool`、`arguments` |
| `core.tool.exec_completed` | 工具执行结束 | `call`（源调用 id）、`status: ok/error/cancelled`、`result`、`error`、可选 `modelText`、可选 `continuation` |

工具声明（`ToolDecl`）：`name`、`description`、`parameters`（JSON Schema）、`effects`（**作用面**：reads/writes/network/executes/reversible——policy 只依据它判断，从不硬编码工具名；未申报一律按最危险处理）。工具真正干活的代码不属于契约——执行体可以在任何地方，只要这三份数据能送到。

工具结果可显式提供 `result.latticeImages`：PNG 文档引用数组，每项为 `{file, mediaType: "image/png", bytes?}`，文件在流水旁、按内容命名。这是**选择加入的附件约定**，不是把任意业务字段 `images` 猜成图片。当前三条模型接口路径都读取这个字段，但载体不同：Responses 放进原 `function_call_output.output` 的内容数组；Anthropic 放进原 `tool_result.content` 的图片块；Chat Completions 保留文字工具回复，并在连续工具回复组之后追加用户图片消息。共同读取入口见 [media_document.rs](../../src/components/media_document.rs)，三种发送形式分别见 [responses_media.rs](../../src/components/responses_media.rs)、[anthropic_model.rs](../../src/components/anthropic_model.rs) 和 [openai_model.rs](../../src/components/openai_model.rs)。读取时核验文件身份、字节数及 PNG 内容，不跟随图片文件的符号链接。普通工具结果仍可以是任意 JSON。

工具还可在完成事件的 **payload 顶层**提供 `modelText`（非空字符串，最多 8192 个字符），作为给模型的简短回执。它不是业务结果里的同名字段。完整 `result` 仍照常入账、供界面和回查使用；只有 `status=ok` 才使用简短回执，错误和取消仍呈现原始结果。适配件明确标记“省略了详情”，并附上原事件编号，原文通过流水文件读取。未提供时完全沿用原渲染；图片附件仍从完整结果读取。提供者负责保留下一步判断所需的事实，不能把截断、部分完成或失败包装成完整成功。

成功的启动回执还可在 **payload 顶层**提供 `continuation: "wait"`，表示“目前只有启动信息，请等新的输入再继续”，不是业务 `result` 中的同名字段。这次工具调用已经答复，后续执行结果由提供者通过普通输入（通常是 `core.input.wake`）交付，不得再补第二份工具答复。提供者必须安排后续通知，不能借此隐藏错误或取消。

随货主循环仅在整批工具答复都是这种成功等待回执、且没有尚未交给模型的新输入时暂停；普通结果、错误、取消均正常触发后续提问。暂停发出有原因、以该批回执为因果的 `loop.waiting` 决策事件，不冒充 `core.control.turn_completed`。等待期间不调用模型；用户消息或后台唤醒照现有输入规则恢复。先到的唤醒不能被后到的回执压住，重复回执不产生第二次暂停。这个约定不识别工具名、不改变内核派信，也不要求其他主循环采用同一策略；旧消费者可忽略该可选字段。

## 输出类

| 类型 | 含义 | payload 要点 |
|---|---|---|
| `core.output.reply` | agent 的对外回复——前端显示的正是它 | `text`、`cancelled`、`error`（判断字段对象） |

## 错误的判断字段（ErrorInfo，所有 error 字段共用）

不建分类树，直接把"读错误的人要做的决定"写成字段：`code`（命名空间机器码，如 `provider.rate_limit`、`tool.unknown`、`core.undeclared_emission`）、`message`（一句人话）、`blame`（谁的锅：request/provider/environment）、`retryable`（+可选 `retryAfterMs`）、`transient`（是否暂时）。核心自记的 `core.control.error` 带 `code`+`message`+`detail`。

## 控制类

| 类型 | 含义 | payload 要点 |
|---|---|---|
| `core.control.interrupted` | 被打断 | `by`（谁发起） |
| `core.control.error` | 没有部件接住的错误 | `message`、`detail` |
| `core.control.turn_started` / `turn_completed` | 一轮工作的边界（**纯边界，不携带内容**——内容走输出类） | — |
| `core.control.component_crashed` | 部件崩溃，由核心代记 | `component`、`processing`（崩溃时正在处理的事件 id，保证因果链不断） |

## 扩展规则

部件自带的新信纸类型（如未来调度层的 `sched.task.*`）在部件自述中注册，装进同一种信封、汇入同一条流水。核心永远不需要因为新事件类型而改动。
