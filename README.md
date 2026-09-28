# Lattice

一个可组装、可审计的 agent runtime。模型、工具、对话循环与前端是不同部件，通过显式接线与可序列化事件协作；事件流水保存执行事实，供恢复和回查。

核心与官方部件使用 Rust，外部部件不受语言限制。仓库包含终端界面、Unix socket daemon、JavaScript/Ink 客户端，以及 Python 进程部件示例。

## 能做什么

- 使用 Anthropic Messages、OpenAI 兼容 Chat Completions 或 Responses 方言接入模型。
- 调用文件、搜索、命令执行、网络、代码导航、浏览器和桌面等工具；部分工具依赖额外程序或系统权限。
- 按部件自述安装工具，用纯数据装配说明书替换实例与接线。
- 保存带因果关系的事件流水，恢复对话而不重新执行历史工具操作。
- 管理上下文视图、压缩、支线对话和独立专家委托。

专家委托不是完整的任务图调度系统，进程隔离也不是沙箱。安全边界见下文。

## 构建与免密钥试运行

需要 Rust/Cargo 和本机 C 编译工具链。Linux 还需要系统 OpenSSL 的开发库、头文件和 `pkg-config`；例如 Debian/Ubuntu 的 `libssl-dev` 与 `pkg-config`，其他发行版使用对应软件包。当前命令执行、进程桥及 daemon 面向 Unix 环境；桌面适配件目前面向 macOS，不承诺 Windows 完整支持。

可选工具另有依赖：Browser 需要 Chrome/Chromium，可用 `LATTICE_BROWSER` 指定程序；代码导航需要对应语言的本机语言服务，缺失时明确报错，不自动安装。桌面驱动见 `docs/桌面操作.md`。

```sh
cargo build --release --bin lattice
./target/release/lattice --version
cargo run --example heartbeat
```

`heartbeat` 是不调用模型的部件与流水示例。确定性终端试运行可使用：

```sh
LATTICE_SCRIPTED=1 ./target/release/lattice
```

这使用脚本模型，不会提供真实模型能力。正式使用前配置自己的模型端点与密钥。

## 配置模型

二进制不附送可用的模型账号或密钥。默认目录是 `~/.lattice/models.json`，也可由 `LATTICE_MODELS` 指定其他文件。创建或编辑目录时保留已有条目，不要为了套用示例覆盖现有配置。

下面只是形状示例，**端点与型号都是占位内容，必须按自己的供应商替换**：

```json
{
  "models": {
    "my-model": {
      "adapter": "openai",
      "model": "your-model-id",
      "baseUrl": "https://api.example.invalid/v1",
      "apiKeyEnv": "LATTICE_MODEL_KEY",
      "profile": {
        "contextWindow": 32768,
        "maxOutputTokens": 4096
      }
    }
  }
}
```

`adapter` 可用 `openai`、`anthropic` 或 `responses`；上下文与输出上限也必须符合实际型号，不要盲用示例数值。目录正本见 `schemas/model_catalog.json`，型号档案见 `schemas/model_profile.json`。兼容方言不等于支持供应商的所有扩展。

由本机凭证管理方式向 `LATTICE_MODEL_KEY` 注入密钥。也可以在 Bash 中交互输入，避免把值直接写成命令行文本：

```bash
printf 'API key: '
read -r -s LATTICE_MODEL_KEY
printf '\n'
export LATTICE_MODEL_KEY
./target/release/lattice
```

已有启动环境变量与保存的选择可能影响默认模型；可在界面用 `/model` 检查与切换。`LATTICE_THINKING` 可选择思考设置，空值表示不发送思考参数；具体档位是否有效取决于模型和端点。不要把真实密钥提交进仓库。

## 运行入口

```sh
./target/release/lattice                 # 终端界面
./target/release/lattice -c              # 继续最近的对话
./target/release/lattice --help          # 命令与环境变量说明
./target/release/lattice assembly        # 输出完整装配基线
./target/release/lattice serve           # Unix socket daemon
```

完整装配基线与安装增补是两份不同文件，见 `docs/装配配置.md`。JavaScript 客户端见 `clients/ink/README.md`；运行其测试需要 Node.js，CI 使用 Node 22。部分 Rust 测试会启动 Python 3 子进程。

## 数据与安全边界

- 默认工具在启动目录工作，但不构成强隔离。工具可以读写文件、执行程序或联网；不要在无法接受这些权限的环境中运行。
- 工具作用面由工具自己声明，信任闸主要防误操作，不防恶意实现。对同一 URL 或本地路径的授权不保证其内容以后不变。
- 对话、工具请求、工具结果和附件会进入本地持久流水，并可能成为发送给所选模型提供方的材料。工具读到的秘密不保证被脱敏，运行期新取得的密钥也不会自动加入脱敏清单。
- 浏览器和桌面截图会留存并发送给模型；系统权限、外部发送及凭证输入仍须由使用者管理。
- 中断表示结局未知，不表示操作没有发生；取消不回滚副作用。不要把不确定的结果当作可以安全重试的证明。
- 流水、模型目录、安装增补和信任记录通常位于 `~/.lattice/`。发布、共享或备份这些数据前应单独检查敏感内容。

详细机制、当前限制与未提供的隔离能力见 `docs/架构决策.md`。本项目尚未指定项目级许可证；第三方资源来源说明保留在各自文件中，例如 `assets/README.md`。

## 阅读与贡献

- `docs/架构总览.md`：系统的整体结构。
- `docs/架构决策.md`：各子系统机制、理由和边界。
- `docs/contracts/`、`schemas/`：事件、部件、装配与跨进程协议。
- `docs/桌面操作.md`：可选桌面驱动、系统许可及使用方式。
- `examples/`：可运行的部件和宿主示例。
- `CLAUDE.md`：贡献规则。

修改后优先运行受影响的定向测试，格式与静态检查使用：

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --test bridge --no-fail-fast
```

完整 Rust 测试使用 `cargo test --no-fail-fast`；需要真实端点、浏览器或桌面权限的验收另行显式运行，跳过不能算作通过。构建版本由提交尾行 `Type:` 计算，因此从 Git 构建时应保留完整公开历史；归档源码没有 Git 时使用 `Cargo.toml` 中的版本。
