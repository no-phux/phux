---
group: build
order: 7
---

# Embed phux

Embed phux in a Rust app with `phux-client-runtime`: it runs the libghostty
replica in your process and hands you a `draw::DrawList` to paint.
[Phux Rune](../clients/rune/README.md) is the in-repo example; phone apps use
the same runtime through `phux-client-ffi`'s UniFFI surface
([phones](./clients.md#phones)). The crates are not on crates.io; depend by
path or git.

## Any Rust toolkit

Enable the runtime's `engine` feature. `options.control.attach` selects the
session. Keep the client and view alive between callbacks.

```rust
use phux_client_runtime::draw::{DrawList, Theme, Visibility};
use phux_client_runtime::publication::Rgb;
use phux_client_runtime::runtime::RuntimeError;
use phux_client_runtime::{Client, ClientOptions, Runtime, Target, ViewId};

fn connect(socket: std::path::PathBuf, options: ClientOptions) -> Result<Client, RuntimeError> {
    Runtime::connect(Target::uds(socket), options)
}

// Once attached: client.create_view(&terminal_id)?. On invalidation:
fn draw(client: &Client, view: ViewId, theme: &Theme<Rgb>) -> Option<DrawList<Rgb>> {
    let frame = client.acquire_view(view)?;
    Some(DrawList::build(&frame, theme, Visibility::default()))
}
// On close: client.destroy_view(view)?.
```

`Theme<C>` maps `Rgb` to your colour type. Send input through
`Client::with_control` as Rune's `desk/input.rs` does; resize from the view
that owns the terminal's size.

Build needs `LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast"` under `[env]` in
`.cargo/config.toml` (the only mode with a prebuilt archive), or Zig on
`PATH`.

## The draw list

Positions are cells; you choose the cell size. Paint in order:

1. Fill the grid with `background`.
2. Fill each background run.
3. A solid cursor: fill its cells.
4. Each text run: one character per column from `col`, at your cell width.
5. Each block cell: its regions, in eighths of a cell.
6. Each decoration across `start..end`.
7. Any other cursor shape.

Faint items paint at half alpha. Colours are final: theme, inverse,
selection, and hidden or blinked-off text are already applied.

## Inline images

Image protocols default to none; advertise only what you paint:

```rust
options.control.image_protocols = phux_protocol::caps::ImageProtocolSet::with(&[
    phux_protocol::caps::ImageProtocol::KittyGraphics,
]);
```

Placements arrive in `GridFrame.images`. Filter with `sample_source`, clamped to the source crop,
so placeholders and fractional sizes do not seam or bleed. Layer by z:
below `-1073741824` under non-default cell backgrounds (an explicit
background masks it even if it equals the default), other negatives under
glyphs, nonnegative above text. Placeholder glyphs do not paint; their
backgrounds do. Report real cell pixels with
`Client::resize_terminal_with_cell`. File and shared-memory media are disabled.

## Frames, wakes, and input

There is one listener and one event drain per client. Drain, then invalidate
only the affected views. A removed slot clears stale presentation even when
its generation matches the cache. Never present a cached frame as proof of
recovery.

Raw-send success means queued, not delivered. Unknown delivery is
terminal-wide: clear it with `acknowledge_projection_if` only after the frame
carrying that fence was presented on a visible, unclipped terminal. Building
or painting is not presenting, and a late receipt must not clear a newer
fence. Rune does this in `clients/rune/src/desk/frame.rs`.

## Documents, trees, and surfaces

Non-terminal [resources](./resource-kinds.md) publish whole into the same
slot table, newest wins. On `Event::DocumentPublished`, read with
`Client::acquire_document_at(resource_id, generation)`; `None` means a newer
wake follows, so no document is copied twice. Show the last one read, not
the highest `seq`. One wake per resource waits undrained and is never
dropped by the queue cap. A slot retires when its resource closes,
detaches, or is released, or the server or engine restarts. Without the
`engine` feature, documents are dropped.

UniFFI's `WireEvent::DocumentPublished` carries a `control` class (`page`,
`window`, `display`). Offer input only when it is set:
`send_document_input(resource_id, intent)` returns a request id settled by
`WireEvent::DocumentInputResult`, or `0` if unsent.

## Runtime consumer contract

- Do not build another dialer; the driver decodes every frame. Queues and
  publications are bounded.
- `take_observation` drains events and samples status, topology, failure,
  and `connection_epoch` under one lock. The same terminal id after a
  reconnect is not the same observation: validate stable identities before
  attaching, and spend a lease takeover once, never on every reconnect.
- Geometry comes from server frames. Only the size-owning view sends resize.
- `close_terminals` needs a current handshake (and a live organization
  snapshot on an organization peer). It returns a request id, or `None`
  (`0` in UniFFI); `TerminalsClosed` settles it, possibly partially. Watch
  pane lifecycle events for actual closure
  ([close intents](./organization.md#close-intents)).
- The listener is a coalesced wake. It runs on the runtime thread, or
  synchronously on the caller of `set_listener`, `wake`, or `close`, outside
  runtime locks but maybe inside yours. Make its context thread-safe, post
  to the UI, and never block or re-enter the client. `with_control` is
  non-reentrant.
- Dropping the last `Client` retires callbacks, waits out running ones, and
  joins the driver. Free callback context after that.
- Agent records are a log, not slots: `Event::AgentPublication` in order.
  An undrained publication replays once after reconnect unless a newer full
  cut supersedes it. Accepting it is not a presentation receipt.

[`runtime.rs`](../crates/phux-client-runtime/src/runtime.rs),
[`agent_delivery.rs`](../crates/phux-client-runtime/src/control/agent_delivery.rs),
and the [observation](../crates/phux-client-runtime/tests/it/control_plane/observation.rs)
and [recovery](../crates/phux-client-runtime/tests/it/control_plane/recovery.rs)
tests own these contracts. Tolerate new enum members.
