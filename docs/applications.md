# Applications as resources

How phux launches an application as a resource instead of mirroring one
that already runs. Part of [resource kinds beyond terminals](./resource-kinds.md).

`phux spawn --app NAME` (MCP `phux_app_open`) spawns a pane running
`phux agent app run NAME`, which launches the application, runs the
mirrors above in-process against its own pane, and ties their lifetime
to the app. The pane is the supervisor
([producers](./resource-kinds.md#producers)).

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

## Publish your own kind

Any program can add a resource kind without a client change: open a typed
`Document` under a pane, publish whole documents of one content type, and
every viewer draws it by that type
([resource kinds](./resource-kinds.md#available-now)): PNG, JPEG, GIF and
WebP as images, JSON pretty-printed, Markdown, CSV, SVG and other text as
source. A publish is at most 4 MiB and must match its type.

`phux agent doc serve --type MIME [--name NAME] [--pane @N]` is the
contract, spoken over a pipe from any language. It opens the document
under `--pane`, or the pane it runs in (`PHUX_TERMINAL_ID`), and prints
JSON lines on stdout:

- `{"event": "opened", "resource": "@12", "parent": "@3", ...}` first.
- `{"event": "input", "intent": "navigate", "url": ...}` for each intent
  a viewer sends: `navigate` (`url`), `click` (`x`, `y`), `type` (`text`),
  `key` (`key`), `scroll` (`dx`, `dy`), `paste` (`text`, `chord`),
  `activate` (`node`), `drag` (`from`, `to`, each `[x, y]`). Viewers send them with `phux agent page
  navigate|click|type|key|scroll DOC ...` or the MCP `phux_document_*`
  tools; only the owner socket may, since a typed document has no
  `control` class. `serve` refuses a clipboard copy itself.

Each stdin line `{"text": "..."}` or `{"base64": "..."}` replaces the
document. A refused publish is reported on stderr and serving goes on.
When `phux upgrade` closes the connection, `serve` redials for up to a
minute and publishes the newest document again, which re-adopts the same
resource ([supervision](./resource-kinds.md#supervision)). The end of
stdin ends it, and the document stays with `producer: "gone"`.

```python
import json, subprocess, time
p = subprocess.Popen(["phux", "agent", "doc", "serve", "--type", "text/markdown",
                      "--name", "clock"], stdin=subprocess.PIPE, text=True)
while True:
    p.stdin.write(json.dumps({"text": "# " + time.strftime("%T")}) + "\n")
    p.stdin.flush()
    time.sleep(1)
```

A Rust program can link `phux-client` instead: `tree::open_document`,
`tree::publish_collecting` (which keeps intents relayed meanwhile) and
`tree::intent` over `Connection::recv` are what `serve` is built on.

Two producers ship built in:

- `phux agent doc folder PATH` lists a directory as Markdown, rereading
  it every `--interval` seconds (default 2) and publishing on change. A
  `navigate` intent opens a subdirectory or `..`, never above PATH. The
  listing names the `phux download HOST:PATH` that fetches a file.
- `phux agent doc notifications` keeps the host's last `--keep` (default
  50) desktop notifications, newest first. On Linux it monitors the
  session bus for freedesktop `Notify` calls. macOS does not let other
  programs read Notification Center, so there it refuses.

## Standing producers

`[[producers]]` in `config.toml` keeps any command running as a producer,
the way [`[[mirrors]]`](./resource-kinds.md#standing-mirrors) keeps
mirrors. Each entry has a `name` and a `command` argv. A first word
`phux` is the server's own binary. It runs in a pane of its own ephemeral
session, `producer-` plus the name, so it opens its documents under that
pane. It is restarted after a backoff (1 s doubling to 60 s), kept running
across `phux upgrade`, recreated on a cold start, and added, restarted or
closed by `phux config reload`. Do not name sessions of your own
`producer-*`.

```toml
[[producers]]
name = "downloads"
command = ["phux", "agent", "doc", "folder", "/home/me/Downloads"]
```
