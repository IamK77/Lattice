# 安装、升级与回退

[文档导航](../README.zh-CN.md) · [English](installation.md) · [第一次对话](getting-started.zh-CN.md) · [故障排查](troubleshooting.zh-CN.md)

## 发布状态与平台范围

**首个正式版本尚未发布。** 现在可以从源码构建，或使用维护者明确指定的签名预演产物。下面的 GitHub Release 命令，要等具名正式版本存在后才能使用。预演即使显示 `vX.Y.Z`，也不是稳定发布。

按实际来源选择一条路线，不必先读完另外两条：

| 你拿到的是什么 | 下一步 |
| --- | --- |
| 源码仓库 | [构建程序](#现在可用的源码构建)，再按[第一次对话](getting-started.zh-CN.md)继续；不用执行正式包下载命令。 |
| 维护者明确指定的签名预演运行 | 按[预演验证](#验证签名预演)继续，不套用正式标签的假设。 |
| 已经发布的具名正式版本 | 先[验证正式版本](#正式版本先验证后执行)，再[安装程序包](#安装到自己的用户目录)。 |
| 需要替换的既有安装 | 先读[升级与回退注意事项](#升级时保留旧程序)。 |

### 平台范围

| 程序包目标 | 已测试的构建、启动环境 |
|---|---|
| `x86_64-unknown-linux-gnu` | Ubuntu 24.04、x86-64 |
| `aarch64-apple-darwin` | macOS 15、Apple Silicon |

其他系统版本和架构尚未测试，目前没有 Windows 原生包。程序包包含程序、署名、许可证、构建记录和依赖报告，见[产物说明](../maintenance/release-artifacts.zh-CN.md)。macOS 程序包尚未提供 Apple Developer ID 签名或公证。

浏览器、桌面驱动、语言服务和 Ink 客户端按需另行安装；模型账号在首次启动时配置。

## 现在可用的源码构建

需要 Rust 1.89 或更新版本、Cargo 和 C 工具链。Linux 还需要 OpenSSL 开发头文件与 `pkg-config`。

```bash
git clone https://github.com/IamK77/Lattice.git
cd Lattice
# 要重复同一次构建，先检查并选定一个具体的已审阅提交。
cargo build --locked --release --bin lattice
./target/release/lattice --version
```

构建完成后，版本信息会标明这是开发构建。保留源码中的许可证和署名文件，接着按[第一次对话](getting-started.zh-CN.md)设置程序路径并连接模型。

## 正式版本先验证、后执行

以下示例使用 **Bash**。另行安装 GitHub CLI，并按需登录。明确选择一个已有、已审阅的版本，不盲目下载 `latest`。在新目录中操作，任一步失败便停止：

```bash
set -euo pipefail
export GH_HOST=github.com
version='X.Y.Z'  # 换成实际存在的、已审阅版本。
work="$(mktemp -d)"
cd "$work"
repo=IamK77/Lattice
release_ok="$(gh api "repos/$repo/releases/tags/v$version" --jq '.immutable == true and .draft == false and .prerelease == false')" || exit 1
[[ "$release_ok" == true ]] || exit 1
commit="$(gh api "repos/$repo/git/ref/tags/v$version" --jq '.object | select(.type == "commit") | .sha')" || exit 1
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || exit 1
gh release download "v$version" --repo "$repo" --dir "$work" \
  --pattern 'lattice-*.tar.gz' --pattern SHA256SUMS \
  --pattern release-manifest.json --pattern provenance.json
for asset in lattice-v"$version"-*.tar.gz SHA256SUMS release-manifest.json; do
  gh attestation verify "$asset" --bundle provenance.json \
    --repo "$repo" --source-digest "$commit" --source-ref refs/heads/main \
    --signer-workflow IamK77/Lattice/.github/workflows/release.yml \
    --deny-self-hosted-runners
done
```

当前发布流程使用轻量标签。脚本遇到其他标签类型会停止，此时向维护者确认对应提交。Linux 接着运行 `sha256sum --check SHA256SUMS`，macOS 运行 `shasum -a 256 --check SHA256SUMS`，确认两个归档都通过。构建环境与签名任务的记录详见[产物说明](../maintenance/release-artifacts.zh-CN.md)。

## 验证签名预演

向维护者取得预演的运行编号、候选提交、候选分支、版本和验证步骤。从该次运行的 `release-preview` Actions 产物下载两个归档、`SHA256SUMS`、`release-manifest.json` 和 `provenance.json`。信息缺失时，先请维护者补全再继续。

按这次预演的候选提交和候选分支验证签名证明与摘要。预演使用候选分支身份，正式版本使用 `refs/heads/main` 和正式标签；两条路线分别验证。[签名预演验收记录](../records/signed-preview-acceptance.zh-CN.md)记录了一次完整实例，每次新的预演也要核对它自己的文件与身份。

## 安装到自己的用户目录

这一步用于已经验证的程序包，不用于源码仓库。留在下载目录中，把 `version` 设为刚验证过的版本；预演必须先通过它自己的验证。按实际机器选择表中的目标，并保留完整解包目录，包括署名文件。

```bash
set -euo pipefail
: "${version:?Use the version verified in the previous step}"
target='aarch64-apple-darwin'  # 已测 Linux x86-64 改为 x86_64-unknown-linux-gnu。
root="$HOME/.local/share/lattice/versions"
destination="$root/v$version-$target"
mkdir -p "$root" "$HOME/.local/bin"
[[ ! -e "$destination" && ! -L "$destination" ]] || exit 1
mkdir "$destination"
tar -xzf "lattice-v$version-$target.tar.gz" --strip-components=1 -C "$destination"
installed_version="$("$destination/lattice" --version)" || exit 1
[[ "$installed_version" == "v$version" ]] || exit 1
"$destination/lattice" --help
```

**仅首次安装**时，用 `ln -s` 创建 `~/.local/bin/lattice`，指向 `"$destination/lattice"`。已有文件或链接不要直接覆盖。把 `~/.local/bin` 加到自己的 shell PATH 后，检查 `command -v lattice`、`ls -l "$HOME/.local/bin/lattice"` 和 `lattice --version`，避免 PATH 前面另一份安装仍被优先使用。

接着按[第一次对话](getting-started.zh-CN.md)连接模型，在自己的项目目录启动。需要浏览器或桌面工具时，再按对应指南安装。

## 升级时保留旧程序

1. 先读变更记录和兼容性说明；没有自动更新命令。
2. 正常停止会话和后台服务，再备份或切换程序。不要在当前 agent 对话里替换正在承载它的运行时。
3. 把**完整** `~/.lattice` 数据树备份到私密位置，只排除临时 socket 文件。还要备份指向其他位置的模型、偏好、完整装配与安装增补文件，以及记录引用到树外的附件、文档目录。`.ledger` 是目录，只复制 JSONL 并不完整。检查备份完整性并保护权限，其中可能有密钥和未脱敏历史。不要通过导出全部环境变量来做备份。
4. 验证新版本，解到**新的**版本目录，不覆盖旧目录；切换前先检查新程序的版本和帮助。
5. 检查现有命令链接，记下旧目标。如果是非本安装方式管理的文件、指向目录的链接，或指向版本目录之外，应停止，按原安装方式管理。
6. 在 `~/.local/bin/lattice` 旁创建一个新的临时符号链接，指向新程序，再重命名替换现有的**符号链接**。两者处在同一文件系统，才能以一次重命名原子切换指针；保留旧版本目录和旧目标记录。不要编辑或截断正在运行的二进制。
7. 再检查 PATH 和版本，先用专门的测试数据启动，再打开重要历史。

**回退需要分别处理程序和数据。** 把命令链接改回旧版本，会恢复旧程序；新版本写过的数据仍会保留。回退前先查版本说明，确认旧程序能否读取现有数据。需要恢复旧格式时，先停止全部会话，另存升级后的数据，再恢复升级前的完整备份。

卸载命令链接和明确选定的程序目录，与删除用户数据是两回事。不要把删除 `~/.lattice` 当成安装清理；是否删除备份和旧版本目录，应另行明确决定。
