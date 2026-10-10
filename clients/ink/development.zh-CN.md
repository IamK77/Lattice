# Ink 客户端开发参考

[使用指南](README.zh-CN.md) · [English user guide](README.md) · [文档导航](../../docs/README.zh-CN.md#开发与维护)

本页供修改客户端或实现兼容前端的人阅读。启动、按键、历史阅读与授权风险在使用指南中，不需要先学协议才能使用 Ink。

## 边界与传输

客户端通过 Unix socket 与 daemon 通信，只认 NDJSON 协议——每行一个 JSON 对象。客户端不包含 Rust，也不依赖内核实现；内核不识 JavaScript。语言中立体现在这条纯数据边界，而不是共享内存中的对象。

客户端发送的基础消息使用外部标签和 snake_case：

```json
{"attach": {"stream": "main", "template": null}}
{"send_text": {"stream": "main", "text": "hello"}}
{"interrupt": {"stream": "main"}}
```

以下展示基础、旧版回复外形；`<event>` 是占位符，不是可直接解析的 JSON。协商扩展后的附着消息见后文，不能把旧结构当成当前完整协议：

```json
{"attached": {"stream": "main", "replay": [<event>, ...]}}
{"appended": {"stream": "main", "event": <event>}}
{"notice": {"stream": "main", "source": "model", "payload": {"chunk": "he"}}}
{"quiescent": {"stream": "main"}}
{"error": {"message": "..."}}
```

事件为 Lattice 信封：`{ v, id, seq, stream, time, type, source, causes, origin?, reason?, payload }`。显示层用 `src/protocol.js` 的 `renderLine` 渲染；权限控件另行消费当前权威状态，不能从旧的显示记录推导当前权限。

## 历史分页

握手声明 `history-pages`。首次只接收最近一页，以及截至该页边界的忙碌、等待、未答授权和技能菜单状态；看旧页不会让这些状态倒退。客户端保留最多 500 条实时显示记录和当前旧页，不自动下载整本账。

服务端每页最多 128 条事件，原始记录字节预算为 128 KiB；单条超大的事件单独发送，预算不是网络编码后的严格字节上限。游标绑定流、附着代次和历史边界，只能顺序消费；新事件仍实时到达，旧页不混入边界之后的事件。读取失败返回携带原游标的 `history_error`，不推进位置。未声明分页能力的旧客户端只能附着到一页以内的历史，超出后明确要求升级，不静默截断。

## 操作授权消息

协商 `operation-permissions-v1` 后，服务端返回独立的 `attached_v2`，不向旧的严格历史结构追加字段。其 `authorization` 包含 `attachment`、`interface`、两个服务实例名、`through`、持久 `grants` 和完整前缀中尚未回答的 `pending_authorizations`（问题信封）。卡片即使在末页之外，也保留类型与范围；没有服务时相应字段为 null，不据此虚构权限。

每个标签的每次绑定取得独立令牌，控制请求形如：

```json
{"set_permission":{"stream":"main","attachment":"<server-token>","enabled":true}}
{"authorize_operation":{"stream":"main","attachment":"<server-token>","request":"<question-event-id>","approve":true,"scope":"once"}}
{"revoke_grant":{"stream":"main","attachment":"<server-token>","grant":"<grant-id>"}}
```

范围还可选 `flow`。`request` 是问题事件编号，不是挂起调用编号。未知范围、过期令牌、未协商或未绑定的控制，都不能降级成旧批准。永久信任仍走显式旧 `authorize` 路径，不由前端写文件。

本地命令的错误参数不发给模型，发送有效请求后等服务端权威状态，不乐观显示成功。旧 `attached` 下新控件不可用；旧 `y` 保留旧服务语义，可能写永久信任，不能重新标为“仅本次”。完整职责与前缀边界见[操作授权设计](../../docs/development/design-action-authorization.zh-CN.md)。

## 源码入口

- `src/connection.js`：socket、NDJSON 分帧和收发。
- `src/protocol.js`：消息编解码与显示行。
- `src/state.js`：可独立测试的多标签状态变换。
- `src/authorization.js`：授权命令、卡片按键和范围说明。
- `src/app.js`：基于 React、Ink 与 htm 的界面，无前端构建步骤。
- `src/theme.js`：配色和说话者标记。
- `bin/lattice.js`：客户端入口。

## 验证

在 `clients/ink` 中运行：

```bash
npm test
```

测试包含协议与模拟 daemon 的完整轮次、多标签状态隔离、无终端挂载的真实 Ink 界面等检查。它们不是实际模型或人工终端体验的验收。

`scripts/xlang-smoke.js` 对接真实 Rust daemon，属于独立的跨语言检查，不在 `npm test` 内。`node scripts/preview.js` 打印示例画面供检查布局，不等于连接了真实对话。这些是开发验证路线，不是使用者启动客户端的前置步骤。
