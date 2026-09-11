---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-06
---

# Shipping-engine wire fixtures

**TL;DR.** These binaries are deterministic server frames produced by the Rust
protocol codec. Feed `hello.bin` after starting the host, then `attached.bin`
after requesting attach ID 1 to session ID 1.

Regenerate from the repository root (native prerequisites in
[setup](../../../../../docs/SETUP.md#native-setup)):

```sh
cargo run --locked -p phux-client-ffi --example cockpit_fixture \
  --profile ffi-dev --target-dir target/cockpit-fixtures
```

The generator accepts an optional output directory after `--`. It validates the
encoded frames against the public C ABI before writing either file: HELLO and
ATTACH queues, negotiated/attached lifecycle, native-engine grid geometry and
marker cells, and decoded outgoing key, paste, focus, and terminal-resize frames.
Use a private target directory per worktree for concurrent builds.

## Fixture contract

- `hello.bin`: one `HELLO_OK` at the current protocol version, server ID
  `cockpit-fixture`, `SynthesizedVtRaw`, 1024-byte bootstrap/history limits, and
  the `TerminalReply` server feature.
- `attached.bin`: five complete concatenated frames in server order:
  `ATTACHED`, `BOOTSTRAP_BEGIN`, `BOOTSTRAP_CHUNK`, `BOOTSTRAP_READY`, `ATTACH_READY`.
  Attach, session, window, and initial client IDs are 1. The session and window
  are named `fixture`. The focused terminal is local terminal 7, 80 columns by
  24 rows, stream 7, bootstrap 1, base sequence 0, chunk sequence 0. There is no
  history cursor.
- `session_renamed.bin`: one `METADATA_CHANGED` of `phux.session.name/v1`
  (Global) with the applied value `fixture\0renamed`: the server's broadcast of
  a rename of the attached session. The generator checks that an attached
  client reads it into its session list.
- Keep-empty sessions (ADR-0105): `hello_keep_empty.bin` is `hello.bin` that
  also advertises `KeepEmptySessions`. `standby_keep_empty_state.bin` answers
  a listing client's first query (ID 1) with `build` (1, one window) and the
  keep-empty `scratch` (3, no windows). `attached_empty.bin` is `ATTACHED`
  (attach ID 1, session 3, the server's sentinel focus IDs, no windows or
  resources) then `ATTACH_READY`. `workspace_empty.bin` is the automatic
  workspace read that follows, correlated as a fresh client's first internal
  requests: absent layout metadata, then the same registry.
- The VT chunk clears/homes the screen, writes `COCKPIT FIXTURE` at row 0,
  column 0, enables bracketed paste (`CSI ? 2004 h`), focus reporting
  (`CSI ? 1004 h`), and Kitty keyboard disambiguation (`CSI > 1 u`).

Pass concatenated bytes through the host's incoming staging/drain path. Direct
`phux_client_feed_frame` callers must split them into individual frames; the
generator demonstrates this using `FrameKind::decode` and its unconsumed tail.
The outgoing validator also uses the Rust decoder, so it checks terminal IDs
and input payloads rather than treating a matching frame tag as sufficient.

The generator is
[`crates/phux-client-ffi/examples/cockpit_fixture.rs`](../../../../../crates/phux-client-ffi/examples/cockpit_fixture.rs).
