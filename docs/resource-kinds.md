# Resource kinds beyond terminals

Hosting resources that are not terminals: an application's accessibility tree,
a window's pixels, a browser page, a typed document. Everything up to
[Not done](#not-done) is implemented. The wire and host rules also live in
their home pages ([architecture](./architecture.md),
[protocol](./protocol.md), [decisions](./decisions.md)) and this page links
to them. [Not done](#not-done) lists the gaps; [History](#history) links the
work that built this.

## Available now

The host supports Terminal, AgentSession, AccessibilityTree, PixelSurface and
Document. A Document carries a content type fixed when it is opened, so a
producer publishes Markdown, HTML, SVG, CSV, JSON or images without a new
kind or a wire change; `phux agent doc open|publish|snapshot|follow` drive
one ([protocol](./protocol.md#commands-events-resources)).
Membership and aggregate attach are kind-neutral but stream only the kinds a
client's `HELLO` lists. Apps supply bounded captures; the host publishes the
attach barrier before live output or lifecycle closes. AgentSession is listed
at attach; its records wait for an explicit, cursor-bearing `ATTACH_RESOURCE`,
so it neither gates aggregate readiness nor sends an unsolicited replacement.
Document-only and agent-only sessions can attach without a Terminal codec.
The server links only the pure Terminal contract; the executable
supplies the concrete engine ([architecture](./architecture.md#where-to-look)).

Phux Rune discovers tree/surface resources before the first frame,
renders plain JSON or letterboxed PNG or JPEG frames, and distinguishes pending, empty and invalid
documents. It draws a Document by its content type: PNG, JPEG, GIF and
WebP as images, JSON pretty-printed, text and `+xml`/`+json` as source
(Markdown and HTML are not rendered), anything else as its size and type.
A tree or surface is named for what it observes (`--app Safari`, `--window
display`, a mirrored page's title) and a typed document for its `--name`;
that name is the tab title in Rune, `name` in `phux ls --json`
resources and MCP `phux_documents`, and `title` in `phux resource show`. Documents can occupy mixed or document-only arrangements and survive
restore/reconnect. Their streams are **read-only**: typing, paste, terminal grid
resizing and file uploads do not control their source. Input goes to the
producer instead: `SEND_DOCUMENT_INPUT` relays one typed intent to the
connection that opened the document ([protocol](./protocol.md#commands-events-resources)),
and `phux agent page navigate|click|type|key|scroll` drive a mirrored browser page
this way. The same verbs drive a macOS `phux agent surface mirror`: it posts
clicks, text, keys and scrolls as system input at the window's or display's
screen points (raising a window's app first), which needs the Accessibility
grant for the app that started it; `--dry-run-input` logs the events
instead. Rune and mobile send input to a surface pane
([authority](./authority.md#in-rune)). Closing a document placement leaves its producer and
other viewers running.

Phux Rune offers mirror startup actions. These launch a dedicated
supervised producer with argv, not commands typed into a person's shell.
Browser mirroring needs an existing Chrome DevTools endpoint; macOS capture
needs the relevant system permission. Local-only startup actions refuse remote
or unsupported platforms. Moving or reopening a view keeps the source on its
original host; it is not browser interactivity or cross-host process migration.

Frame formats, the Linux producers and what the host shows while input
arrives are in [Mirror formats and platforms](./mirrors.md).

Agents read documents like panes, over MCP or the pi, Oh My Pi and OpenCode
plugins: `phux_documents` lists them, `phux_tree_snapshot` and
`phux_tree_wait` read a tree's JSON, and `phux_surface_snapshot` a frame
([parity](./reference/parity.md)). MCP agents drive one with `phux_document_navigate`,
`phux_document_click`, `phux_document_type`, `phux_document_key` and
`phux_document_scroll`, one tool per intent. The loop: find a surface and its
`control` in `phux_documents`, look at it with `phux_surface_snapshot`, click
at pixel coordinates read off that image, and snapshot again to confirm. A
document with no `control` refuses input, and a connection other than the
owner socket needs an `input@CLASS` grant ([authority](#authority)).

## Supervision

The producer of a document is the connection that opened it. While that
connection lives, the document reports `producer: "connected"` in
`phux ls --json` resources, `phux resource show --json` and MCP
`phux_documents`; once it disconnects the document stays with its last
frame and reports `"gone"`. Other kinds omit the field (`null` in
`resource show`). On the wire it is `RESOURCE_STATE` field `PRODUCER`
(`u8`, `1` connected, `0` gone), absent from an older server. A restarted
producer opens a fresh document. A gone document is re-bound only by a
publish to it: the publishing local connection becomes its producer and
receives its input.

`phux upgrade` carries every document whose parent pane crosses: same id,
same record, its newest document and `control`, with `producer: "gone"`.
A running `phux agent surface|tree|page mirror` redials
when its connection closes (backing off, for up to a minute) and publishes
again, which re-binds it, so viewers keep the same resource. A surface
capture that yields no frame (the display asleep, the session locked, a
refused grant) keeps the surface and its last frame and retries, backing
off to 30 s; only the first capture, before the surface exists, ends the
mirror when it fails.

A server restart (not `phux upgrade`, which keeps every pane running)
ends producers with every other program. A mirror run as a pane
(`phux new -- phux agent page mirror ...`, or a Rune startup action) is an
ordinary pane with a recorded command, so `phux restore` relaunches it
and it opens fresh documents (`phux service install --restore` keeps the
panes; it does not run `phux restore`). The old document records cannot
be relaunched: `phux restore` lists them as refused, and `phux rm ^KEY`
forgets one.

A mirror can also run as the child of the app that holds the macOS grants
(Rune, for standing mirrors). `phux agent tree|surface|page mirror
--parent-pid PID` exits within a second of process `PID` ending, so a
parent killed without dropping its children leaves no producer behind.

## Standing mirrors

A host can keep displays and windows mirrored without anyone running a
mirror by hand. Each `[[mirrors]]` entry in `config.toml` names a
`phux agent surface mirror` target (`display`, `display:OUTPUT`, `App`,
`App:title`), with optional `format`, `quality`, `max-width` and
`interval-ms`. The server runs each in a pane of its own ephemeral
session, `mirror-` plus the target (`mirror-display`,
`mirror-display-hdmi-a-1`). The pane is the surface's parent, so clients
list the surface under that session and a `display` or `window` grant
applies as usual. A mirror that exits is restarted after a backoff (1 s,
doubling to 60 s; a run over a minute resets it). `phux upgrade` keeps the
pane and the mirror re-adopts its surface. A cold start recreates it.
`phux config reload` starts new entries, restarts changed ones, closes
`mirror-*` sessions no entry names. Do not name sessions of your own
`mirror-*`.

On macOS a server's children cannot hold Screen Recording, so the pane
only waits and Rune, while open, runs the mirrors into it as children
(`phux agent surface standing`); the grant prompts name Rune. An older
server refuses a config with the table; upgrade first.

## Applications

`phux spawn --app NAME` (MCP `phux_app_open`) launches an application as a
resource: a pane runs `phux agent app run NAME`, which starts the app, runs
the mirrors against that pane, and ties their lifetime to the app. The
launch, lifetime and platform rules are in [Applications](./applications.md).

## Typed documents

A `Document`'s content type is ASCII `type/subtype` with optional `;`
parameters, at most 255 bytes; any other value fails the `SPAWN_RESOURCE`
frame. It is fixed for the resource's life. A publish (at most 4 MiB) is
checked by type family: `image/png` must be a PNG, `application/json` must
parse, `text/*` and `+xml`/`+json` must be UTF-8, and any other type is
opaque bytes. The `DocumentTypedV1` bootstrap payload is one length byte,
the content type, then the newest document (empty before the first
publish), so a late viewer learns the type; each `RESOURCE_OUTPUT` is a
whole document of that type.

## The seam

A kind's engine owns its semantics and binary codecs. phux owns connection
negotiation, stream and bootstrap identity, ordering and fences, resource
bounds, routing and tombstones.

A kind supplies:

- its state and a codec for it (VT bytes, a record, a whole document);
- the frames and commands it owns, through its `ResourceApp`;
- its row in the kind catalog (`KindSpec`): gate, methods, events and
  metadata keys;
- its geometry, in its own units: cells for a Terminal, the newest image's
  own size in pixels for a `PixelSurface`.

phux supplies, the same for every kind: identity, authorization from the
kind catalog, generations and fences, sequence numbers, bounded mailboxes,
gap recovery and tombstones. Any resource in a session's arrangement can be
a layout leaf. The Terminal invariant "one PTY, one authoritative geometry"
holds.

Wire fields named `terminal_id` carry the resource id of any kind. Renaming
them in source is free, because the wire matches field ids, not names.

## Delivery modes

Each kind delivers in one or more of four modes.

| Mode | Kinds | Catch-up |
| --- | --- | --- |
| Log | `AgentSession` records | resume from a cursor, or an explicit gap |
| Raw stream | Terminal PTY bytes | a gap fences the generation and rebootstraps |
| Replicated state | Terminal StateSync | deltas against the viewer's acknowledged reference |
| Whole document | `AccessibilityTree`, `PixelSurface`, `Document` | the newest document; a lapped viewer is recut from it |

Replicated state gives each viewer its own reference, so a phone on a slow
link and a desktop on a LAN get different deltas from one source. The
newest state wins because a delta is computed against what the viewer
acknowledged, not because bytes are dropped: "never silently skip bytes"
holds for every stream. The per-viewer engine (sequence numbers, ack
acceptance, RTT, cadence, loss tolerance) is
`phux_resource::replication::ConsumerSync`, generic over the kind's
reference type. The Terminal actor (`phux-terminal`, `actor/sync.rs`)
supplies its cell reference and synthesizer.

A whole-document stream takes no `FRAME_ACK`. Each viewer gets the newest
document, so a slow viewer skips only intermediate documents
([protocol](./protocol.md#commands-events-resources)).

## Producers

OpenCode owns its conversations; phux owns a projection of them (the
`AgentSession` record stream), the guardian's lifecycle, and an
authenticated, fenced control channel. That composition is the template for
every external source. macOS owns windows; phux owns the view ("mirror
window X", "the accessibility tree of app Y") and the helper that produces
it. A window that disappears is the view's exit, not a resource phux failed
to keep.

A projection's parent supervises its producer and carries the activation
the producer is checked against. The binding names no kind: the
organization checks structure, that a projection has a parent and that the
parent can supervise (`ResourceKind::requires_parent`, `can_parent`; only a
Terminal can). At spawn the app checks the parent is fit, its handle
carrying the `AgentParent` companion (agent sessions and documents alike),
and checks a guardian against the parent record's activation. So a producer
runs in a Terminal, as the ACP guardian does, and inherits supervision, logs
and upgrade survival.

Encoding happens where the state is, so the server stays a fenced relay and
its single-threaded runtime does no pixel work. Viewer input reaches the
producer as typed intents: `SEND_DOCUMENT_INPUT` is a command, and the
server queues it on the producer's own connection as an unjournaled
`document_input` event, so the producer receives it beside its publishes
without polling. The producer is the connection that opened the document,
or the one that re-bound it ([supervision](#supervision)); once it is gone
the intent is refused, never held.

On macOS the Screen Recording and Accessibility grants belong to the app
that starts the producer, not to the server: a server's children cannot
hold Screen Recording, so Rune runs standing mirrors as its own children
([standing mirrors](#standing-mirrors)). The server's own grants belong to
**phux Server**, its signed bundle, run through `Versions/Current`
([macOS permissions](./install.md#macos-permissions)).

## Semantic first

phux ranks agent evidence with the screen as the floor
([agents](./agents.md#agent-states)). Applications get the same ladder:

1. An application integration, such as the Chrome DevTools Protocol for a
   browser.
2. The accessibility tree.
3. Pixels.

Pixels are for a person watching and the last resort for an agent. An
agent reads a tree with
[`phux snapshot` and `phux wait`](./scripting.md#reading-a-screen),
`phux agent tree snapshot|follow`, or MCP `phux_tree_snapshot` and
`phux_tree_wait`; producing one on macOS needs the Accessibility grant, not
Screen Recording. A browser page has no kind of its own: `phux agent page
mirror` drives one over the DevTools Protocol and publishes its
accessibility tree into an `AccessibilityTree` and its screencast into a
`PixelSurface` under one pane, so `phux agent tree snapshot` and `phux agent
surface snapshot` read it like any other. Navigation and input are typed intents to the page's
producer, addressed to either of its documents.

## Authority

The kind catalog stays the one table for discovery and authorization, so a
new kind starts denied. Observing a display shows every application on the
machine, and input to it is control of the machine, so window and display
grants are separate from terminal grants and from each other. A device paired
to watch terminals does not inherit a desktop. Control classes, who may watch
and control a page, window or display, and what Rune's panes
send are in [Authority over pages, windows, and displays](./authority.md).

## Not done

Beads epic `phux-rbz0y` tracks the work to make every layer kind-neutral.
Each gap below holds on current `main`:

- **Producer feedback.** The server relays typed intents only. Per-viewer
  acknowledgements, viewport and RTT do not reach a producer.
- **A PTY-less producer.** Every producer runs in a Terminal pane,
  `phux agent app run` included. A supervised process without a PTY is
  added only if a PTY proves costly.
- **The kind catalog.** `KindSpec` does not yet own a kind's stream
  contract, spawn rule, geometry or input vocabulary; a new kind still edits
  closed matches in `phux-protocol`, the client kernel and the hub relay
  (`phux-rbz0y.7`).
- **Runtime publication.** The client kernel keeps one lane per resource
  behind a single dispatch, and the runtime keeps one subscription per
  resource with a per-kind policy, but the published-frame table holds
  grids only: documents still arrive as events (`phux-rbz0y.21`).
- **Clients.** The terminal UI shows a non-terminal pane as a placeholder
  tile, not its content ([clients](./clients.md)). phux-web drops document
  publications (`phux-rbz0y.10`).
- **Upgrade.** Terminals, agent sessions and documents cross `phux upgrade`
  through separate paths in the upgrade blob, not through `ResourceApp`
  (`phux-rbz0y.13`).

## History

[krd](https://github.com/zzjcool/krd) paints a Mac desktop into a terminal
with kitty graphics and reads windows as text through the accessibility API.
Run inside a phux pane, it showed where phux assumed every resource was a
terminal. Beads epic `phux-phiio` (closed) made the host kind-neutral and
added the document kinds:

- the design: [#1906](https://github.com/no-phux/phux/pull/1906);
- a kind-neutral substrate and host, with stream and input frames routed
  through `ResourceApp`: [#1909](https://github.com/no-phux/phux/pull/1909),
  [#1910](https://github.com/no-phux/phux/pull/1910),
  [#1912](https://github.com/no-phux/phux/pull/1912),
  [#1913](https://github.com/no-phux/phux/pull/1913),
  [#1933](https://github.com/no-phux/phux/pull/1933);
- the per-consumer replication engine, `ConsumerSync`:
  [#1914](https://github.com/no-phux/phux/pull/1914);
- lagged `AgentSession` consumers recovered instead of losing records
  (`phux-o5e95`): [#1915](https://github.com/no-phux/phux/pull/1915);
- kitty images sent to the outer terminal once (`phux-hjvbk`):
  [#1916](https://github.com/no-phux/phux/pull/1916);
- per-stream codec negotiation: [#1950](https://github.com/no-phux/phux/pull/1950);
- `AccessibilityTree`: [#1955](https://github.com/no-phux/phux/pull/1955),
  [#1960](https://github.com/no-phux/phux/pull/1960),
  [#1961](https://github.com/no-phux/phux/pull/1961);
- `PixelSurface`: [#1970](https://github.com/no-phux/phux/pull/1970),
  [#1971](https://github.com/no-phux/phux/pull/1971),
  [#1976](https://github.com/no-phux/phux/pull/1976);
- document streams in clients: [#1973](https://github.com/no-phux/phux/pull/1973);
- a browser page over CDP: [#1975](https://github.com/no-phux/phux/pull/1975).

Later work added document input
([#2021](https://github.com/no-phux/phux/pull/2021),
[#2060](https://github.com/no-phux/phux/pull/2060),
[#2074](https://github.com/no-phux/phux/pull/2074)), the typed `Document`
kind ([#2023](https://github.com/no-phux/phux/pull/2023),
[#2053](https://github.com/no-phux/phux/pull/2053)), applications as
resources ([#2068](https://github.com/no-phux/phux/pull/2068)) and standing
mirrors run by Rune on macOS
([#2152](https://github.com/no-phux/phux/pull/2152)).
