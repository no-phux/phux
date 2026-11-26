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
use phux_client_runtime::engine::{EngineError, ViewSource};
use phux_protocol::ids::ResourceId;
use phux_client_runtime::publication::Rgb;
use phux_client_runtime::runtime::RuntimeError;
use phux_client_runtime::{Client, ClientOptions, Runtime, Target, ViewId};

fn connect(socket: std::path::PathBuf, options: ClientOptions) -> Result<Client, RuntimeError> {
    Runtime::connect(Target::uds(socket), options)
}

// Capture using the server identity from the original qualified observation,
// not a newer identity read by a delayed callback.
fn source_for_intent(
    client: &Client,
    terminal: &ResourceId,
    observed_server: Option<&[u8]>,
    observed_connection_epoch: u64,
) -> Result<ViewSource, EngineError> {
    client.view_source(terminal, observed_server, observed_connection_epoch)
}
// Keep that source through a deferred admission: client.create_view(&source)?.
// On invalidation:
fn draw(client: &Client, view: ViewId, theme: &Theme<Rgb>) -> Option<DrawList<Rgb>> {
    let frame = client.acquire_view(view)?;
    Some(DrawList::build(&frame, theme, Visibility::default()))
}
// On close: client.destroy_view(view)?.
```

`Theme<C>` maps `Rgb` to your colour type. Send input through
`Client::with_control` as Rune's `desk/input.rs` does; resize from the view
that owns the terminal's size.

### Creation and view lifetime

`Client::create_session(name, lifetime, launch)` is exact creation: a colliding
name is a host refusal, never an implicit attach. `submit_spawn` creates a terminal
in the selected or explicitly addressed session without changing local selection.
Their `WorkflowSubmission` distinguishes `NotQueued` (no accepted work), `Queued`
(exact request id), and `Journaled` (retained operation id and optional spawn
request id). Neither queue nor journal acceptance means host commit or execution.
Reconcile `OrganizationOperation` under the original operation after a lost
receipt; an `Unknown` outcome never authorizes a new create.

UniFFI's terminal-spawn methods return `WireWorkflowSubmission`, not a zero/void
sentinel. `spawn_terminal_in_directory` requires the named existing session and
does not create it as an unreported fallback or select it. Session creation returns
the journaled operation text; `WireEvent::OrganizationOperation` carries settlement
and the exact committed session key.
Queued spawns settle as `TerminalCreation` / `WireTerminalCreationOutcome`:
`LiveTarget`, `Refused`, or `Unknown`. The older `TerminalSpawned` event remains
only a compatibility projection; never infer refusal from its error text.

`ensure_session(name, lifetime, launch)` instead resolves existing versus created
inside one host operation. Its exact session key and `ensured_created` flag arrive
on the retained committed receipt. A peer without `SESSION_ENSURE` is explicitly
unsupported, with no local lookup/create race and no uncorrelated attach fallback.
Ensuring an existing session does not launch another terminal or change selection.

Runtime `ControlOptions.attach` and `attach_session` are lookup-only. The frozen
wire `CreateIfMissing` form remains a transport compatibility edge, not an implicit
runtime ensure. Submit ensure explicitly, then select the receipt's durable key.
Across a server restart, automatic recovery waits for a new organization seed and
the same authority/key; a reused session name or numeric id is not a substitute.

Selecting a view is local presentation, not create/attach/terminate.
`destroy_view` releases only that view; sibling views and durable execution remain.
Release subscription interest separately when its last consumer leaves.
`view_source` binds the original observed server id and connection epoch to a
renderable publication slot and its exact stream/bootstrap generation. Retain the resulting `ViewSource`
through delayed callbacks; `create_view(&source)` validates it on the engine owner
when admission actually runs. A stale source is `StaleViewSource`, not permission
to look up a newer execution using the same numeric id. An unavailable replica
is `ProjectionUnavailable`, with no installed view. A genuinely retained offline
source remains usable without a live socket; `None` as the expected server is
valid only for an owner that really has no negotiated HELLO.

`close_terminals` is an explicit execution-termination intent, not a view-close
callback. Failed creation never gains an implicit cleanup kill: any compensation
must explicitly fence the exact created activation.

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
