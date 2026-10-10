# 契约参考

[English](README.md) · [开发参考](../development.zh-CN.md) · [用户文档](../README.zh-CN.md)

这里规定部件之间交换的数据，不是普通模型账号的配置教程。六份详细契约目前使用中文；外部部件可以用任意语言实现。

## 按要实现的边界选择

| 任务 | 阅读 | 机器可读结构与实现定位 |
| --- | --- | --- |
| 读取或发出事件 | [一、事件信封](01-envelope.zh-CN.md) | [envelope.json](../../schemas/envelope.json) · [event.rs](../../src/contracts/event.rs) |
| 处理核心请求、结果或控制事件 | [二、核心事件类型](02-core-events.zh-CN.md) | [信纸 Schema](../../schemas/payloads/) · [core_events.rs](../../src/contracts/core_events.rs) |
| 声明部件、口与工具 | [三、部件自述](03-component-manifest.zh-CN.md) | [component_manifest.json](../../schemas/component_manifest.json) · [component.rs](../../src/contracts/component.rs) |
| 描述实例与显式接线 | [四、装配说明书](04-assembly-manifest.zh-CN.md) | [assembly_manifest.json](../../schemas/assembly_manifest.json) · [assembly.rs](../../src/contracts/assembly.rs) |
| 实现可替换的职责 | [五、标准口型](05-standard-interfaces.zh-CN.md) | 当前标准口型目录在 [profile.rs](../../src/contracts/profile.rs)，行为考卷在 [conformance.rs](../../src/conformance.rs) |
| 用其他进程或语言运行部件 | [六、跨进程桥](06-process-bridge.zh-CN.md) | 按行传输的协议见正文；其中的事件使用上面的信封与信纸契约 |

编写外部工具时，先看信封、部件自述、相关口型与跨进程桥，再看[部件示例](../../examples/components/)。示例只证明某条接入路径，不代替完整契约。

## 规范与实现

- 人读契约解释含义和行为义务，已经发布的 JSON Schema 规定各自覆盖的文档结构；上表不意味着每一种运行时行为都有独立 Schema。
- Rust 类型、校验器与标准口型目录用于定位当前实现，Rust 不是外部部件的语言要求。当前口型目录住在 `profile.rs`，并没有另行发布一份覆盖全部口型的 Schema。
- [Schema 一致性测试](../../tests/schema_canon.rs)比较其覆盖的 Schema、类型序列化与示例；行为考卷另外检查执行。它们不证明供应商兼容、所有可能输入、操作系统隔离或每个第三方实现。
- 契约、Schema 与实现出现分歧时，应明确记录问题并解决，不能悄悄把契约改成代码现状，也不能只为迎合旧文字就改变运行行为。

部件自述文档把随货工具的暴露策略、安装行为与字段约束分开。装配契约描述交给内核的有效实例和接线，不承诺一份磁盘文件包含全部动态状态。

## 产品配置是另一层

聊天产品先准备基线、填入运行时位置并合并安装增补，再把普通装配交给内核。[产品装配配置](../assembly-configuration.zh-CN.md)、[product_assembly.json](../../schemas/product_assembly.json)和 [assembly_overlay.json](../../schemas/assembly_overlay.json)描述这一层，不向六份契约增加内核业务名词。

模型账号和项目约定请走[配置入口](../README.zh-CN.md#配置与定制)，不需要把本页当成使用前提。
