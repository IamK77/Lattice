# Desktop operation

[简体中文](desktop.zh-CN.md) · [Documentation](README.md) · [Developer reference (Chinese)](desktop-development.zh-CN.md)

`Desktop` lets Lattice view and operate applications on your computer, including clicking buttons, filling fields and scrolling. On macOS, install the driver, grant system permissions and select a model with image input; see [Model configuration](model-configuration.md). You can use ordinary text conversations while preparing these tools.

## Install the driver and grant system permissions

This guide describes configuration with **Cua Driver 0.26.1**. Install the driver separately; Lattice starts it when the desktop tool is used. Obtain it from the publisher and check the version, checksum, archive paths and application signature before running it. After changing driver versions, use a small task to check observation and input.

The default driver entry is `~/.lattice/drivers/desktop`, which can point to an installed `CuaDriver.app/Contents/MacOS/cua-driver`. Inspect an existing entry's target first. Alternatively, set the driver location in the same terminal **before starting Lattice**:

```bash
export LATTICE_DESKTOP_DRIVER="/path/to/CuaDriver.app/Contents/MacOS/cua-driver"
```

For a custom assembly, its `executable` setting takes precedence over this environment variable.

In **System Settings → Privacy & Security**, enable these permissions for the terminal or host application that starts Lattice:

- **Screen Recording**, sometimes labeled to include system audio, for viewing windows.
- **Accessibility** for clicks and input.

The tool reports missing permissions. If macOS asks you to reopen the application, end the current conversation and reopen the terminal yourself to avoid cutting off an active session.

## Use it through the agent

Tell Lattice which application to use and what you want to accomplish. For example:

> Look at this application's settings window and tell me which options are available. For this step, inspect it and wait for my confirmation before making changes.

Follow this order:

1. List windows and select the target application and window.
2. View a screenshot of that window to establish its current state.
3. Perform a small batch of actions, then check the screenshot and the result in the application.

The tool supports clicks, typing, keys, scrolling and dragging, with up to eight actions in a batch. Input brings the target window to the foreground; switch back manually when you want to continue your own work. Observe again after a size change. Select a new target after its identity changes; a title change alone allows continued use.

Screenshots are saved beside the conversation records and sent to the current model. Put away keys and private content before observation. Desktop operates your real local applications, which continue to read, write and use the network. Desktop uses the existing authorization flow, without a separate approval dialog for each click. Explicitly confirm the task and scope before sending messages, entering credentials or deleting data.

## Partial actions, closing and protection

### After interruption

A batch may have completed only partly. Input may also have happened when the screenshot taken afterward fails. Check the application's actual state before choosing the next action.

When input state is uncertain, the driver pauses further input. Have Lattice close the driver connection, list windows again and observe before continuing. Completed changes and external sends remain in effect.

### Closing and returning to a conversation

`close` releases the driver connection and window records. Applications remain open and existing input stays in place. Reopening a saved conversation reads its history without repeating past desktop actions. Observe the current window before continuing.

### Protected windows

The current Lattice process and its host ancestors are protected. The default configuration also excludes specified terminals, proxies and system-control applications to avoid cutting off the conversation. When a target is refused, inspect the reason and configuration. Check that exclusion list after changing hosts or proxies too.

## Help and developer reference

For driver, permission or image errors, check the relevant steps above. For help, send [Support](../SUPPORT.md) the error and reproduction steps with private content removed; inspect screenshots before sharing them too.

See the [developer reference (Chinese)](desktop-development.zh-CN.md) for tool parameters, backend integration, image storage and tests.
