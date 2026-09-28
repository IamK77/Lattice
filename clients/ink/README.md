# @lattice/ink

A React/[Ink](https://github.com/vadimdemedes/ink) frontend for the Lattice daemon.

It connects to the daemon over its Unix socket and speaks **only the wire
protocol** (NDJSON — one JSON object per line). It contains no Rust and knows
nothing about the core's internals; the core does not know it is written in
JavaScript. This is Lattice's "language-agnostic frontend" made real: the
frontend is just another driver on the pure-data boundary.

## Run

先按根目录 README 配置模型目录与密钥环境变量，再从仓库根目录启动 Rust daemon：

```
cargo run --bin lattice -- serve
```

Then, here:

```
npm install
npm start
```

- `Enter` sends · `Esc` interrupts a running turn · `Ctrl-C` quits
- `Tab` / `Shift+Tab` cycle tabs
- Slashes are parsed in the driver (the core knows none of them):
  - `/new [name]` — open a new conversation in a new tab
  - `/btw [question]` — open a **sidechannel** derived from the current tab: a
    new conversation that observes the parent read-only (marked `⌥`), without
    disturbing it
  - `/tab <n>` — switch to tab n · `/close` — close the current tab
  - `/exit` quits · `/clear` clears the current tab

Environment:

- `LATTICE_SOCKET` — socket path (default `~/.lattice/daemon.sock`)
- `LATTICE_STREAM` — which conversation to attach to (default `main`); two
  clients attaching to the same stream watch the same conversation

## 历史分页

客户端在握手时声明 `history-pages`。首次只接收最近一页，同时接收截至该页边界的忙碌、等待、未答授权和技能菜单状态；这些状态不会因为查看旧页而倒退。`/older` 每次向前读取一页，`/latest` 重新附着回到最新位置。界面只保留最近 500 条显示记录和当前旧页，不自动下载整本账。

服务端每页最多 128 条事件，原始记录字节预算为 128 KiB；单条超大的事件单独发送，预算不是网络编码后的严格字节上限。游标绑定流、附着代次和历史边界，只能顺序消费；新事件仍实时到达，旧页不混入边界之后的事件。读取失败返回携带原游标的 `history_error`，不推进位置。未声明分页能力的旧客户端只能附着到一页以内的历史，超出后明确要求升级，不静默截断。

## The protocol, in one screen

Client → daemon (externally tagged, snake_case):

```json
{"attach": {"stream": "main", "template": null}}
{"send_text": {"stream": "main", "text": "hello"}}
{"interrupt": {"stream": "main"}}
```

Daemon → client:

```json
{"attached": {"stream": "main", "replay": [<event>, ...]}}
{"appended": {"stream": "main", "event": <event>}}
{"notice": {"stream": "main", "source": "model", "payload": {"chunk": "he"}}}
{"quiescent": {"stream": "main"}}
{"error": {"message": "..."}}
```

An `<event>` is a Lattice envelope: `{ v, id, seq, stream, time, type, source,
causes, origin?, reason?, payload }`. Render it with `renderLine` in
`src/protocol.js` (a byte-for-byte mirror of the Rust `render_line`).

## Layout

- `src/connection.js` — the socket, NDJSON framing, encode/decode
- `src/protocol.js` — `encode`, `decode`, `renderLine` (the whole protocol)
- `src/state.js` — the multi-tab state as a pure, testable reducer
- `src/app.js` — the Ink UI (React via [htm](https://github.com/developit/htm),
  no build step): tab bar, header, colored role markers, wrapped multi-line
  messages, a hand-rolled spinner while the agent works, a rounded input box
- `src/theme.js` — the palette and speaker markers
- `bin/lattice.js` — entry point

## Tests

```
npm test
```

- `test/client.test.js` — protocol + a full turn round-tripped against a mock
  daemon (`scripts/mock-daemon.js`)
- `test/state.test.js` — the multi-tab reducer (open/switch/close, per-tab isolation)
- `test/render.test.js` — the real Ink app mounted headlessly, incl. /new + /btw tabs

`scripts/xlang-smoke.js` is a cross-language check against a real Rust daemon
(not in `npm test`). `node scripts/preview.js` prints a sample frame so you can
eyeball the layout without a live terminal.
