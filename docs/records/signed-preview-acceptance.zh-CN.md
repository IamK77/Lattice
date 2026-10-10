# 签名预览验收

[English](signed-preview-acceptance.md) · [维护者清单](../maintenance/maintainer-handbook.zh-CN.md)

这是针对下述源码快照的一次验收，不是正式发布许可，也不证明后来提交已签名。记录与验收测试在实测构建之后加入，不为把记录放进它自己的源码身份而反复重建。

## 输入与运行记录

- 仓库：`IamK77/Lattice`；候选 PR：[#31](https://github.com/IamK77/Lattice/pull/31)。
- 候选版本：`0.1.0`；分支引用：`refs/heads/release/next`。
- 签名对应源码：`aab7d1e06f236f9d986bdca50db455ddd0b4daa0`。
- 开发快照：`690c36d7ca964b697332a05622d44a7593b271bf`。
- 候选准备运行：`36540069785`；必需 CI：`36540101596`；本机平台打包检查：`36540101829`。
- [签名预览运行 36540553467](https://github.com/IamK77/Lattice/actions/runs/36540553467)：身份检查、两平台构建和预览全部通过，正式发布与发布后同步任务跳过。
- 下载附件：该次运行的 `release-preview`，附件编号 `11020561940`。Actions 保留期为 14 天；过期不意味着可以拿重建的字节冒充原件。
- 独立消费端验证：macOS ARM64，GitHub CLI `2.93.0`，Python `3.12.13`。

首次候选的 CI 运行 `36538643456` 暴露了测试数据问题：测试把真实候选变更日志复制进本应“尚未发布”的临时仓库。[PR #32](https://github.com/IamK77/Lattice/pull/32) 改用固定测试数据。新增隔离测试在修复前失败、修复后通过；没有放宽生产代码的重复版本检查，也没有删改真实变更日志。刷新后的候选检查通过。

## 下载文件的身份

| 文件 | SHA-256 |
|---|---|
| `lattice-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` | `73632ebe55887f0b6a3c7ae52b3f7c638d4f79778d112f34d1aed423b0f69f14` |
| `lattice-v0.1.0-aarch64-apple-darwin.tar.gz` | `befaf12f21bb85895f5613e0cf5c8af7d2b3e732c8107a7c80521f91a545646c` |
| `SHA256SUMS` | `4ac47eecb9e3c904920861d7a1f99e300423ebe8e67437c77c026789c690fada` |
| `release-manifest.json` | `5a6b0037dbae151d80364f885355f14720ad9bbb6f20a5d73aa3b7c24400277a` |
| `provenance.json` | `8860738fa0750bf7dc901083c44104b56f1aa0b25178de8d1718198fa479f19a` |

## 可以重跑的验收

先让 `gh` 登录能够读取该仓库 Actions 附件的账号。下载目录须为空，预期提交与分支来自已核对的工作流运行，不能从下载的清单中反推。以下命令在仓库根目录执行：

```bash
assets="$PWD/target/signed-preview-36540553467"
gh run download 36540553467 --repo IamK77/Lattice \
  --name release-preview --dir "$assets"
python3.12 scripts/accept_signed_preview.py \
  --directory "$assets" --version 0.1.0 \
  --commit aab7d1e06f236f9d986bdca50db455ddd0b4daa0 \
  --source-ref refs/heads/release/next
```

这套手动验收已对真实下载的字节通过：

- 两份压缩包、校验和及清单的来源证明，与仓库、提交、候选分支、`release.yml` 签名工作流及托管签名运行环境一致。
- 包内内容、构建身份与校验和、清单的完整内容相符。
- 改动临时压缩包副本的一个字节，验证拒绝。
- 使用错误源码提交，因 `SourceRepositoryDigest` 不符而拒绝。
- 声称候选来自 `main`，因 `SourceRepositoryRef` 不符而拒绝。
- 每个反向测试前后都重新验证原件，并检查具体拒绝诊断；原始文件字节保持不变。

该测试需要已下载的签名附件，并联网访问验证基础设施，所以不放进普通单元测试的自动发现范围。将来 CLI 的错误措辞变化时，应先检查差异，不能把断言放宽成“任意非零退出都算密码学验证拒绝”。

## 结论边界

本次通过的是这份预览包的获取、来源身份、完整性及反向验证。原生构建任务还运行了包内程序的版本与帮助检查；消费端测试只检查数据，不执行下载程序。

它不代表已验证本机安装、不同构建之间的升级回退、真实模型供应商、可复现构建、Apple 公证或真正的不可变发布。

在本次验收阶段，候选保持未合并且没有自动合并，没有创建发布或标签，`RELEASE_AUTOMATION_ENABLED` 仍未设置。
