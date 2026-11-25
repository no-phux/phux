---
group: build
order: 3
---

# Server protocol

The rules a client must follow to speak to a phux server.
[`crates/phux-protocol`](../crates/phux-protocol) is the definition. Every
frame, tag, and field is in the generated
[`wire-schema.json`](./spec/wire-schema.json); every command, verb, and
feature bit is in the generated [kinds reference](./reference/kinds.md).
Rust clients use `phux-client-core` and `phux-client-runtime`;
[`clients/phux-web`](../clients/phux-web) is a non-Rust one.

## Transports

A Unix socket locally; QUIC, WebSocket (one binary message per frame), or
WebTransport under TLS remotely; `phux stdio-bridge` over SSH stdio.

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

- Match fields by id in any order; skip unknown ids.
- Nested records are positional and big-endian, with `u32 length || bytes`
  strings, except organization records and some event bodies, which nest
  tagged fields ([organization codec](../crates/phux-protocol/src/wire/frame/org.rs)).
  Top-level UTF-8 values are raw bytes.
- A record that is not last in its field carries a `u32` length; readers
  skip bytes past the fields they know.
- Field ids are append-only and never reused.

`FRAME_COMPRESSED` wraps one deflated frame, only when the client's `HELLO`
offered compression, only for bodies of 4 KiB or more, and never for a frame
that settles a request.

## Handshake

