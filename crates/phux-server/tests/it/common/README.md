# `tests/common/` — shared scaffolding

This directory is Cargo's special-cased `tests/common/` slot: files here
are *not* compiled as standalone integration binaries. Helpers live here
so each `tests/*.rs` binary can `mod common;` and pull in only what it
uses.

## `mod.rs`

Wire-level scaffolding for the `phux-byc.6.*` integration tests:
`spawn_server`, `attach_by_name`, `recv_typed`, `send_frame`, etc. See
the module-level doc-comment.

## `screen.rs` — VT oracle for client-render assertions

`Screen` parses the VT bytes the server emits (over `TERMINAL_OUTPUT` or
any other stream you care to feed it) into an inspectable grid using a
fresh `libghostty_vt::Terminal`. Use it whenever a test wants to
assert "after I sent these keystrokes, the rendered terminal contains
`X`" — rather than counting bytes or stripping SGR by hand.

```rust
use crate::common::screen::Screen;

let mut screen = Screen::new(80, 24).unwrap();
screen.write(&pane_output_bytes); // any VT byte stream
assert!(screen.row(0).contains("hi"));
assert_eq!(screen.cursor(), (2, 0));
```

API surface: `new`, `write`, `row`, `rows`, `cursor`, `contains`,
`snapshot_text`. Mutable receivers because each call walks the live
grid through libghostty's render iterators.

### libghostty pitfall (`phux-l0t`)

`Snapshot::dirty()` on the version of `libghostty-vt` pinned in
`Cargo.toml` returns `Error::InvalidValue` on every other update in
the "drop snapshot + re-update" pattern this harness uses. `Screen`
deliberately ignores `dirty()` and walks the grid every call; the
production client's renderer applies the same workaround
(`crates/phux-client/src/attach/render.rs`).

`Snapshot::cursor_viewport()` rides on the same FFI surface — `Screen`
degrades it to `(0, 0)` on error rather than panicking inside an
assertion helper. Tests that want a precise cursor must be tolerant.

### Demo

`tests/screen_harness_demo.rs` exercises the full pipeline: spin up a
server with `cat` on a PTY, attach over the wire, send `INPUT_KEY`,
feed each `TERMINAL_OUTPUT` chunk into `Screen`, assert `screen.contains("a")`.
That test is the canonical reference for "how do I use this in a real
end-to-end check".
