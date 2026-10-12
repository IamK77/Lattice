# 技能兼容范围

[English](skill-compatibility.md)

技能是包含 `SKILL.md` 和可选资源的目录。安装目录、加载说明、成功完成任务是三件事。本页记录 #59 的资料核对和首批消费行为修正，**不表示完整的技能管理已经交付**。

## 不存在一份覆盖所有宿主的“最新技能标准”

以下官方资料采集于 2026-10-12。这是观察日期，不是功能发布日期；可变网页也不能证明所有已安装客户端版本的行为。

| 层次 | 一手资料 | 定义什么 |
| --- | --- | --- |
| 开放 Agent Skills 格式 | [规范](https://agentskills.io/specification) | 目录、YAML 头部、Markdown 正文及可选资源。必填 `name`、`description`；可选 `license`、`compatibility`、`metadata` 和实验性的字符串 `allowed-tools`。 |
| Claude Code 宿主 | [技能文档](https://code.claude.com/docs/en/skills) | 更宽松的缺省元数据、扫描范围、调用开关、参数替换、动态执行等。当前页面也接受 `allowed-tools` 的 YAML 列表形式。 |
| Codex 宿主 | [技能文档](https://developers.openai.com/codex/skills)、[OpenAI 附加配置说明](https://github.com/openai/codex/blob/main/codex-rs/skills/src/assets/samples/skill-creator/references/openai_yaml.md) | 发现与调用约定；可选的 `agents/openai.yaml` 描述界面、隐式调用策略及外部工具依赖。 |
| 分发 | [Claude 市场](https://code.claude.com/docs/en/plugin-marketplaces)、[OpenAI 公开插件](https://github.com/openai/plugins) | 包和索引，一个包可能有多个技能及非技能能力，不等于 `SKILL.md` 格式。 |

开放规范仍然使用 YAML 加 Markdown，没有换成另一套文件格式。`$ARGUMENTS`、Claude 调用开关和 OpenAI 附加配置都不是基础格式的通用要求；反过来，能读取基础格式也不等于实现了这些宿主语义。

## 当前实现实际做到的

- 扫描配置的项目目录，默认依次为 `.lattice/skills`、`.agents/skills`、`.claude/skills`，不自动扫描用户主目录或祖先目录。机器上有文件不等于已安装到当前技能库。
- 校验名称和描述，缺名称时用目录名，有名称时须与目录一致。名称仍限 ASCII 小写字母、数字和单连字符；规范关于 Unicode 的文字存在歧义，本批不悄悄扩宽路径规则。
- 接受字符串和字符串列表两种 `allowed-tools` 写法。只是语法兼容，两者都不改变权限或授权路径。
- 按需加载正文和资源，保留现有大小及资源路径限制。每次显式加载正文都返回内容，不再因流水里出现过就只返回提示；历史上有，不证明压缩后的上下文里还有。
- 精确技能名优先于更长的前缀候选，应用内置命令保留优先权。Rust TUI 中 Tab 只补全名称，不删参数、折叠粘贴或图片占位符；对技能前缀直接按 Enter 会保留草稿。Ink 有技能候选时 Tab 接受首个显示项，没有时仍切换会话。未知的命令形式保留草稿并给出本地反馈。
- 保留已识别技能参数的尾部空白；展开现有的整段 `$ARGUMENTS` 时只替换消息文本，图片字段不丢。Ink 当前发送入口只有文本，不能把这些测试说成已支持 Ink 附件。

与内置命令同名的技能，尚不能通过独立结构化选择调用，仍属后续目录及调用工作。既有别名、非命令形式的普通路径保留各前端原有行为。

## 已知缺口，不要推断成完整兼容

以下仍由 #59 跟踪：

- 尚未执法 `disable-model-invocation`、`user-invocable` 和 OpenAI `policy.allow_implicit_invocation`。当前不能依赖这些声明限制调用。
- 尚未实现位置参数、命名参数、转义占位符及精确宿主参数语义；当前整段字符串替换可能误解索引形式或相似名称的占位符。专门解析器落地前不要使用这些模板。
- 宿主变量、动态命令、钩子、子代理上下文、模型覆盖及显式外部依赖，还没有完整的兼容诊断和阻断。解析或安装成功不证明这类技能能在这里正常工作。
- 无效、被遮蔽条目、不可读目录和不支持元数据，尚未形成 #59 要求的共享诊断目录和完整的人类可见呈现。
- Lattice 的 schema 是实现兼容约定，不是 Agent Skills 官方发布的 schema。当前对 `metadata` 的值和空 `compatibility` 比开放规范宽松；开放规范要求字符串元数据值、提供时非空的 compatibility。收紧及迁移需后续明确验证。
- 消费和安装仍是两个运行时部件。统一授权的准备／提交安装、外部索引、预览恢复和完整 TUI／Ink 管理属于后续批次。本次资料核对不等于实现市场，更没有启用插件的其他能力。

#59 交接稿有些未来行为刻意与 Claude 不同，例如缺少位置参数时报错；这是 Lattice 的契约选择，不是声称与宿主完全一致。已知运行依赖不足必须明说，发现命令存在不等于核对了版本。

## 验证边界

回归覆盖精确名称、显式补全、空白／折叠／图片保留、重复加载、两种工具字段写法及非法列表项。合成内核测试检查模型请求引用的展开事件；真实前端处理入口不使用供应商凭据。关键反向验证必须在旧行为下失败。这不是账户实测、完整宿主仿真、剩余管理界面的人类验收，也不代表安装了新版程序。
