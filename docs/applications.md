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

## macOS apps outside a pane

On macOS a pane holds no capture grant, so `phux spawn --app` and
`phux_app_open` refuse a non-browser app. Run
`phux agent app run NAME --pane @N` from a terminal that holds Screen
Recording and Accessibility instead; the documents still bind to `@N`.
`--parent-pid PID` ends the helper, and the app it launched, within a
second of `PID` exiting.

## Capture cadence and input

A window or display mirror (`phux agent surface mirror`) captures every
`--interval` (default 1000ms) while nobody sends it input. An input intent
brings the next capture forward to 50ms after the event is posted, then
the mirror captures every 150ms for two seconds after the last input
before it falls back to the interval. The fast interval is measured from
the end of the previous capture, so a slow capture still leaves 150ms of
rest between two. A capture identical to the last one is not published,
and on macOS an unchanged capture skips the `sips` re-encode.

A page mirror needs none of this: Chrome's screencast sends a frame when
the page repaints, so a click shows in the next frame.

## Agents clicking elements

An agent can act on an element by its node instead of by a pixel.
The macOS and Linux tree mirrors give every node an `id` (the path of
child indices from the root, `0.3.1`) and a `frame` in screen points.
A page's `DevTools` tree uses each node's `nodeId` as its id.

`phux agent tree click TREE --node ID`, or `--name TEXT [--role ROLE]`
(MCP `phux_click_element`), finds one node. An exact name match beats a
substring match. When several nodes tie, the error lists their ids. The
click sends an `Activate` intent to the tree. Its producer presses the
node with `AXPress` on macOS (or clicks its centre when the node has no
press action), `DoAction(0)` over AT-SPI, or a `DevTools` click at the
element's box. App trees take input of class `window`, so the intent
needs the same `input@window` grant (or the owner socket) as a click on
the surface. If the tree refuses the intent, or with `--pixel`, the node's
centre is clicked on the window surface beside the tree instead. The point
is mapped from the window's frame into frame pixels, which accounts for
Retina scale and `--max-width`. `--wait-until TEXT [--timeout SECS]` then
follows the tree until a document contains TEXT.

## Picking a screen or window

`MIRROR` lists what a host can mirror and starts one on demand, in the
same `mirror-<target>` session a `[[mirrors]]` entry gets
([standing mirrors](./resource-kinds.md#standing-mirrors)). Rune's palette
("Mirror a screen or window…") and the phone apps list every host's
targets, start the pick and open its surface.

- `Targets` answers `{"targets": [{target, kind, label}]}`: `display`,
  each output and each mapped window from Hyprland (`hyprctl monitors`,
  `clients`; another compositor offers `display`), or the main display and
  on-screen windows on macOS. `phux agent surface targets --json` prints
  the same list.
- `Start` answers `{"session", "surface"}`, waiting up to 20 s for the
  first frame (`surface` is null if none came). Starting a running or
  configured target returns it. The mirror restarts like a standing one
  and lasts until `Stop`, or until the server restarts or upgrades.
- A satellite named as the host answers for its own screens.
- Listing needs `observe@window[:HOST]`; starting or stopping needs
  `observe@display` for a `display` target, else `observe@window` (with
  `:HOST` for a satellite). No broader grant reaches either.
- On macOS the server cannot see window titles or capture, so Rune's
  runner (`phux agent surface standing`) reports the targets and runs the
  mirrors. Without Rune open there, both are refused `UNAVAILABLE`.
