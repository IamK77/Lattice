# 开发参考导航

[English](README.md) · [文档导航](../README.zh-CN.md) · [参与贡献（英文）](../../CONTRIBUTING.md)

修改 Lattice、实现部件或维护发布时走这条路线。连接模型、添加项目约定和使用现成集成，仍属于[使用与配置入口](../README.zh-CN.md)。

## 什么问题归哪份文档？

| 问题 | 参考与职责 |
| --- | --- |
| 这是怎样的系统？ | [架构总览](architecture-overview.zh-CN.md)：简短、完整的心智模型。 |
| 为什么采用这些边界？ | [架构决策](architecture-decisions.zh-CN.md)：机制与理由。 |
| 贡献时必须守住什么？ | [贡献规则](../../CLAUDE.md)为英文；[参与贡献（英文）](../../CONTRIBUTING.md)和[协作流程](../maintenance/workflow.zh-CN.md)说明实际贡献路径。 |
| 部件之间必须就哪些数据与行为达成一致？ | [契约参考](../contracts/README.zh-CN.md)：六份人读说明、对应 Schema 与当前实现定位。 |
| 聊天产品怎样组装？ | [装配配置](assembly-configuration.zh-CN.md)与[能力迁移（英文）](capability-split-migration.md)：产品基线、运行时位置和安装增补，不是新增内核概念。 |
| 怎样维护与发布？ | [维护者入口（英文）](../../MAINTAINERS.md)和[维护者手册](../maintenance/maintainer-handbook.zh-CN.md)：责任、检查、依赖与发布流程。 |

契约含义、机器可读结构、当前源码和已经观察到的测试证据，彼此相关但不能互相替代。[契约导航](../contracts/README.zh-CN.md#规范与实现)解释这些层次，包括当前标准口型目录。实现有出入，不等于可以悄悄改写契约。

## 当前实现参考

- **部件：** [示例](../../examples/)、[自造工具演示](tool-building-walkthrough.zh-CN.md)和[跨进程桥](../contracts/06-process-bridge.zh-CN.md)。外部部件不限于 Rust。先读每个示例的用途，测试探针与脚本演示不一定是普通安装教程，也不一定没有副作用。
- **前端与集成：** [首次配置实现（英文）](setup-development.md)、[Ink 协议与测试](../../clients/ink/development.zh-CN.md)、[桌面后端与测试](desktop-development.zh-CN.md)。对应的用户指南另行保留。
- **授权：** [操作授权与界面权限](design-action-authorization.zh-CN.md)，与安装信任、操作系统隔离区分。
- **专家：** [定义、启用与执行快照](design-expert-execution.zh-CN.md)。这是当前实现参考，不能因为文件名带 `design` 就当成未实现提案。

## 原型与验收记录

先看记录的范围，再把它当成证据：

| 记录 | 证明什么，又不证明什么 |
| --- | --- |
| [专家快照原型](../records/design-expert-snapshot-prototype.zh-CN.md) | 历史上的测试内架构实验；当时缺少的生产功能，不是今天的能力清单。 |
| [专家管理体验](../records/expert-management-prototype.zh-CN.md) | 首版操作与验收说明。#45 在 2026-09-30（UTC）记录人工验收与交付，不接受后来的修改，也不证明 Ink 已有同等表单。 |
| [签名预演验收](../records/signed-preview-acceptance.zh-CN.md) | 某份准确源码、运行和签名字节的正反向验证；不是发布批准、当前候选状态页，也不是安装、升级或模型提供方验收。 |

保留历史实验及反例，标明阶段并链接后继实现，不把过去改写成今天的功能；反过来，也不能把所有设计文档都归为过时材料。

## 发布与验证

从维护者手册进入[候选准备](../maintenance/release-preparation.zh-CN.md)、[产物说明](../maintenance/release-artifacts.zh-CN.md)和[发布执行](../maintenance/release-publication.zh-CN.md)。获取或替换程序的使用者应读[安装与回退](../guides/installation.zh-CN.md)，不必先还原整条发布流水线。

发布说明描述机制与前提，不报告仓库设置的实时值。预演记录只对准确的那批产物作证；进行新的操作时，重新核对本次获准源码、运行和仓库条件。人工验收、合并、安装与发布仍是不同的事。

按相关实现与贡献指南跑受影响的检查。跳过的集成测试不算通过，脚本模型不证明供应商兼容，本地回归不代替真人体验验收。本页增加的是导航，不是新的运行时或协议保证。
