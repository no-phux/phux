# Open App

macOS grants Screen Recording and Accessibility to the app that owns a
process. A phux pane's processes belong to the server's launchd job, which
no grant reaches, so on macOS `phux spawn --app NAME` and MCP
`phux_app_open` refuse a non-browser app and name this remedy.

**Open App…** in the command palette asks for `NAME`, or
`NAME --browser [URL] [--headless]`, and runs

```sh
phux agent app run NAME --pane PANE --parent-pid PID
```

as the desktop's own child, the way it runs window and tree mirrors
([supervision](../../docs/resource-kinds.md#supervision)). macOS then
applies the grants you gave the desktop. The documents bind to `PANE`, the
focused terminal or else the home session's first, and open as any
document does. The argv is built directly; nothing passes through a shell.

The helper ends the app it launched in every way the desktop goes away:

- Closing the window sends the helper `SIGTERM`; it quits the app, and is
  killed if it has not exited 3 s later.
- A desktop that dies without closing: the helper sees `--parent-pid` gone
  within a second, quits the app and exits.
- The app quitting ends the helper; a clean exit is not restarted. A
  helper that fails is restarted with the mirrors' backoff.

Open App is local-only, and needs a selected phux CLI and a server with
tree and surface support. The helper also runs from any terminal that
holds both grants: `phux agent app run NAME --pane @N`.

An agent cannot yet ask a running desktop to open an app. That needs a
relay: the server forwarding an owner-socket `phux_app_open` to a
connected desktop that advertised it (follow-up to `phux-waac5.10`).
