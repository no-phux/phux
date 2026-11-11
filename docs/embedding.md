# Embed phux

Embed phux in your own app with one of two crates. A Rust toolkit uses
`phux-client-runtime` and paints its `draw::DrawList`; the in-repo example is
[Phux Rune](../clients/rune/README.md), the desktop client. A Swift, C or other
native app links `phux-client-ffi` and paints `phux_client_terminal_draw_list`.
Both run phux's libghostty replica in your process and share one set of
paint rules ([ADR-0189](https://github.com/no-phux/phux/blob/3cd9ddac22075c1b3db2674e2edc4e00c91d67f4/docs/adr/0189-the-terminal-view-is-a-crate-and-paint-rules-are-shared.md)).
None of these crates is on crates.io yet; depend on them by path or git.

---

## Pick a route

| Your app                                               | Depend on                  | You write                                                     |
| ------------------------------------------------------ | -------------------------- | ------------------------------------------------------------- |
| A Rust UI (Rune, egui, iced, Slint, a custom renderer)  | `phux-client-runtime`      | Map cells to pixels and draw the [draw list](#the-draw-list). |
| Swift, Objective-C, C, C++, Zig                        | `phux-client-ffi` (C ABI)  | Connect, poll on wake, paint the C draw list.                  |
| A phone app                                            | `phux-client-ffi` (UniFFI) | See [phones](./clients.md#phones).                            |

Every route starts the same way: open a client, attach a session, and create a
view of one terminal. phux does the rest of the terminal: the replica,
reconnect, acknowledged input, history, selection and search.

## Frames, wakes, and input

One listener and one event drain exist per runtime client. Drain, then
invalidate affected terminals. A removed slot clears stale presentation even
when its generation equals the cached number. Never publish a cached frame as
evidence of recovery.

Raw-send success is a local queue result, not delivery. Unknown delivery is
terminal-wide. Clear its fence (`acknowledge_projection_if`) only once the
frame carrying that fence has been presented on a visible, unclipped
terminal; building or painting a frame is not presentation, and a late
receipt must not clear a newer fence. Phux Rune does this in
`clients/rune/src/desk/frame.rs`.

## Any Rust toolkit

Enable the runtime's `engine` feature. This sketch separates connection from
UI callbacks; `options.control.attach` must select the intended disposable
server session. Keep the returned client/view alive between callbacks.

```rust
use phux_client_runtime::draw::{DrawList, Theme, Visibility};
use phux_client_runtime::publication::Rgb;
use phux_client_runtime::runtime::RuntimeError;
use phux_client_runtime::{Client, ClientOptions, Runtime, Target, ViewId};

fn connect(socket: std::path::PathBuf, options: ClientOptions) -> Result<Client, RuntimeError> {
    Runtime::connect(Target::uds(socket), options)
}

// After attached/readiness, call client.create_view(&terminal_id)? once.
// On UI invalidation, acquire the latest immutable frame:
fn draw(client: &Client, view: ViewId, theme: &Theme<Rgb>) -> Option<DrawList<Rgb>> {
    let frame = client.acquire_view(view)?;
    Some(DrawList::build(&frame, theme, Visibility::default()))
}
// On closing that view, call client.destroy_view(view)? once.
```

`Theme<C>` is generic over your colour type: give it a `fn(Rgb) -> C` and
optional theme colours, and the list arrives in your colours. Send input
through `Client::with_control` the way Phux Rune's `desk/input.rs` does, and
resize the terminal from the view that owns its size.

### Inline images

Image capabilities default to empty: text-only and ABI consumers must not
advertise protocols they cannot paint. A Rust renderer that paints Kitty images
adds the matching `phux-protocol` dependency and opts in before connecting:

```rust
options.control.image_protocols = phux_protocol::caps::ImageProtocolSet::with(&[
    phux_protocol::caps::ImageProtocol::KittyGraphics,
]);
```

Renderers consume `GridFrame.images` alongside the text draw list. Each
placement borrows immutable, shared RGBA pixels through an `Arc<ImageData>`,
with signed cell-space geometry, pixel-space source/crop rectangles and a signed
z-order. Apply `sample_source` to texture filtering independently of the visible
`source` slice, so Unicode placeholder cells do not introduce sampling seams.
Layer them by z-order: planes below `z = -1073741824` paint beneath
non-default cell backgrounds, other negative planes beneath glyphs, and
nonnegative planes above text. An explicitly set background masks the lowest
plane even if its RGB equals the theme default. Placeholder glyphs and their
decorations do not paint, but their backgrounds still do. Clamp filtering to
the original source crop so fractional sizes do not bleed neighbouring pixels.
Report actual cell pixels through `Client::resize_terminal_with_cell` so native
image sizes and offsets match the renderer.

Kitty consumers negotiate synthesized Raw/StateSync bootstrap: native GHOSTSNP
does not include the engine's image/placement registry. Synthesized replay
includes unplaced payloads and virtual prototypes for later live placements.
External file/shared-memory media stay disabled. The C/Swift draw-list ABI
is still text-only; no image fields were added to it.

## C and Swift

[`include/phux/client.h`](../crates/phux-client-ffi/include/phux/client.h)
is the C ABI. For product connections use `phux_client_connect`: the runtime
owns socket, HELLO, framing and reconnect. Post its wake to your event loop,
then call `phux_client_poll` on the owning thread. Never feed frames, queue
a second HELLO or drain outgoing bytes on a connected handle. Fully
initialize the thread-safe wake context **before** `phux_client_connect`: a
registration wake may call it synchronously before the client is returned.
It must only post asynchronously to the event loop, never call the ABI
inline or wait for a mutex held across connect. Start polling only after
connect returns the handle; keep the context valid until free completes.

`phux_client_new` plus `feed_frame`/`outgoing_*` is the separate embedded
lane for offline use, harnesses and fault injection: the host then owns
dialing, framing, reconnect and bounds. The lanes are mutually exclusive.

Every root release attaches the
`PhuxFFI` xcframework (macOS and iOS, header included); on other platforms
build the static library with `just ffi-release`. Call
`phux_client_terminal_draw_list(client, &terminal, &options, &list)` after
output arrives and paint the records it lends, which stay valid until your
next mutable call on the client. The low-level `phux_client_terminal_grid`
and `phux_client_terminal_grid_metadata` are still there for a renderer that
wants every cell.

### Documents and non-terminal leaves

An arrangement leaf, and a `phux_client_catalog_terminal_get` entry, is any
resource but an `AgentSession`: a terminal, an accessibility tree, a pixel
surface, a typed document, or a kind the header does not name. Read
`resource_kind` before creating a terminal view; only a terminal has a grid.
`unarranged_count` counts terminals only. Use the header and library from
one build: a bridge older than `resource_kind` leaves it undefined.
A read never prunes these leaves, so a write never loses another client's
placement. A document kind delivers whole documents instead: attach it with
`phux_client_queue_attach_resource`, then read `PHUX_CLIENT_EFFECT_DOCUMENT`
effects. `bytes` holds the content type (`status_code` bytes long, empty for
a tree or surface), then the document; show the last one delivered, not the
highest `seq`. In Rust these
are `Event::DocumentPublished` and `Topology::resources`
([resource kinds](./resource-kinds.md)).

## Documents, trees, and surfaces

A session can hold resources that are not terminals
([resource kinds](./resource-kinds.md#available-now)). Each arrives whole:
the runtime emits `Event::DocumentPublished`, whose `DocumentPublication`
carries the resource id, its kind, the publish sequence, the bytes (JSON
for a tree, PNG or JPEG for a surface, the content type of a typed
document) and that content type. Show the last one delivered, not the
highest `seq`. The event is live-only, and there is no grid or replica to
paint.

UniFFI's `RemoteClient` projects the same event as
`WireEvent::DocumentPublished`, with the document's `control` class
(`page`, `window` or `display`). Offer input only when `control` is set:
`send_document_input(resource_id, intent)` returns a request id that a
`WireEvent::DocumentInputResult` settles, or `0` when nothing was sent.

The C ABI delivers the same documents as `PHUX_CLIENT_EFFECT_DOCUMENT`
effects ([above](#documents-and-non-terminal-leaves)).

## Runtime consumer contract

Native Rust consumers normally use the runtime's **Fed** delivery: the
driver feeds the control plane, and the UI consumes events and immutable
`GridFrame` publications. A binding with owning-thread per-frame catalog
state (the C ABI) uses **Queued** delivery: the driver retains inbound bytes,
and the owner drains/polls and decodes them. Do not build another dialer to
move the decode point. Queues and frame publications are bounded.

Use `take_observation` to drain events and sample status, topology, failure
and `connection_epoch` under one lock. An identical terminal id after
reconnect is not the same observation. For queued batches, carry the drained
epoch and check `accepts_inbound(epoch)` under the same control guard used
to feed **each** frame; reconnect can retire a batch after it was drained.
Caller-validated recovery reads inventory and validates stable identities
before attaching, never blindly adopts reused handles. Deliberate lease
takeover is spent once, not repeated automatically on reconnect. Geometry
comes from authoritative frames; the designated size-owning view sends
resize intent, while observers do not create resize feedback loops.

A listener is a coalesced wake, not an event payload. Drain then invalidate
only affected views. Driver notifications run on the runtime thread;
registration (`set_listener`) and explicit `wake`/`close` can invoke the
listener synchronously on their caller's thread. All run outside runtime
locks, not necessarily outside your locks. Initialize context before
registration, make it thread-safe, and only post asynchronously to the UI;
never block or re-enter the client from a callback.
`with_control` is non-reentrant and must not block or call back into the
client. Dropping the last `Client` closes the driver, retires callbacks,
waits out callbacks on other threads, and joins the driver (never joins
itself from its own callback). Free callback context only after retirement;
C `phux_client_free` performs that stop/join.

Immutable Rust grid frames can outlive a later publication. C grid/draw/effect
pointers instead last only until the next mutable call: copy before retaining.
A pointer borrow ending is **not delivery acceptance**. Ordinary event drains
accept agent publications; leased drains retain their journal allocation
until explicit `accept_agent_publication` (C: `phux_client_effect_clear`), unless
a newer full retained cut supersedes earlier same-resource publications.
Resume appends preserve their unaccepted prefix. Remaining unaccepted
publications replay after replacement once catalog/attach are ready.
This acceptance is also not the presentation receipt that clears an
unknown-input fence; see [Frames, wakes, and input](#frames-wakes-and-input).

The [runtime API](../crates/phux-client-runtime/src/runtime.rs),
[publication delivery seam](../crates/phux-client-runtime/src/control/agent_delivery.rs),
[epoch observations](../crates/phux-client-runtime/tests/it/control_plane/observation.rs)
and [recovery tests](../crates/phux-client-runtime/tests/it/control_plane/recovery.rs)
own these contracts. The C header owns ABI versions, open enums, thread and
borrow lifetimes; tolerate additive enum members rather than inventing a
new binding vocabulary.

## The draw list

Positions are cells; you choose the cell size. Paint in this order:

1. Fill the grid with `background`.
2. Fill each background run.
3. A solid cursor: fill its cells. The glyphs on it already have the cell's
   background colour.
4. Each text run: one character per column from `col`, at your cell width,
   not the font's advance, in its face and colour.
5. Each block cell: fill its regions, given in eighths of the cell.
6. Each decoration: underline styles, strikethrough and overline across
   `start..end`.
7. Any other cursor: a bar, an underline, or a hollow block.

A faint item paints at half alpha. The colours are final: the theme, palette
provenance, inverse video and selection are applied, and a hidden or
blinked-off cell has no text.

## Build requirements

`libghostty-vt` downloads its prebuilt archive only in `ReleaseFast`. Set
`LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast"` under `[env]` in your
`.cargo/config.toml`, as this repository does, or have Zig on `PATH`.
