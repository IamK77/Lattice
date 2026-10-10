# Lattice Ink client

[简体中文](README.zh-CN.md) · [Documentation](../../docs/README.md) · [Developer reference (Chinese)](development.zh-CN.md)

Ink is an alternative terminal interface for Lattice, with tabs for viewing multiple conversations. Configure a model and start the Lattice background service first, then open the client to connect to it.

<a id="run"></a>

## Start the daemon, then the client

Prepare Node.js 18 or newer and npm. Running the background service from source also needs the [Rust build prerequisites](../../docs/guides/installation.md#source-build-available-now).

**1. Prepare model configuration.** Follow [Model configuration](../../docs/guides/model-configuration.md). Make configuration and keys available in the terminal that starts the **background service**, which reads them at startup. Prepare the initial configuration through the ordinary terminal setup guide or by editing the configuration file before starting the service.

**2. Start the service** from the repository root:

```bash
cargo run --bin lattice -- serve
```

This uses the repository as the working directory. To work in another project, run your built `lattice serve` from that project directory. Tools can also access files outside the working directory.

**3. Start the client** in another terminal, from the repository root:

```bash
cd clients/ink
npm install
npm start
```

The default connection path is `~/.lattice/daemon.sock`. When changing it with `LATTICE_SOCKET`, use the same value on both ends. `LATTICE_STREAM` selects the conversation name, defaulting to `main`. Two clients using the same name operate on the same conversation.

If connection fails, check that the service is running and the paths match, then reopen the client to connect. Keep existing socket files; another service may be using them. See [Troubleshooting](../../docs/guides/troubleshooting.md) for account or startup errors.

## Everyday controls

| Control | What it does |
| --- | --- |
| `Enter` | Send the input draft. |
| `Tab` | Switch tabs. |
| `Esc` | Request interruption of the current busy conversation; other clients sharing it also see the change. |
| `/new [name]` | Open a tab for the named conversation. Omitting the name generates a new one; an existing name attaches to that conversation. |
| `/btw [question]` | Open a side conversation that can read the current conversation's history to discuss something else. The current conversation keeps running; tools in the side conversation can also perform actions. |
| `/tab <n>` | Switch to tab n. |
| `/close` | Close the current tab while keeping the background conversation; the last tab stays open. |
| `/exit` or `Ctrl-C` | Exit the client. The background service and its tasks continue. |
| `/clear` | Clear the current tab's live display. Model context, saved history, running tasks, permissions and the older page being viewed remain in place. |

Other slash-prefixed text, such as `/reset`, is sent to the model as a message. Use the listed commands for client operations.

<a id="历史分页"></a>

## Read earlier history

Use `/older` to move back one page and `/latest` to return to the latest position. `/latest` reattaches this tab to the conversation and ends its previous temporary permission; reopen the client to reconnect after a lost network connection.

The interface displays up to 500 recent live records plus the older page you are viewing. The complete history stays in the service, and model context is managed separately. New messages continue to arrive while viewing older pages; permission and work state reflect the current service state. A failed page read preserves your position so you can try again.

<a id="操作授权"></a>

## Understand authorization before approving

The service sends a card when approval is needed. With an empty input draft, use these shortcuts:

- `y`: approve this request once; `n`: refuse.
- `f`: save the conversation-scoped permission shown on the card, when offered.
- `p`: permanently trust the request shown on the card, when offered.

Command-prefix grants allow added arguments. For example, after approving `git push origin`, it uses the working directory and `origin` configuration at execution time. The grant may still apply after the directory, remote address or executable contents change. Choose one-time approval when you want to confirm each action separately.

| Command or key | What it does |
| --- | --- |
| `/permission [on\|off]` | Inspect or change temporary action permission for this tab's attachment to the conversation. |
| `Shift+Tab` | Toggle temporary action permission while keeping the input draft. |
| `/grants` | View conversation-scoped grants. |
| `/revoke <id>` | Request revocation of a listed grant and wait for service confirmation. |

An already displayed approval card still needs its own answer after temporary permission is enabled. Closing the tab, disconnecting, reattaching the current conversation with `/latest` or restarting the service ends that temporary permission. Conversation grants are saved separately, and permanent trust is managed separately again. Completed actions remain in effect after revocation.

These shortcuts require a matching background service. Older services may lack some controls, and a legacy `y` can grant permanent trust. Check the scope displayed on the card before approving.

## Data and help

Conversations and tool output are saved and sent to the configured model service; they can contain file contents and keys. Completed actions remain in effect after cancelling work, closing tabs or exiting the client. See [Data and permissions](../../docs/guides/getting-started.md#data-and-permissions) for more information.

For help, describe the feature or step and include an error excerpt with keys and private content removed; see [Support](../../SUPPORT.md).

<a id="the-protocol-in-one-screen"></a>
<a id="layout"></a>
<a id="tests"></a>

## Developer reference

See the [developer reference (Chinese)](development.zh-CN.md) for the protocol, source layout and tests.
