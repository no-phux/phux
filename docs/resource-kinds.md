---
group: build
order: 6
---

# Resource kinds beyond terminals

How phux hosts resources that are not terminals (an accessibility tree, a
window's pixels, a browser page, a typed document) and how to produce one.
The kind catalog, with each kind's methods and grants, is
[kinds](./reference/kinds.md).

## Available now

Besides `Terminal` and `AgentSession`, the host carries three
whole-document kinds:

- `AccessibilityTree`: an application's or page's tree as JSON.
- `PixelSurface`: a window's, display's or page's frames, PNG or JPEG.
- `Document`: whole documents of one content type fixed at open
  ([typed documents](#typed-documents)).

Each is a pane child, survives restore and reconnect, and is named for what it observes (`--app Safari`,
`display`, a page's title) or by its `--name`. Clients draw a document by
its type: images as images, JSON pretty-printed, Markdown through the
[Markdown viewer](./config.md#markdown-viewer), other text as source (HTML
is not rendered). A client attaches only the kinds its `HELLO` lists, so a
document-only client needs no Terminal codec.

Document streams are read-only. Typing, paste and resizing do not reach the
source; input goes to the producer as typed intents
([producers](#producers)). Closing a placement leaves the producer and other
viewers running.

Prefer meaning over pixels: an application integration (DevTools for a
browser) first, then the accessibility tree, then pixels. Agents read and
drive documents over MCP: `phux_documents` lists them with their `control`,
`phux_tree_snapshot` and `phux_tree_wait` read a tree,
`phux_surface_snapshot` returns a frame, and the `phux_document_*` tools send
one intent each ([parity](./reference/parity.md)). The CLI equivalents are
`phux agent tree|surface|doc snapshot|follow` and `phux agent page
navigate|click|type|key|scroll|drag|paste|copy`.

## Typed documents

A content type is ASCII `type/subtype` with optional `;` parameters, at
most 255 bytes, fixed for the resource's life. A publish is at most 4 MiB
and must match its type family: `image/png` must be a PNG,
`application/json` must parse, `text/*` and `+xml`/`+json` must be UTF-8;
any other type is opaque bytes. The bootstrap (`DocumentTypedV1`) is one
length byte, the content type, then the newest document, so a late viewer
learns the type.

## Publish your own kind

Any program adds a kind without a client change: open a typed `Document`
under a pane and publish whole documents. `phux agent doc serve` is the
contract over a pipe, from any language. It opens the document under
`--pane`, or its own pane, and prints JSON lines:

- `{"event": "opened", "resource": "@12", "parent": "@3", ...}` first;
- `{"event": "input", "intent": ...}` per viewer intent: `navigate` (`url`),
  `click` (`x`, `y`), `type` (`text`), `key` (`key`), `scroll` (`dx`, `dy`),
  `paste` (`text`, `chord`), `activate` (`node`), `drag` (`from`, `to`).

A typed document declares no control class, so only the owner socket sends
it input. Each stdin line `{"text": ...}` or `{"base64": ...}` replaces the
document; a refused publish is reported on stderr. Across `phux upgrade` it
redials and republishes the same resource. End of stdin ends it, leaving the
document `producer: "gone"`.

```python
import json, subprocess, time
p = subprocess.Popen(["phux", "agent", "doc", "serve", "--type", "text/markdown",
                      "--name", "clock"], stdin=subprocess.PIPE, text=True)
while True:
    p.stdin.write(json.dumps({"text": "# " + time.strftime("%T")}) + "\n")
    p.stdin.flush()
    time.sleep(1)
```

Rust programs can link `phux-client`'s `tree` module instead.
Two producers ship built in: `phux agent doc folder PATH` (a directory
listing that `navigate` walks, never above PATH) and `phux agent doc
notifications` (the host's recent desktop notifications; Linux only).

## Standing producers

`[[producers]]` in `config.toml` keeps a command running as a producer.
Each entry has a `name` and a `command` argv; a first word `phux` is the
server's own binary. It runs in its own session, `producer-<name>`,
restarts with backoff (1 s doubling to 60 s), survives `phux upgrade`, and
follows `phux config reload`. Do not name your own sessions `producer-*`.

```toml
[[producers]]
name = "downloads"
command = ["phux", "agent", "doc", "folder", "/home/me/Downloads"]
```

## Applications

`phux spawn --app NAME` (MCP `phux_app_open`) opens a pane running
`phux agent app run NAME`, which launches the app, mirrors it against that
pane, and ties their lifetimes together:

- `--browser [URL]`: a Chromium-family browser with a fresh temporary
  profile and a debugging port; its first page is mirrored (control `page`).
- macOS: `open -n -a NAME` starts a new instance, never an existing one; its
  first window (control `window`) and its tree are mirrored.
- Linux: NAME is a command or `.desktop` id run in the foreground; the tree
  is read by pid, and on Hyprland the surface is the pid's window.

The app quitting closes the pane and its documents. Killing the pane quits
only the app the helper launched (a browser's profile is removed). A pane
holds no macOS capture grant, so on macOS `phux spawn --app` refuses a
non-browser app: run `phux agent app run NAME --pane @N` from a terminal
that holds Screen Recording and Accessibility instead.

## Agents clicking elements

`phux agent tree click TREE --node ID` or `--name TEXT [--role ROLE]` (MCP
`phux_click_element`) acts on an element instead of a pixel. Tree mirrors
give every node an `id` (child-index path such as `0.3.1`, or a page's
DevTools `nodeId`) and a `frame` in screen points. The click sends an
`Activate` intent; the producer presses the node (`AXPress`, AT-SPI
`DoAction(0)`, or a DevTools click). If the tree refuses it, or with
`--pixel`, the node's centre is clicked on the surface beside the tree. App
trees take input of class `window`, so the same `input@window` grant
applies.

## Standing mirrors

`[[mirrors]]` in `config.toml` keeps displays and windows mirrored. Each
entry names a `phux agent surface mirror` target (`display`,
`display:OUTPUT`, `App`, `App:title`) and runs in a pane of its own session,
`mirror-<target>`, restarted with backoff like a standing producer and
reconciled by `phux config reload`. Do not name your own sessions
`mirror-*`. On macOS a server's children cannot hold Screen Recording, so
Rune, while open, runs the mirrors into those panes (`phux agent surface
standing`).

## Picking a screen or window

The `MIRROR` command lists what a host can mirror (`Targets`) and starts or
stops a mirror on demand (`Start`, `Stop`) in the same `mirror-<target>`
session. Rune's palette and the phone apps use it; `phux agent surface
targets` prints the same list. `Start` waits up to 20 s for the first frame.
Listing, and starting or stopping any non-display target, needs
`observe@window`; a `display` target needs `observe@display` (append
`:HOST` for a satellite). On macOS it needs Rune
open on that host, else `UNAVAILABLE`.

## Mirror formats and platforms

Frames are PNG or JPEG, told apart by signature. Page mirrors default to
JPEG, window and display mirrors to PNG; `--format`, `--quality` and
`--max-width` are in [cli](./reference/cli.md#phux-agent-surface-mirror). A
frame over 4 MiB switches a PNG mirror to JPEG; a JPEG still too large is
refused, so lower `--max-width`. A window or display mirror captures every
`--interval`, faster for two seconds after input; a page sends a frame on
each repaint.

On Linux neither mirror needs a grant. `phux agent tree mirror APP` reads
AT-SPI over D-Bus. `phux agent surface mirror` captures with `grim` on a
wlroots compositor (Hyprland, Sway); window targets need Hyprland. Input
uses the wlroots virtual-pointer and virtual-keyboard protocols, falling
back to `wtype` and `wlrctl` or `ydotool`. On macOS the window mirror needs
Screen Recording, and input needs Accessibility, both held by the app that
started it ([macOS permissions](./install.md#macos-permissions)).

While input arrives the host notifies who is in control and logs each
burst with the sender's credential, never the text.

## Supervision

A document's producer is the connection that opened it. `phux ls --json`,
`phux resource show --json` and `phux_documents` report `producer:
"connected"`, or `"gone"` once it disconnects; the document keeps its last
frame. A publish to a gone document re-binds the publisher as producer.

`phux upgrade` carries every document whose parent pane crosses, as
`producer: "gone"`. A running mirror or `doc serve` redials for up to a
minute and republishes, re-binding the same resource. A surface whose
capture stops yielding frames (display asleep, session locked) keeps its
last frame and retries.

A server restart ends producers. A mirror run as a pane has a recorded
command, so `phux restore` relaunches it into fresh documents; old document
records are listed as refused, and `phux rm ^KEY` forgets one. `--parent-pid
PID` on a mirror exits within a second of `PID` ending.

## Authority

A new kind starts denied. A scoped grant opens a kind only once its spawn
contract opts in (`scoped_child`), and then with `CREATE` on the parent's
Group and `BIND` on the parent. Window and display grants are separate from
terminal grants and from each other: a display shows every app on the
machine. Who may watch and control which class is in
[authority](./authority.md).

## The seam

A kind's engine owns its semantics and codecs. phux owns negotiation,
stream and bootstrap identity, ordering and fences, bounds, routing and
tombstones. A kind supplies its `ResourceApp` (state, codec, frames and
commands, what crosses `phux upgrade`) and one `KindSpec` row in the catalog:
gate, methods, events, metadata keys, its stream contract (codec tags,
[delivery mode](#delivery-modes), geometry, cursor resume) and its spawn
contract (parent, `provider` and `native_id`, refused fields, its own
checks, `scoped_child`). The decoder, authorization classifier, hub relay,
session attach and client kernel read those contracts instead of matching
on kinds.
Wire `terminal_id` fields carry any kind's resource id.

## Delivery modes

| Mode | Kinds | Catch-up |
| --- | --- | --- |
| Log | `AgentSession` records | resume from a cursor, or an explicit gap |
| Raw stream | Terminal PTY bytes | a gap fences the generation and rebootstraps |
| Replicated state | Terminal StateSync | deltas against the viewer's acknowledged reference |
| Whole document | `AccessibilityTree`, `PixelSurface`, `Document` | the newest document; a lapped viewer is recut |

Replicated state never skips bytes: each viewer gets deltas against what it
acknowledged (`phux_resource::replication::ConsumerSync`). A whole-document
stream takes no `FRAME_ACK`; a slow viewer skips intermediate documents. A
client that sees no recut within 30 s after a hub fence re-attaches, and
reports the document lost after three empty re-attaches.

## Producers

A projection's parent supervises its producer. Only a Terminal can parent
(`ResourceKind::requires_parent`, `can_parent`), so every producer runs in a
pane and inherits supervision, logs and upgrade survival. Encoding happens
in the producer; the server stays a fenced relay and does no pixel work.
`SEND_DOCUMENT_INPUT` is queued on the producer's own connection as an
unjournaled `document_input` event. Once the producer is gone the intent is
refused, never held. On macOS the capture grants belong to the app that
starts the producer, not the server.
