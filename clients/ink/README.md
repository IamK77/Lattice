# Lattice Ink client

[简体中文](README.zh-CN.md) · [Documentation](../../docs/README.md) · [Developer reference (Chinese)](development.zh-CN.md)

An alternative terminal interface for Lattice, with multiple conversation tabs. It connects to an already running Lattice daemon; it does not start the daemon or configure your model account for you.

<a id="run"></a>

## Start the daemon, then the client

You need Node.js and npm (the package declares Node.js 18 or newer). The source-run command below also needs the [Rust build prerequisites](../../docs/installation.md#source-build-available-now).

**1. Prepare the model configuration.** Use [model configuration](../../docs/model-configuration.md). The configuration and credentials must be available to the terminal that starts the **daemon**. Setting a key in the client terminal does not change a daemon already running. The daemon checks local configuration, not provider authentication, and does not open the terminal setup wizard.

**2. Start the daemon** from the repository root:

```bash
cargo run --bin lattice -- serve
```

This uses the repository as its working directory. To work in another project, start your verified built executable with `serve` from that project directory instead. A working directory is not a filesystem sandbox.

**3. Start the client** in a second terminal, from the repository root:

```bash
cd clients/ink
npm install
npm start
```

The default socket is `~/.lattice/daemon.sock`. If you set `LATTICE_SOCKET`, use the same path for both processes. `LATTICE_STREAM` selects the client's conversation name; it defaults to `main`. Two clients attached to the same name share the same conversation, not private copies.

If connection fails, check that the daemon started successfully and that the socket paths match, then restart the client. The client does not automatically reconnect. Do not delete an existing socket as a routine fix: another daemon may be using it. For account or startup errors, use [troubleshooting](../../docs/troubleshooting.md).

## Everyday controls

| Control | What it does |
| --- | --- |
| `Enter` | Send the draft. |
| `Tab` | Switch tabs. |
| `Esc` | Request interruption of the current busy conversation. This affects the shared conversation and is not rollback. |
| `/new [name]` | Open or attach a conversation in a tab. Omit the name for a generated name; reusing an existing name may return to old work, not reset it. |
| `/btw [question]` | Open a separate side conversation that can inspect the parent's history read-only without interrupting it. Its own tools are not thereby restricted to read-only access. |
| `/tab <n>` | Switch to a numbered tab. |
| `/close` | Close this client's current tab/subscription, unless it is the last tab. The daemon conversation remains. |
| `/exit` or `Ctrl-C` | Exit the client, not the daemon. This does not by itself interrupt work running in the daemon. |
| `/clear` | Clear the current tab's live display records only. It does not reset model context, delete history, cancel work or revoke permissions; an older page already displayed remains. |

Only the supported local commands are intercepted. Other slash-prefixed text is sent as conversation text; do not assume `/reset` is an implemented reset control.

<a id="历史分页"></a>

## Read earlier history

Use `/older` to read one earlier page at a time and `/latest` to attach again at the latest position. `/latest` is not a network reconnect command and it ends the old binding's temporary permission.

The display retains at most 500 live records plus the current older page, rather than downloading the entire history. This is a display limit, not a model-context limit or deletion policy. Looking at an old page does not restore old busy/permission state; live updates still arrive. If reading a page fails, the position is retained so it can be retried.

<a id="操作授权"></a>

## Understand authorization before approving

With a compatible daemon and the relevant services, authorization cards offer shortcuts **only while the draft is empty**:

- `y`: approve this request once; `n`: refuse.
- `f`: save the flow-scoped permission actually offered by this card, when available.
- `p`: explicitly grant permanent trust for an introduced addition, when offered. This is different from a flow permission.

Read the displayed scope. A command-prefix grant may permit appended arguments; it does not bind the working directory, remote mapping or executable contents, and is not a filesystem sandbox. A grant for `git push origin` does not mean an independently checked destination for every future push.

`/permission [on|off]` inspects or changes this interface's temporary permission. `Shift+Tab` toggles it without sending or clearing the draft. `/grants` lists flow grants; `/revoke <id>` requests revocation of a listed flow grant, not permanent trust or already completed actions. The interface waits for server confirmation rather than declaring success immediately. Turning permission on does not automatically answer the pending card.

Closing a binding, disconnecting, reattaching with `/latest`, or reopening the runtime ends temporary permission. Flow grants persist separately; permanent trust is another mechanism. On an older daemon or without the required services, new controls are unavailable. A legacy `y` retains its old service meaning and may grant permanent trust; do not read it as “once.”

## Data and help

Conversations and tool output can contain secrets and are sent to the configured model provider. Cancellation, closing tabs and exiting the client do not undo actions. Review the [data and permission boundaries](../../docs/getting-started.md#data-and-permissions) before sensitive work. For help, report the feature or step and a short inspected diagnostic, not a complete history or environment dump: [Support](../../SUPPORT.md).

<a id="the-protocol-in-one-screen"></a>
<a id="layout"></a>
<a id="tests"></a>

## Developer reference

Protocol messages, backend boundaries, source layout and test commands belong to the [developer reference (Chinese)](development.zh-CN.md); they are not prerequisites for using this client.
