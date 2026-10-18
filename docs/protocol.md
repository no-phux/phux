# Protocol

How a client talks to a phux server. The code in
[`crates/phux-protocol`](../crates/phux-protocol) is the definition; this
page is the map. [`wire-schema.json`](./spec/wire-schema.json) lists every
frame and field and is generated from the codec, so it cannot be stale.

If you are writing a client in Rust, use `phux-client-core` and
`phux-client-runtime` rather than this. If you are writing one in another
language, [`clients/phux-web`](../clients/phux-web) is a complete
non-native implementation to read alongside.

## Transports

A Unix socket for local clients. QUIC (one bidirectional stream per
connection, more with `QUIC_STREAMS`), WebSocket (one binary message per
frame), or WebTransport for remote ones, all under TLS. SSH carries frames
over stdio through `phux stdio-bridge`. The frame format is the same on
every transport.

## Framing

```
length: u32 big-endian   (1 ..= 16 MiB)
type:   u8
body:   length - 1 bytes
```

A body is a sequence of tagged fields:

```
field_id:  varint
wire_type: u8      (always 4: length-delimited bytes)
length:    varint
value:     bytes
```

Decoders match fields by id in any order and skip ids they do not know, so
adding a field never breaks an older peer. Inside a value, records are
positional and big-endian, with strings as `u32 length || bytes`. Field
ids are append-only; a retired id is never reused, and the schema lists
them.

`FRAME_COMPRESSED` (`0x9A`) wraps one deflated frame. A server uses it only
if the client's `HELLO` offered compression, and only for bodies of 4 KiB
or more.

## Handshake

1. The client sends `HELLO` (`0x01`): its name, protocol
   `major.minor.patch`, and `ClientCapabilities` (color support, image and
   keyboard protocols, which bootstrap profiles it can consume, compression).
2. The server checks `major.minor` against its own. On a mismatch it sends
   `ERROR` with `VERSION_INCOMPATIBLE` and closes.
3. The server authorizes the connection (owner on the Unix socket; a scoped
   grant from the client certificate otherwise) and picks a bootstrap
   profile the client offered.
4. The server replies `HELLO_OK` (`0x80`): its version, `ServerCapabilities`
   (two words of feature bits, `features` and `features_ext`), a server
   instance id, the selected profile, and size limits.

Before `HELLO_OK`, only `HELLO` and `PING` are accepted. The Rust clients
also refuse a `patch` mismatch; the server does not.

`PING` (`0x7F`) and `PONG` (`0xFF`) go both ways and keep the connection
honest; answer them.

## Capabilities, not versions

The protocol version is `0.9` and has been for a long run of wire changes.
Clients and servers must agree on `major.minor`; everything added since is
either a feature bit the server advertises in `HELLO_OK` (`RESOURCE_KINDS`,
`ACKNOWLEDGED_INPUT`, `APPROVALS`, ...) or an optional field an old peer
skips. A client checks the bit before relying on the behavior. A version
bump is reserved for a change no additive shape can express, and it breaks
every deployment at once, so it is rare on purpose.

The feature names, their bits, and which commands each gates are in the
generated [kinds reference](./reference/kinds.md). `phux status --json`
prints the running server's.

## Attach and output

`ATTACH` (`0x02`) names a session or pane and the client's viewport. The
server answers `ATTACHED` (`0x81`), then for each pane streams a bootstrap
(`BOOTSTRAP_BEGIN`, `BOOTSTRAP_CHUNK`..., `BOOTSTRAP_READY`) that brings the
client's replica up to the current screen, then `ATTACH_READY` (`0x83`).
From there, `RESOURCE_OUTPUT` (`0x90`) frames carry each pane's bytes with
a per-stream sequence number. For a terminal, the bytes are the PTY's VT
output, and the client feeds them to its own emulator. `RESOURCE_CLOSED`
ends a stream. `DETACH` (`0x03`) gets `DETACHED`.

Which bootstrap you get depends on the profile the server selected:

- **Native state**: a libghostty checkpoint, for clients that run
  libghostty themselves. Exact, and the only profile that supports history
  paging (`HISTORY_REQUEST` / `HISTORY_PAGE`).
- **Synthesized VT, raw**: a VT sequence that repaints the screen, then the
  raw PTY bytes. Works with any emulator.
- **Synthesized VT, state sync**: the server keeps a per-client reference
  screen and sends deltas against it, acknowledged with `FRAME_ACK`.

A client that only wants text does not attach: it sends `GET_SCREEN`
through `COMMAND` and gets the server's rendering.

## Input

Input is structured. `INPUT_KEY` (`0x10`) carries a key event in
libghostty's vocabulary: action, physical key, modifiers, text,
unshifted codepoint. `INPUT_MOUSE`, `INPUT_FOCUS`, `INPUT_PASTE`, and
`VIEWPORT_RESIZE` (`0x20`) likewise. The server encodes them to bytes for
the PTY according to the terminal's current modes (application cursor
keys, kitty keyboard protocol, bracketed paste...), so clients never
generate escape sequences. `INPUT_TERMINAL_REPLY` is the one opaque
exception, for a reply the client's emulator must make itself.

With `ACKNOWLEDGED_INPUT`, batches go through `APPLY_INPUT` and are
acknowledged, which is what lets a client replay input across a reconnect
without duplicating it.

## Commands, events, resources

Everything that is not attach, input, or output is a `COMMAND` (`0x31`)
with a sub-tag and a `COMMAND_RESULT`, or an `EVENT` (`0xB3`) the client
subscribed to. Commands include listing sessions, spawning and killing
panes, `GET_SCREEN`, tags, organization moves, agent session streams,
approvals. Events include pane lifecycle, agent state changes, asks,
bells, and command boundaries from shell integration.

Panes are **resources** with a **kind**: `Terminal` or `AgentSession` so
far, each with its own methods and events. A client tolerates kinds it
does not know. The kind catalog in `phux-protocol::kinds` lists every
method with the authorization verb it needs; the generated
[kinds reference](./reference/kinds.md) renders it.

`GET_METADATA` / `SET_METADATA` / `SUBSCRIBE_METADATA` (`0x50`-`0x54`)
attach small keyed values to resources. Some keys are plain storage; some
are read by the server (`phux.agent/v1` is the agent record; the TUI's
layout rides under `phux.tui.layout/v1`).

## Minimal client

1. Framing and tagged fields, including skipping unknown ids.
2. `HELLO` offering the synthesized raw profile and no compression; check
   `HELLO_OK`.
3. `ATTACH`; handle `ATTACHED`, the bootstrap frames, `ATTACH_READY`,
   `RESOURCE_OUTPUT` in sequence order, `RESOURCE_CLOSED`, `ERROR`.
4. Answer `PING`. Send `INPUT_KEY` and `VIEWPORT_RESIZE`. `DETACH` to leave.
5. Feed terminal bytes to an emulator. Tolerate unknown kinds, events, and
   close reasons.

## Changing the wire

- Allocate a new type byte or sub-tag in `wire/frame/mod.rs`; duplicates
  fail to compile.
- Add field ids in `wire/field.rs`, append-only.
- Gate new behavior on a feature bit in `caps.rs`. Do not bump the minor.
- Classify any new command in `kinds.rs`, or dispatch denies it.
- Regenerate the schema (`PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p
  phux-protocol wire_schema_matches`), fill in any `?` types, and
  `just docs-gen` for the kinds page.
- Add roundtrip and snapshot tests next to the existing ones in
  `crates/phux-protocol/tests/it/`.
- Say in the PR what an older client sees.
