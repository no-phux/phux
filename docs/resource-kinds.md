# Resource kinds beyond terminals

Hosting resources that are not terminals: an application's accessibility tree,
a window's pixels, a browser page. The available contract below is implemented;
later sections retain the remaining target design tracked by Beads `phux-phiio`.
Where a part has landed, the live contract also lives in its home page
([architecture](./architecture.md), [protocol](./protocol.md),
[decisions](./decisions.md)) and this page links to it.

## Available now

The host supports Terminal, AgentSession, AccessibilityTree, PixelSurface and
Document. A Document carries a content type fixed when it is opened, so a
producer publishes Markdown, HTML, SVG, CSV, JSON or images without a new
kind or a wire change; `phux agent doc open|publish|snapshot|follow` drive
one ([protocol](./protocol.md#commands-events-resources)).
Session membership and aggregate attach are kind-neutral. Apps supply bounded
captures; the host publishes the complete attach barrier before releasing live
output or lifecycle closes. AgentSession is catalogued at session attach, but
its app defers records to explicit, cursor-bearing `ATTACH_RESOURCE` requests.
It does not gate aggregate readiness or emit an unsolicited full replacement.
Document-only and agent-only sessions can attach without a Terminal codec.
The server links only the pure Terminal contract; the executable
supplies the concrete engine ([architecture](./architecture.md#where-to-look)).

Phux Rune discovers tree/surface resources before the first frame,
render plain JSON or letterboxed PNGs, and distinguish pending, empty and invalid
documents. Documents can occupy mixed or document-only arrangements and survive
restore/reconnect. Their streams are **read-only**: typing, paste, terminal grid
resizing and file uploads do not control their source. Input goes to the
producer instead: `SEND_DOCUMENT_INPUT` relays one typed intent to the
connection that opened the document ([protocol](./protocol.md#commands-events-resources)),
and `phux agent page navigate|click|type|key` drive a mirrored browser page
this way. Rune and mobile do not send input yet. Closing a document placement
leaves its producer and other viewers running.

Phux Rune offers mirror startup actions. These launch a dedicated
supervised producer with argv, not commands typed into a person's shell.
Browser mirroring needs an existing Chrome DevTools endpoint; macOS capture
needs the relevant system permission. Local-only startup actions refuse remote
or unsupported platforms. Moving or reopening a view keeps the source on its
original host; it is not browser interactivity or cross-host process migration.

Agents read documents over MCP the way they read panes: `phux_documents`
lists them, `phux_tree_snapshot` and `phux_tree_wait` read a tree's JSON, and
`phux_surface_snapshot` returns a surface's PNG as MCP image content
([parity](./reference/parity.md)).

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

## Why

[krd](https://github.com/zzjcool/krd) paints a Mac desktop into a terminal
with kitty graphics and reads windows as text through the accessibility API.
Run inside a phux pane it exposed where phux still assumes every resource is
a terminal:

- The command plane is already per kind. A kind is a `ResourceApp`, and the
  kind catalog routes and authorizes its commands
  ([architecture](./architecture.md#where-to-look)).
- The stream plane is per kind. Input, `FRAME_ACK`, history and
  resize frames now reach their resource's app through `ResourceApp::frame`
  ([decisions](./decisions.md)), and `HELLO` admits a client with no
  terminal profile. `VIEWPORT_RESIZE` remains a per-connection Terminal
  vote; document image dimensions are pixels, not cells. Native layout leaves
  can be documents without creating a Terminal engine view.
- Per-consumer delivery (StateSync: a reference per viewer, deltas against
  it, acknowledgements, RTT-adaptive cadence) lives inside the terminal actor.
- A producer process is modelled as a Terminal: an `AgentSession` needs a
  Terminal parent, and its guardian is authenticated by that Terminal's
  process id.
- Images are stored decoded and re-sent raw on every TUI paint (`phux-hjvbk`).
- A lagging `AgentSession` consumer silently loses records (`phux-o5e95`).

## The seam

The native terminal design (`phux-slogic`) already states the split for one
kind: the engine owns terminal semantics and binary codecs; phux owns
connection negotiation, stream and bootstrap identity, ordering and fences,
resource bounds, routing and tombstones. This design makes that the rule for
every kind.

A kind supplies:

- its state and a codec for it (VT bytes, a record, a tree delta, encoded
  tiles);
- the frames and commands it owns, through its `ResourceApp`;
- its capability record, negotiated when a stream binds;
- its geometry, in its own units.

phux supplies, the same for every kind: identity, authorization from the
kind catalog, generations and fences, sequence numbers, bounded mailboxes,
acknowledgements, RTT, gap recovery and tombstones.

## Delivery modes

A kind declares one or more of three modes. All three exist today; none is
new.

| Mode | Today | Catch-up |
| --- | --- | --- |
| Log | `AgentSession` records | resume from a cursor, or an explicit gap |
| Raw stream | Terminal PTY bytes | a gap fences the generation and rebootstraps |
| Replicated state | Terminal StateSync | deltas against the viewer's acknowledged reference |

Replicated state is the mode for anything that changes faster than a viewer
can follow. Each viewer has its own reference, so a phone on a slow link and
a desktop on a LAN get different deltas from one source. The newest state
wins because a delta is computed against what the viewer acknowledged, not
because bytes are dropped: "never silently skip bytes" stays true for every
stream.

The per-viewer engine (sequence numbers, ack acceptance, RTT, cadence, loss
tolerance) moves out of the terminal actor into `phux-resource`, generic over
the kind's reference type (`phux-phiio.2`). The terminal keeps its cell
reference and synthesizer.

## Rules this changes

Each lands with the code that implements it, as an edit to
[decisions](./decisions.md):

- "Server to client is VT bytes; both sides run libghostty" becomes "each
  kind declares its stream contract; the Terminal's is VT bytes and
  libghostty on both ends" (landed with `phux-phiio.3`).
- Stream and input frames reach the kind that owns their resource through
  `ResourceApp`, as commands already do. A kind owns no frame it did not
  opt into (`phux-phiio.1`, landed; the rule is in
  [decisions](./decisions.md)).
- Codec negotiation moves from the connection to the stream. A connection
  with no terminal profile is admitted; a Terminal stream it cannot decode
  is refused when it binds (`phux-phiio.3`, landed; the rule is in
  [decisions](./decisions.md) and [protocol](./protocol.md#handshake)).
- Geometry is per resource in units the kind defines, and any resource with
  a surface can be a layout leaf (`phux-phiio.5`). The terminal invariant
  "one PTY, one authoritative geometry" is unchanged. On the server a
  `PixelSurface`'s geometry is its newest image's own size, in pixels, and
  an arrangement leaf already names any member resource. The session kernel
  hands every document on whole (a bootstrap cut at READY, each live frame
  as a newer document), and the desktop app adopts a session's surfaces as
  leaves and draws the newest frame letterboxed in the card.
- Wire fields named `terminal_id` keep their ids. Renaming them in source is
  free because the wire matches field ids, not names.

## Producers

OpenCode owns its conversations; phux owns a projection of them (the
`AgentSession` record stream), the guardian's lifecycle, and an
authenticated, fenced control channel. That composition is right and is the
template for every external source. macOS owns windows; phux owns the view
("mirror window X", "the accessibility tree of app Y") and the helper that
produces it. A window that disappears is the view's exit, not a resource
phux failed to keep.

Two parts change (`phux-phiio.4`):

- The producer binding stops naming a kind (landed with `phux-phiio.4`:
  a parent is fit when its handle carries the `AgentParent` companion, and
  the guardian is checked against the parent record's activation). The rule
  that matters is that a projection's parent supervises its producer and
  carries the activation the producer is checked against. A helper keeps running in a Terminal, as the ACP guardian does, so
  it inherits supervision, logs and upgrade survival; a PTY-less supervised
  process is a later kind, added only if a PTY proves costly.
- The server relays per-viewer feedback to the producer: acknowledgements,
  viewport and RTT, besides typed intents. Encoding happens where the
  state is, so the server stays a fenced relay and its single-threaded
  runtime does no pixel work. Typed intents landed with the browser page
  mirror (`phux-xvibq.2`): `SEND_DOCUMENT_INPUT` is a command, and the
  server queues it on the producer's own connection as an unjournaled
  `document_input` event, so the producer receives it without polling
  beside its publishes. The producer is the connection that spawned the
  document; once it is gone the intent is refused, never held. Per-viewer
  acknowledgements, viewport and RTT land with the first producer that
  consumes them.

A macOS helper holds the Screen Recording and Accessibility grants. The
system ties those grants to a stable signed binary, so the helper is not the
server binary that `phux upgrade` replaces, and it keeps running across an
upgrade.

## Semantic first

phux already ranks agent evidence with the screen as the floor
([agents](./agents.md#agent-states)). Applications get the same ladder:

1. An application integration, such as the Chrome DevTools Protocol for a
   browser.
2. The accessibility tree.
3. Pixels.

Pixels are for a person watching and the last resort for an agent. The first
non-terminal kind is therefore a replicated accessibility tree
(`phux-phiio.6`; the kind, its engine and its stream landed, see
[protocol](./protocol.md#commands-events-resources); `phux snapshot`,
`phux wait` and the macOS producer follow): `phux snapshot` and `phux wait`
for an application, with no
Screen Recording grant. A pixel surface follows (`phux-phiio.7`), drawn first
by the desktop app, then a browser page over CDP (`phux-phiio.8`), whose
screencast is already acknowledged per frame. A page needed no kind of its
own: `phux agent page mirror` drives one over the DevTools Protocol and
publishes its accessibility tree into an `AccessibilityTree` and its
screencast into a `PixelSurface` under one pane, so `phux agent tree
snapshot` and `phux agent surface snapshot` read it like any other.
Navigation and input did not need one either: they are typed intents to
the page's producer, addressed to either of its documents.

## Authority

The kind catalog stays the one table for discovery and authorization, so a
new kind starts denied. Observing a display shows every application on the
machine, and input to it is control of the machine, so window and display
grants are separate from terminal grants and from each other. A device paired
to watch terminals does not inherit a desktop. Until such grants exist,
`SEND_DOCUMENT_INPUT` has its own rule that only the owner socket meets: no
scoped or remote grant, whatever its terminal verbs, can send input to a
page or window.

## Order

| Step | Beads |
| --- | --- |
| Stop re-sending unchanged images | `phux-hjvbk` |
| Never lose `AgentSession` records silently | `phux-o5e95` |
| Route stream and input frames through `ResourceApp` | `phux-phiio.1` |
| Generic per-viewer replication engine | `phux-phiio.2` |
| Per-stream capability negotiation | `phux-phiio.3` |
| Producer identity and feedback | `phux-phiio.4` |
| Per-resource geometry and non-terminal leaves | `phux-phiio.5` |
| Accessibility-tree kind | `phux-phiio.6` |
| Pixel surface kind | `phux-phiio.7` |
| Browser kind | `phux-phiio.8` |

The first four change no Terminal behavior and need no new kind. Each later
step is tested by the kind that needs it, not built ahead of one.