1. The client sends `HELLO`: name, protocol `major.minor.patch`, and
   capabilities (Terminal bootstrap profiles, [resource kinds](./compatibility.md#a-newer-server-an-older-client),
   compression, display features).
2. Another major gets `ERROR` `VERSION_INCOMPATIBLE` and a close. Any minor
   or patch is admitted.
3. The server authorizes the connection ([remote authority](./remote.md)) and
   picks one offered Terminal profile. If it can serve none of those offered,
   it sends `ERROR` `CODEC_UNAVAILABLE` and closes.
4. `HELLO_OK` carries the server version, feature bits (`features`,
   `features_ext`, `features_ext2`), instance id, selected profile, and size limits.
   Trailing capability words are omitted when empty and absent words mean no
   support. `features_ext2` bit `0x1` is `ARCHIVED_LAUNCH_ADMISSION`: current-host
   archived execution enforcement, not raw execution approval.

Without a Terminal profile, binding a Terminal stream is refused
`CODEC_UNAVAILABLE` and the connection stays. Before `HELLO_OK` only `HELLO`
and `PING` are accepted. `PONG` echoes the `PING` nonce and is
server-to-client only.

Check a feature bit before sending what it gates, and degrade that one
feature without it. [Compatibility](./compatibility.md) has the 1.x rules.

## Attach and output

`ATTACH` (session or pane, plus viewport) gets `ATTACHED`, a bootstrap per
Terminal and document (`BOOTSTRAP_BEGIN`, `BOOTSTRAP_CHUNK`...,
`BOOTSTRAP_READY`), then `ATTACH_READY`. `RESOURCE_OUTPUT` carries payloads
with a per-stream sequence; `RESOURCE_CLOSED` ends a stream; `DETACH` gets
`DETACHED`. To read text without attaching, send `GET_SCREEN`.

Terminal bootstrap profiles:

- **Native state**: a libghostty checkpoint; the only profile with history
  paging.
- **Synthesized VT, raw**: a repaint, then raw PTY bytes. Any emulator.
- **Synthesized VT, state sync**: deltas against a per-client reference,
  acknowledged with `FRAME_ACK`.

A successful `SPAWN_RESOURCE` subscribes its connection to the new Terminal
without `ATTACH`. Input leases and authorization still apply.

### Generations, publication, and history

- An attach cuts a terminal's state at an inclusive `base_seq`; live output
  starts at `base_seq + 1`. A generation has nonzero `stream_id` and
  `bootstrap_id`, and its chunks are contiguous.
- Decode into invisible staging; never rewrite native checkpoint bytes. Publish atomically only after both the
  engine decode and `BOOTSTRAP_READY`; `ATTACH_READY` replaces neither.
- A tombstone retires its generation even if late frames arrive. Keep the
  last view display-only until the replacement is ready.
- On a gap, overflow, resize, or codec discontinuity, fence and recut. Never
  skip bytes or replay into an obsolete replica. Every recovery path is
  bounded and ends in an explicit error.
- A lagging `AgentSession` consumer is replayed from the session ring; if
  records were evicted, the generation is tombstoned `OUTBOUND_GAP` and
  rebootstrapped.
- Only StateSync live output is acknowledged: `FRAME_ACK` is cumulative,
  generation-bound, and sent after applying to the replica. It does not prove
  a paint.
- Native history is a bounded pull lease on immutable snapshots, not durable
  scrollback. Authenticate each page before requesting the next; the final
  page must authenticate `FINISH` with no trailing bytes. A history tombstone
  or decode failure never retires a healthy live generation.

### QUIC stream routing

With `QUIC_STREAMS`, control stays on the control stream and terminal traffic
goes on client-opened bidirectional streams, each starting with
`STREAM_BIND` (its stream id is not the QUIC stream number). Binding needs
authorization for that terminal; frame provenance must match the binding.
Separate QUIC streams are unordered, so a viewport update must cross a
control `PING`/`PONG` [barrier](../crates/phux-client-runtime/src/session.rs)
before dependent terminal-stream work.

## Input

A Terminal takes `INPUT_KEY` (libghostty key vocabulary), `INPUT_MOUSE`,
`INPUT_FOCUS`, `INPUT_PASTE`, and `VIEWPORT_RESIZE`. The server encodes them
for the PTY from the terminal's modes; clients never generate escape
sequences. `VIEWPORT_COLORS` sends the outer terminal's colors after a theme switch.
A document takes `SEND_DOCUMENT_INPUT` ([below](#commands-events-resources)).

### Terminal queries

The server's terminal answers each child query once. Suppress replica
emulator replies; the server discards `INPUT_TERMINAL_REPLY`.

### Receipts, retries, and backpressure

With `ACKNOWLEDGED_INPUT`, one logical batch is one `APPLY_INPUT`.

- Retry with the same operation id and payload, never a new id. That is safe
  only against the same `HELLO_OK` server incarnation within the ten-minute
  dedupe window; keyed creates and supervisory operations follow the same
  rule. Otherwise read state and reconcile.
- Saturation refuses new ids; it never evicts live receipts.
- OK means the PTY writer flushed, not that the child consumed it.
- `INPUT_DELIVERY_UNKNOWN` may have written bytes: reconcile, never resubmit.
- Legacy `INPUT_*` and `ROUTE_INPUT` are ordered by connection backpressure,
  with no reconnect receipts. Never split one logical command into pastes.

Events are bounded wakeups over level state; after a journal gap, reread
state. For delayed cleanup use `KILL_RESOURCE_IF`, bound to the allocation
instance token (not `HELLO_OK.server_id`); on refusal, never fall back to an
unconditional kill.

## Commands, events, resources

Everything else is a `COMMAND` answered by `COMMAND_RESULT`, or a subscribed
`EVENT`. Every resource has a kind; tolerate kinds you do not know.

- `AccessibilityTree` and `PixelSurface` are whole documents (at most
  4 MiB) the producer replaces with `PUBLISH_RESOURCE_STATE`.
  `ATTACH_RESOURCE` bootstraps the newest at `0 x 0`; each later one is one
  `RESOURCE_OUTPUT`. A lapped viewer is tombstoned `OUTBOUND_GAP` and recut
  from the newest. These streams take no input, ACK, or history request.
- A `Document` names its MIME type in `SPAWN_RESOURCE`'s `provider`
  ([resource kinds](./resource-kinds.md)).
- `SEND_DOCUMENT_INPUT` is relayed unread and unjournaled to the connection
  that spawned the document only. It answers `OK` once queued, `NOT_PRODUCER`
  if that connection is gone, `OVERFLOW` if its mailbox is full.
- An `AgentSession` or document has one immutable parent Terminal. Purging
  the parent closes its children atomically; closing a child never kills the
  parent ([organization](./organization.md)).
- Organization commands own membership and arrangement; the
  `phux.tui.layout/v1` metadata key is a
  [legacy adapter](./compatibility.md#legacy-tui-layouts).
- A `RESOURCE_CREATE` placement needs `CREATE` and `ORGANIZE` on the
  destination session and admits no cross-session anchor.
- `GET_CREATION` returns a retained creation receipt or `NOT_FOUND`; it
  needs `INVENTORY` on the original session, and a missing receipt does not
  prove the mutation never ran. `DESCRIBE_API` returns the public workflow
  schemas ([scripting](./scripting.md)).
- `PERF_CONTROL` (traces, heap accounting) works on the owner socket only:
  traces write files on the host and accounting slows every allocation.
  Delegated and remote grants cannot enable it.

[Host operations](./protocol-files.md) covers files, checkout, host services,
and push.

## Changing the wire

- Allocate type bytes in `wire/frame/catalog.rs` and sub-tags in the
  `*tags.rs` files beside it; duplicates fail to compile.
- Append field ids to `tlv_body!` in `wire/field/`.
- Gate new behavior with an extension capability bit; `features` and all
  published bit positions are frozen. `ServerFeatureExt` owns the first
  extension word and `ServerFeatureExt2` the next. An absent word means
  unsupported, not permission to try the operation. Restore/relaunch admission
  checks the executing peer's current word, including at federation hops.
  Follow [Making a change compatible](./compatibility.md#making-a-change-compatible).
- Classify every new command in `kinds.rs`, or dispatch denies it.
- Regenerate, then fill in any `?` types in the schema:

  ```sh
  PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p phux-protocol wire_schema_matches
  just docs-gen
  ```

- Add roundtrip and snapshot tests in `crates/phux-protocol/tests/it/`.
