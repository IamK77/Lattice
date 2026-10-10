# 桌面工具开发参考

[使用与安装](../guides/desktop.zh-CN.md) · [English user guide](../guides/desktop.md) · [文档导航](../README.zh-CN.md#开发与维护)

本页供维护工具适配件、实现后端与运行回归的人阅读。普通用户需要的安装、系统许可、截图暴露和部分执行风险留在使用指南；这里的测试不是安装前置步骤。

## 工具接口

标准装配提供 `Desktop`，没有隐藏的启用开关；完整参数通过 `FindTools` 按需发现，不常驻每轮提示。这是通用桌面工具，不是某个模型供应商的专用 computer-use API。

操作外形如下；目标必须来自实际列举，坐标只是示例，不能照抄到任意窗口：

```json
{"operation":"list"}
{"operation":"observe","target":"从列表取得的 id"}
{"operation":"act","target":"同一个 id","actions":[{"type":"click","x":120,"y":80},{"type":"type","text":"literal text"}]}
{"operation":"close"}
```

- `list` 返回不透明标识、应用名和标题，不公开原生进程编号或供应商工具名。
- `observe` 返回 PNG 文档引用及像素宽高。坐标从截图左上角开始，不能等于宽或高。
- `act` 每批最多八项，可用 `click`、`type`、`key`、`scroll`、`drag`。点击次数最多三次，滚动量为一至五十。
- `key` 支持字母数字与常用编辑、方向键；修饰键为 `command/control/shift/option`。`delete` 表示向前删除，`backspace` 表示向后删除。
- 批次结束后尽力返回新截图，观察失败单独报告。`completedActions` 是结束的驱动操作数，不是业务效果保证；`uncertainAction` 指向可能部分执行的一项，不能整批重试。
- 不确定输入使当前驱动拒绝继续输入；显式关闭后要重新列举并观察。取消会清掉连接、目标和坐标状态，但不会撤销已经发生的输入。

## 后端与宿主责任

工具与驱动边界定义在 [desktop_driver.rs](../../src/components/desktop_driver.rs)，随货实现见 [desktop_cua.rs](../../src/components/desktop_cua.rs)。新后端可实现 `DesktopDriver`，也可作为普通进程外工具部件使用 JSON 行契约，无需修改核心事件。

随货适配件面向 Cua Driver 0.26.1 的 MCP 接口，但不会校验或锁定驱动版本。入口优先级为配置 `executable`、环境变量 `LATTICE_DESKTOP_DRIVER`、默认路径 `~/.lattice/drivers/desktop`。路径覆盖不构成任意程序兼容承诺。

驱动以 `mcp --direct` 启动，不连接共享桌面守护进程；使用独立临时 HOME、清理后的环境，并设置关闭遥测与更新检查的变量。macOS 权限属于实际启动 Lattice 的宿主。适配件检查权限而不绕过：观察需要屏幕录制，输入另需辅助功能。

输入请求明确使用 `foreground` 投递，不在失败后偷偷升级方式。原焦点能否恢复由外部驱动承担，本仓真机测试没有建立无条件恢复保证，中断或终止时尤其不能承诺。

自动排除当前进程及宿主祖先；标准装配另通过 `hostProtection.applications` 配置终端、代理和系统控制应用清单。这是宿主配置，不是供应商内置策略。更换宿主或代理时需要维护它，但不能据此声称隔离恶意应用。

目标身份包括进程出生信息、窗口编号和应用身份；变化时拒绝复用旧目标。标题变化不改变身份，尺寸变化要求重新观察。`close` 终止驱动连接并释放目标，不退出用户应用。重放流水不重放输入。

## 图片与授权边界

观察和输入需要持久流水。图片按内容取名，落成 PNG 文件，不在每行一条的账本中内联 base64。发送前同时核对内容身份、字节数和 PNG 像素；损坏或替换要报错，不能静默丢图。图片适配覆盖 Anthropic、Chat Completions 与 Responses，但具体型号仍须支持视觉输入。

正常有界调用没有 Desktop 自己的逐窗口、逐批或逐点击审批。不要把共享工具规则改写成“每次点击都会出现批准卡”。窗口内容仍是外部内容，不能自行授权发送信息、填写凭证或破坏数据。

## 验证

在仓库根目录运行普通回归；它们使用可替换的本地假驱动和假 MCP 进程，不调用模型端点，也不需要桌面权限：

```sh
cargo test --lib desktop
cargo test --test desktop --test images
cargo test --test preset the_standard_assembly_starts_and_runs_a_turn
```

真机验收是明确标记的独立测试，不进入默认运行：

```sh
cargo test --test desktop_live -- --ignored --nocapture
```

它先检查宿主权限，再编译并启动仓库内的 AppKit 测试窗口，只对这个窗口截图、点击和输入，核对原文与截图变化，最后关闭自己创建的进程。发现目标时仍枚举在屏窗口的应用名、标题等元数据，不能声称完全不读取既有窗口信息。不联网、不调用模型；缺少权限是失败，不能伪装成通过。

这项测试覆盖专用窗口的发现、截图、点击与文字输入；不证明所有应用、滚动、拖动、快捷键、焦点恢复或模型自主操作循环都已经验证。每台机器仍需独立确认驱动兼容性和系统权限。
