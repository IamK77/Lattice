# Desktop operation

[简体中文](desktop.zh-CN.md) · [Documentation](README.md) · [Developer reference (Chinese)](desktop-development.zh-CN.md)

`Desktop` lets the agent observe and operate a selected **local application window**. It is not the isolated Browser. The standard assembly includes the tool, but you must separately provide its macOS driver and system permissions. The selected model must support image input; see [model configuration](model-configuration.md).

## Install the driver and grant system permissions

The included backend supports macOS and starts a separate Cua Driver process on demand. The driver is not bundled with the CLI; its absence produces an explicit error when needed, without preventing ordinary chat from starting.

The documented compatibility baseline is **Cua Driver 0.26.1**, not an automatic version lock. Obtain the driver from its publisher and verify the version, checksum, archive paths and application signature before running it. Lattice does not perform those checks for you. Other versions need separate compatibility verification.

The default entry is `~/.lattice/drivers/desktop`; it can point to a separately installed `CuaDriver.app/Contents/MacOS/cua-driver`. That layout is an example, not a required installation directory. Inspect an existing entry before replacing it. To use another location, set this **before starting Lattice**, in the same terminal:

```bash
export LATTICE_DESKTOP_DRIVER="/path/to/CuaDriver.app/Contents/MacOS/cua-driver"
```

If your assembly sets `executable`, that setting takes precedence over the environment variable. A path override does not make an arbitrary executable a compatible driver. Lattice starts the driver with a cleaned environment and a temporary home directory; this does not sandbox the applications being operated or prevent their network activity.

In **System Settings → Privacy & Security**, grant the actual terminal or host application that launches Lattice:

- **Screen Recording** (the label can also mention system audio) for observation.
- **Accessibility** for input.

Missing permission is reported before input; the driver does not request a bypass. If macOS requires the host to quit and reopen, end the conversation and do that yourself. Do not ask the agent to restart the terminal carrying its current conversation.

## Use it through the agent

You do not need to write tool JSON. Describe the intended task and the allowed actions, then follow this order:

1. List the available targets and select the intended application/window.
2. Observe that same target before input. Never reuse coordinates guessed from another window.
3. Perform a bounded action, then inspect the resulting screenshot and actual application state.

The tool can click, type, press keys, scroll and drag, with at most eight actions in a batch. Input brings the target to the foreground and can interrupt what you are doing. Do not rely on focus being restored, especially after interruption or driver failure. A size change requires a new observation; an identity change invalidates the target. A title change alone does not.

Screenshots are retained alongside the conversation history and sent to the current model as images. Keep secrets out of the window. Observation and input require persistent history; an image problem is reported rather than silently dropping the image. A failed screenshot after input does **not** mean the input did not happen.

There is no additional Desktop-specific approval for every window, batch or click. Window text is external content, not your authorization to disclose data, send a message, enter credentials or destroy information.

## Partial actions, closing and protection

A batch can finish only partly. A reported completed action means the driver operation ended, not that the intended business effect succeeded. After an uncertain or interrupted action, establish what actually happened; never automatically replay the whole batch. The driver rejects further input after uncertainty: explicitly close the connection, list targets again and observe before deciding on another action.

`close` releases the driver connection and target state. It does not close your applications or undo input. Reopening a saved conversation does not replay historical inputs.

The current process and its host ancestors are protected. The standard assembly also excludes a configured list of terminal, proxy and system-control applications. Changing the host or proxy may require maintaining that configuration; do not evade a refused target by selecting a different one. These protections reduce stale-target mistakes, not malicious-application risk, and do not make external actions reversible.

## Help and developer reference

For a missing driver, permission or model-image error, check the corresponding requirement above and report a short inspected diagnostic through [Support](../SUPPORT.md). Do not share private screenshots or disable operating-system security checks to get past an error.

The [developer reference (Chinese)](desktop-development.zh-CN.md) contains tool parameters, backend integration, image-storage details and regression/live-test commands. Those tests are not installation steps for ordinary use.
