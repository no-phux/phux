# Embed a phux terminal in your app

A GPUI app adds the `phux-gpui` view. Another Rust toolkit uses
`phux-client-runtime` and paints its `draw::DrawList`. A Swift, C or other
native app links `phux-client-ffi` and paints `phux_client_terminal_draw_list`.
All three run phux's libghostty replica in your process and share one set of
paint rules ([ADR-0189](https://github.com/no-phux/phux/blob/3cd9ddac22075c1b3db2674e2edc4e00c91d67f4/docs/adr/0189-the-terminal-view-is-a-crate-and-paint-rules-are-shared.md)).
None of these crates is on crates.io yet; depend on them by path or git.

---

## Pick a route

| Your app                                               | Depend on                  | You write                                                     |
| ------------------------------------------------------ | -------------------------- | ------------------------------------------------------------- |
| GPUI (gpui-fast 0.1.9)                                 | `phux-gpui`                | Wake GPUI from the runtime and place the view.                |
| Another Rust UI (egui, iced, Slint, a custom renderer) | `phux-client-runtime`      | Map cells to pixels and draw the [draw list](#the-draw-list). |
| Swift, Objective-C, C, C++, Zig                        | `phux-client-ffi` (C ABI)  | Feed transport bytes, paint the C draw list.                  |
| A phone app                                            | `phux-client-ffi` (UniFFI) | See [phones](./clients.md#phones).                            |

Every route starts the same way: open a client, attach a session, and create a
view of one terminal. phux does the rest of the terminal: the replica,
reconnect, acknowledged input, history, selection and search.

## GPUI

[`clients/desktop/native/phux-gpui`](../clients/desktop/native/phux-gpui/README.md)
is the terminal view the phux desktop is built on. `examples/embed.rs` is a
whole app in one file:

Use an explicit disposable server socket and a non-production profile, not
an inherited pane socket. From the repository root, first run
`just desktop-source` to materialize the workspace's pinned GPUI sources.
The example opens a window; run it only in an authorized graphical session.
That server must already have a session named `embed-test` containing a
terminal; substitute its actual session name in the final argument if different.

```sh
# /tmp/phux-embed-test.sock must belong to your disposable test server.
unset "${!PHUX_WS_@}" # Bash: clear inherited WebSocket credentials/TLS settings
export PHUX_PROFILE=embed-test
export PHUX_SOCKET=/tmp/phux-embed-test.sock
cargo run --locked --manifest-path clients/desktop/native/Cargo.toml \
  -p phux-gpui --example embed -- "$PHUX_SOCKET" embed-test
```

`Terminal::new` does not own or destroy its runtime `ViewId`. When closing
a view on a shared `Client`, the host calls `client.destroy_view(view)`;
dropping the GPUI entity alone leaves that view registered. Sibling views
and their shared client remain alive.

It builds on the published gpui-fast. Its `patched-gpui` feature adds the
retained glyph sprites, drawable-presented receipts and macOS key repeat
that phux's own GPUI patches provide.

## Any Rust toolkit

```rust
use phux_client_runtime::{Runtime, Target, ClientOptions};
use phux_client_runtime::draw::{DrawList, Theme, Visibility};

let client = Runtime::connect(Target::uds(socket), options)?;
// After Event::StatusChanged(Status::Attached):
let view = client.create_view(&terminal_id)?;
// On every Event::TerminalChanged:
if let Some(frame) = client.acquire_view(view) {
    let list = DrawList::build(&frame, &theme, Visibility::default());
    // fill list.background, then each run, block, decoration and the cursor
}
```

`Theme<C>` is generic over your colour type: give it a `fn(Rgb) -> C` and
optional theme colours, and the list arrives in your colours. Send input
through `Client::with_control` the way `phux-gpui`'s `input` module does, and
resize the terminal from the view that owns its size.

## C and Swift

`include/phux/client.h` is the C ABI. Every root release attaches the
`PhuxFFI` xcframework (macOS and iOS, header included); on other platforms
build the static library with `just ffi-release`. Call
`phux_client_terminal_draw_list(client, &terminal, &options, &list)` after
output arrives and paint the records it lends, which stay valid until your
next mutable call on the client. The low-level `phux_client_terminal_grid`
and `phux_client_terminal_grid_metadata` are still there for a renderer that
wants every cell.

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
