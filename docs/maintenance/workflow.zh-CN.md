# Git Flow 协作流程

[English](workflow.md) · [贡献规则](../../CLAUDE.md)

`main` 是对外稳定线，`develop` 汇集下一批开发成果。已有公开提交保持不动；这套流程约束后续改动，不倒造之前不存在的分支历史。

## 分支与合并请求

| 分支 | 从哪里创建 | 合到哪里 | 用途 |
| --- | --- | --- | --- |
| `feature/<名称>` | `develop` | `develop` | 新功能、未发布代码的修复、文档、测试和维护 |
| `release/<名称>` | `develop` | `main`，随后同步到 `develop` | 整理并发布维护者明确批准的版本 |
| `hotfix/<名称>` | `main` | `main`，随后同步到 `develop` | 不夹带未完成开发，修复稳定线 |
| `main` | — | `develop` | 将已发布内容及其合并历史带回开发线 |

不得直接向 `main` 或 `develop` 推送改动，不得强推或删除这两条长期分支。分支名只约束合并方向，不能证明代码从哪里来；审查时仍须检查差异与祖先关系。

Dependabot 有一个有限例外：同仓库、由 `dependabot[bot]` 创建的 PR 可以使用 `dependabot/` 分支进入 `develop`，不得进入 `main`。检查和人工审阅不减免。详见[依赖与持续集成](supply-chain.zh-CN.md)。

普通改动的路径：

```sh
git fetch origin
git switch develop
git pull --ff-only origin develop
git switch -c feature/short-description
# Edit and run focused tests.
git add <explicit-paths>
git commit
# Pushing requires maintainer authorization; fork contributors use their own remote.
git push -u origin feature/short-description
gh pr create --base develop
```

提交标题和正文使用英文，标题遵循 Conventional Commits：`type: description` 或 `type(scope): description`，例如 `feat: add model switching`、`fix(cli): handle missing credentials`。类型取 feat、fix、perf、refactor、docs、test、style、chore、ci、build、revert 之一。不再要求自定义 `Type:` 尾注。按改动本身分类，不按分支名字分类；不兼容变更用 `!` 或 `BREAKING CHANGE:` 标记并说明迁移。详见[版本与变更记录](versioning.zh-CN.md)。

## 合并保留改动身份

只使用**保留原始提交的合并提交**，不使用压缩合并或变基合并。同一批工作进入两条长期分支时保留同一个提交身份。正式版本由 `Cargo.toml` 决定；发布整理排除合并提交，不把同一批工作重复计算。

合并请求的标题、正文分别作为默认合并消息的标题、正文，因此：

- 合并请求标题和正文使用英文，标题采用 Conventional Commits 格式，按实际改动分类。
- 正文说明改动、验证证据和未验证部分，不需要自定义尾注。
- 合并时不要换回 GitHub 默认的不带规范前缀的标题。
- 必须等待必需检查通过，且分支已同步目标分支。本地同步产生的合并提交也遵守同一套标题规范，通常使用 `chore: ...`。

`workflow` 检查合并方向、合并请求提供的默认消息，以及本次进入目标分支的每条提交。它检查标题格式、拒绝中文标题；它不判断英文行文质量，也不代替人判断某项改动应归 feat 还是 fix。检查对象是合并请求真实的源分支，不是 GitHub 临时生成的试合并提交；推送后的检查也会核对实际合并消息。

## 长期分支保护

`main` 与 `develop` 都要求通过合并请求进入、审查讨论已解决、分支已同步目标分支，并且 `workflow`、`check`、`frontend` 三项检查通过。管理员同样受约束，禁止强推和删除。当前只有一位维护者，不强制第二人的批准；**这不是独立审查**，合并前维护者仍须检查差异与验证证据。检查失败时查原因，不绕过保护。

保护规则和合并方式是 GitHub 上的设置，不随克隆复制。派生仓库或新建仓库要单独配置；有 CI 文件和这篇文档，并不代表保护已经生效。默认分支保持 `main`，让访客先看到稳定版入口。

## 发布与紧急修复

维护者把发布 PR 合入 `main`，才确认本次发布；准备候选不等于发布。版本以 `Cargo.toml` 为正本，重要变更记录在 [CHANGELOG.md](../../CHANGELOG.md)。开发检查通过本身不构成发布授权。

1. 发布从 `develop` 创建 `release/<名称>`；紧急修复从 `main` 创建 `hotfix/<名称>`。
2. 只放本次整理或修复所需改动，完成验证，向 `main` 提交合并请求。
3. 检查通过且发布范围得到确认后，使用准备好的规范提交消息合并。对外发布的标签和安装包必须对应这条已批准的稳定提交。
4. 再开 `main` → `develop` 的合并请求，把稳定线的合并历史与修复带回来。如果 `develop` 已前进，不得为满足“已同步目标分支”而把未发布开发合进 `main`。应从 `main` 新建工作用的 `release/` 或 `hotfix/` 分支，在该分支以合规消息合入最新 `develop`、解决可能的冲突，再向 `develop` 提交请求；不直接推长期分支。
5. 两条线都包含所需内容后，再删除短期分支。不自动删除，以免发布或紧急修复尚未回到开发线。

不得为补流程的形式而改写已经公开的历史。
