# Applications as resources

How phux launches an application as a resource instead of mirroring one
that already runs. Part of [resource kinds beyond terminals](./resource-kinds.md).

`phux spawn --app NAME` (MCP `phux_app_open`) spawns a pane running
`phux agent app run NAME`, which launches the application, runs the
mirrors above in-process against its own pane, and ties their lifetime
to the app. The pane is the supervisor, per `phux-phiio.4`.

- `--browser [URL]` runs the Chromium-family browser NAME (`chrome`,
  `chromium`, a bundle name or a path) as the helper's child, with a fresh
  profile in the temp directory and `--remote-debugging-port=0`, and mirrors
  its first page (control `page`). `--headless` keeps it off screen.
- Otherwise, on macOS, `open -n -a NAME` starts a new instance. The helper
  adopts the one pid that appeared after the launch and refuses when
  several did. It never attaches to a running instance. It mirrors that
  pid's first window (control `window`) and its tree.
- On Linux NAME is a command, or a `.desktop` id whose `Exec` runs. It
  runs as the helper's child and must stay in the foreground. The tree is
  read by pid; on Hyprland the surface is the pid's window class.

The app quitting ends the helper, so the pane closes and its documents
close with it. Killing the pane (`SIGHUP`, `SIGTERM` or `SIGINT` to the
helper) quits the app the helper launched and nothing else. A macOS app
is asked to quit and may prompt to save. A child gets `SIGTERM`. A browser
that lingers is killed, and its profile is removed. The pane's title is
the app's name. A producer that cannot start, such as a macOS capture
without its grant, is logged in the pane; the app and the other
producer go on. `--dry-run` prints what would be launched.
