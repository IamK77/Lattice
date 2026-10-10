# Lattice 文档

[产品首页](../README.zh-CN.md) · [English](README.md)

按你正在做的事情选择入口。用 Lattice 处理软件项目，不需要先学习它的运行时实现。

## 使用 Lattice

| 我想要…… | 从这里开始 |
| --- | --- |
| 获取程序、验证下载、升级或回退 | [安装与升级](installation.zh-CN.md) |
| 连接自己的账号，提出第一个问题 | [第一次对话](getting-started.zh-CN.md) |
| 继续之前的工作 | [恢复对话](getting-started.zh-CN.md#4-继续之前的对话) |
| 了解授予了哪些文件和数据访问权限 | [数据与权限](getting-started.zh-CN.md#数据与权限) |
| 解决问题或求助 | [故障排查](troubleshooting.zh-CN.md) · [支持说明（英文）](../SUPPORT.md) |
| 私密报告漏洞，避免泄露秘密 | [安全报告（英文）](../SECURITY.md) |

## 配置与定制

选择你要调整的内容：连接模型、添加项目约定，或配置浏览器、桌面等可选工具。

- [模型配置](model-configuration.zh-CN.md)：连接服务、手写模型文件、能力、容量、语言与凭证。
- [项目约定](getting-started.zh-CN.md#5-加入项目约定)：用项目规则指导 agent。
- [可选工具](getting-started.zh-CN.md#可选工具)：浏览器和代码导航的使用前提。
- [桌面配置](desktop.zh-CN.md)：可选的 macOS 驱动和权限。
- [JavaScript/Ink 客户端](../clients/ink/README.zh-CN.md)：另一种客户端的启动和操作。

想编写自己的工具或改变 Lattice 的运行方式，请看下面的“开发与维护”。

## 开发与维护

[开发参考导航](development.zh-CN.md)说明文档各自的职责，并区分当前实现、历史原型与验收记录。

| 任务 | 参考 |
| --- | --- |
| 理解运行时及其设计边界 | [架构总览](architecture-overview.zh-CN.md) · [架构决策](architecture-decisions.zh-CN.md) |
| 用自己熟悉的语言编写部件 | [示例](../examples/) · [自造工具演示](tool-building-walkthrough.zh-CN.md) |
| 组装运行时或迁移自定义装配 | [装配配置](assembly-configuration.zh-CN.md) · [能力迁移（英文）](capability-split-migration.md) |
| 实现部件接口 | [契约导航](contracts/README.zh-CN.md) · [JSON Schema](../schemas/) |
| 修改首次配置前端 | [引导实现与验证（英文）](setup-development.md) |
| 实现或测试可选集成 | [Ink 协议与测试](../clients/ink/development.zh-CN.md) · [桌面后端与测试](desktop-development.zh-CN.md) |
| 贡献一项小范围改动 | [参与贡献（英文）](../CONTRIBUTING.md) · [分支与 PR 流程](workflow.zh-CN.md) |
| 维护依赖、准备或执行发布 | [维护者说明（英文）](../MAINTAINERS.md) · [维护者手册](maintainer-handbook.zh-CN.md) |

完整运行时装配与安装增补不是同一份文件，装配参考解释两者的职责。普通模型账号配置不需要修改它们。

免模型的部件演示可在源码目录运行：

```bash
cargo run --locked --example heartbeat
```

这个示例用预设回复展示部件间的事件传递。连接真实模型请看[第一次对话](getting-started.zh-CN.md)，开发检查见[引导开发说明（英文）](setup-development.md#developer-checks-without-a-provider)。

当前使用说明从本页进入，历史原型和验收记录收在[开发参考](development.zh-CN.md)中。深层设计文档主要使用中文，英文入口标明了语言。文件名使用英文，中文文档以 `.zh-CN.md` 区分；[路径迁移表](path-migrations.json)记录旧名称。
