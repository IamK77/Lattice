# Lattice

**面向终端、基于事件溯源的可组合 AI Agent 运行时。**

### 先记录，再投递。

今天，用它理解代码、完成改动；明天，按自己的工作方式重新组装。模型、工具、策略、上下文管理，甚至 Agent 主循环，都是可以替换的部件。

事件先进入只追加、带因果关系的历史，再投递给下一个部件。同一份历史支撑追查、上下文与恢复，而恢复不会重新执行历史上的工具操作。

[English](README.md) · **简体中文**

[开始使用](docs/getting-started.zh-CN.md) · [按你的方式定制](#按你的方式定制) · [文档导航](docs/README.zh-CN.md) · [参与贡献](CONTRIBUTING.md)

## 为什么选择 Lattice

- **替换部件，掌握自己的工作方式。** 通过明确的装配说明替换模型、工具、策略和主循环。从随附的终端 Agent 开始，逐步掌握它如何工作。
- **历史就是状态的正本。** 从记录下来的事件恢复工作。恢复重建状态，而不是重复过去的工具操作。
- **追查因果，而不只查看时间戳。** 沿着因果关系找到请求、结果和决定，查清答案怎样形成、操作停在哪里。
- **用你熟悉的语言扩展。** 进程外部件通过逐行 JSON 协议通信。用 Python、JavaScript、Rust，或适合这项工作的其他语言编写工具。

## 十秒看懂运行时

```text
                      内核
部件 A ──→ 记录事件 ──→ 投递 ──→ 部件 B
               │
               ▼
          带因果的流水
```

**流水就在投递路径上，不是事后补写的日志。** 内核记录事件，再沿装配接线投递。模型、主循环、上下文管理、策略、工具和界面，都是围绕它组装的可替换部件。

可以深入[架构总览](docs/architecture-overview.zh-CN.md)、查看 [JSON 契约](schemas/)，也可以直接从下面开始使用。

## 开始使用

从源码构建，连接自己的模型，再进入项目开始工作。

**构建环境：** Rust/Cargo 和 C 工具链；Linux 还需要 OpenSSL 开发库和 `pkg-config`。原生打包已在 Ubuntu 24.04 x86-64 和 macOS 15 Apple Silicon 实测。平台配置及版本发布状态见[安装与升级指南](docs/installation.zh-CN.md)。

```sh
git clone https://github.com/IamK77/Lattice.git
cd Lattice
cargo build --locked --release --bin lattice
```

**接下来：[连接自己的模型账号，开始第一次对话](docs/getting-started.zh-CN.md)。** 指南带你配置密钥、在项目里启动，以及继续之前的工作。

## 用它完成工作

从一项具体任务开始对话：

| 你想做什么 | 可以这样开始 |
| --- | --- |
| 理解陌生项目 | “解释这个仓库的结构，找出主要入口和相关测试。” |
| 完成一项小改动 | “调查这个失败的测试，做一个小范围修复，并运行受影响的测试。展示改动和结果。” |
| 为工作流程造工具 | “帮我把这项重复工作变成工具。完成设计和测试，再带我安装。” |

用 `/model` 切换已配置的模型，回到保存的对话，查看工作背后的请求和结果。

## 按你的方式定制

先使用随附的配置，再一次调整一件事。

### 配置现有功能

- **你的模型：** 接入 Chat Completions、Responses 或 Anthropic Messages 端点，用 `/model` 切换已配置的模型，见[模型配置](docs/model-configuration.zh-CN.md)。
- **你的项目：** 在项目的 `AGENTS.md` 或 `CLAUDE.md` 中写下约定与测试要求。Lattice 在启动时读取项目规则；修改后需要启动新会话，见[项目定制](docs/getting-started.zh-CN.md#5-加入项目约定)。
- **可选集成：** 使用前查看[工具和客户端指南](docs/README.zh-CN.md#配置与定制)中的依赖与权限。

### 开发部件与装配

这些是开发任务，不是使用 agent 的前提。

- **你的工具：** 通过增加部件扩展能力，不必为此修改整个应用。外部部件不限于 Rust，见[示例](examples/)与[自造工具演示](docs/tool-building-walkthrough.zh-CN.md)。

- **你的运行时：** 用 `lattice assembly` 导出装配，通过 `LATTICE_ASSEMBLY` 选择自己的装配。替换决定 Agent 行为的部件和接线，见[装配配置](docs/assembly-configuration.zh-CN.md)。

## 先了解你授予了什么权限

工具能够读写文件、执行程序和访问网络。**Lattice 不是沙箱。** 请在能接受这些权限的工作目录里运行，并在采用结果前检查改动。

对话、工具请求、结果和附件会保存在本地，也可能发送给你选择的模型提供方。工具读到的秘密不保证被脱敏；浏览器和桌面截图也可能留存并发送给模型。取消操作不会撤销已经发生的修改。

使用敏感文件或账号前，请阅读[数据、权限与恢复边界](docs/getting-started.zh-CN.md#数据与权限)。

## 进一步了解

[文档导航](docs/README.zh-CN.md)按任务提供三个入口：

- [使用 Lattice](docs/README.zh-CN.md#使用-lattice)：安装、开始、继续工作、了解权限和求助。
- [配置与定制](docs/README.zh-CN.md#配置与定制)：模型设置、项目规则和现成集成。
- [开发与维护](docs/README.zh-CN.md#开发与维护)：部件、装配、架构、契约、贡献与发布。

英文首页和入门路径已经提供；更深入的架构与集成文档目前主要使用中文。

## 许可证

采用 [Apache License 2.0](LICENSE)。项目声明见 [NOTICE](NOTICE)，第三方资源来源见[资源署名](assets/README.md)。依赖和可选外部部件保留各自的许可证。

使用问题见[支持说明（英文）](SUPPORT.md)，漏洞请通过[安全政策（英文）](SECURITY.md)中的私密渠道报告。参与社区须遵守[行为准则（英文）](CODE_OF_CONDUCT.md)，项目职责见[维护者说明（英文）](MAINTAINERS.md)。
