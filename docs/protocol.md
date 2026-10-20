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
frame), or WebTransport for remote ones, all under TLS. Manual
`phux stdio-bridge` carries frames over SSH stdio; this is not the normal
[`attach --ssh` bootstrap path](./remote.md#other-ways-in). The frame format
is the same on every transport.

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
additive optional fields can be skipped by an older peer. Most nested
records are positional and big-endian, with string leaves encoded as
`u32 length || bytes`; top-level UTF-8 field values are raw bytes.
Organization bodies/records and selected extensible event bodies instead
use nested tagged fields: see the [organization codec](../crates/phux-protocol/src/wire/frame/org.rs).
Do not infer a nested grammar from a type name in the schema. Field ids
are append-only; a retired id is never reused, and the schema lists them.

`FRAME_COMPRESSED` (`0x9A`) wraps one deflated frame. A server uses it only
if the client's `HELLO` offered compression, and only for bodies of 4 KiB
or more.

## Handshake

1. The client sends `HELLO` (`0x01`): its name, protocol
   `major.minor.patch`, and `ClientCapabilities` (color support, image and
   keyboard protocols, which bootstrap profiles it can consume, compression).
2. The server checks `major.minor` against its own. On a mismatch it sends
   `ERROR` with `VERSION_INCOMPATIBLE` and closes.
3. The server authorizes the connection ([owner, delegated, or scoped
   remote authority](./remote.md#connection-authority)) and picks a bootstrap
   profile the client offered.
4. The server replies `HELLO_OK` (`0x80`): its version, `ServerCapabilities`
   (two words of feature bits, `features` and `features_ext`), a server
   instance id, the selected profile, and size limits.

Before `HELLO_OK`, only `HELLO` and `PING` are accepted. The Rust clients
also refuse a `patch` mismatch; the server does not.

A client sends `PING` (`0x7F`); the server replies with `PONG` (`0xFF`)
carrying the same nonce. `PONG` is server-to-client only; the classifier
denies a client-originated `PONG`.

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

A fresh successful `SPAWN_RESOURCE` subscribes its connection to the new
Terminal, even without a session `ATTACH`: output and terminal-scoped
fanout reach it, and `INPUT_*` passes the subscription gate. This does not
bypass input leases, authorization, or acknowledged-input checks.

A client that only wants text does not attach: it sends `GET_SCREEN`
through `COMMAND` and gets the server's rendering.

### Generations, publication, and history

An attach cuts one terminal actor's state at an inclusive `base_seq`.
A generation has nonzero application `stream_id` and `bootstrap_id`;
bootstrap chunks are contiguous, and live output begins at `base_seq + 1`.
Native checkpoint bytes belong to the engine: transport and UI layers must
not rewrite them. Decode into invisible staging. Publish atomically only
when both the matching engine decode and protocol `BOOTSTRAP_READY` are
ready; `ATTACH_READY` is not a substitute for either.

A tombstone retires that generation even if late chunks/output arrive.
Keep the last published view display-only until the replacement is ready.
On gaps, overflow, resize or codec discontinuity, fence and recut; never
silently skip bytes or replay them into an obsolete replica. Consumer
mailboxes, stale-output recovery and resync attempts are bounded; failure
ends in an explicit error rather than unbounded backlog or a frozen view.
Only StateSync live output gets a cumulative, generation-bound `FRAME_ACK`,
after application to the replica; raw output and bootstrap never do.
Normal reliable StateSync advances its server reference on emit: an ACK is
not proof of a paint or application consumption, nor a lossy-delivery promise.

Native history is a bounded, pull-based lease on retained engine history,
not durable scrollback and not part of the attach's eager work. Follow the
engine cursor and authenticate each page before requesting its successor.
The final page must authenticate the engine's `FINISH`; no row-count guess
or protocol cursor alone substitutes for it, and trailing bytes after
`FINISH` are rejected.
A rejected page/request and an invalidated cursor are distinct outcomes;
a history tombstone invalidates that history, not the live replica or input.
Pruning or history decode failure must not retire a healthy live generation.
The [kernel](../crates/phux-client-core/src/session.rs), its
[generation/ACK/history tests](../crates/phux-client-core/src/session/tests.rs)
and the server [pump](../crates/phux-server/src/runtime/pump.rs) own these transitions.

### QUIC stream routing

With negotiated `QUIC_STREAMS`, keep connection control on the control
stream and terminal traffic on client-opened bidirectional terminal streams.
Each begins with `STREAM_BIND`; its application stream id is not the QUIC
stream number. Binding requires authorization for that terminal and frame
provenance must match the binding. A control-stream viewport update must
cross the ordering barrier before dependent terminal-stream work; ordering
within two separate QUIC streams does not provide that barrier for you.
See [server stream admission](../crates/phux-server/src/transport/quic.rs)
and the [runtime stream router](../crates/phux-client-runtime/src/session/multistream.rs).
The [viewport barrier](../crates/phux-client-runtime/src/session.rs) uses a
control `PING`/matching `PONG` while retaining preceding control frames.
Without the capability, use the single-stream path.

## Input

Input is structured. `INPUT_KEY` (`0x10`) carries a key event in
libghostty's vocabulary: action, physical key, modifiers, text,
unshifted codepoint. `INPUT_MOUSE`, `INPUT_FOCUS`, `INPUT_PASTE`, and
`VIEWPORT_RESIZE` (`0x20`) likewise. The server encodes them to bytes for
the PTY according to the terminal's current modes (application cursor
keys, kitty keyboard protocol, bracketed paste...), so clients never
generate escape sequences.

### Terminal queries

The canonical server terminal answers each child query once. Replica
emulator replies are not another source of PTY input. `INPUT_TERMINAL_REPLY`
remains for compatibility but the server [discards it](../crates/phux-server/src/runtime/commands.rs).
New clients suppress replica replies; never resend them as paste or raw input.
The [engine regression tests](../crates/phux-client-core/src/engine/ghostty.rs)
exercise this single-reply boundary.

### Receipts, retries, and backpressure

With `ACKNOWLEDGED_INPUT`, one logical batch uses `APPLY_INPUT`. Retry an
unresolved request with the same random operation id and unchanged semantic
payload, not its connection-local `request_id`. This is safe only against
the same `HELLO_OK` server incarnation within the dedupe retention horizon
(currently ten minutes from admission). Keyed spawn/session creates and
supervisory operations have the same bounded incarnation rule. Saturation
refuses new ids rather than evicting live receipts. After a changed
incarnation or expired horizon, read current state and reconcile; never
blindly replay, even with the old key. A cached create result may name a
resource that has since closed.

Input OK means the PTY writer completed `write_all`/flush, not child
consumption, shell completion, or model admission. A provably not-written
refusal differs from `INPUT_DELIVERY_UNKNOWN`, which may already have
written bytes: report/reconcile unknown, never resubmit under a fresh id.
Looking up the retained outcome under the original id is not permission to
execute it again. Legacy `INPUT_*`/`ROUTE_INPUT` are ordered and bounded by
connected backpressure, not reconnect receipts. Do not fragment one logical
command into fire-and-forget paste events. See the shared
[dedupe store](../crates/phux-server/src/runtime/operation_dedupe.rs),
[acknowledged lane](../crates/phux-server/src/runtime/input_lane/acknowledged.rs)
and [incarnation recovery tests](../crates/phux-client-runtime/tests/it/control_plane/recovery.rs).

Events are bounded in-memory wakeups over level state. On a journal gap,
reread authoritative state rather than inferring the missing transitions.
For delayed cleanup, `KILL_RESOURCE_IF` binds the allocation instance token
(not `HELLO_OK.server_id`) and attachment/use conditions atomically. A
refusal leaves the resource alone; never fall back to unconditional kill.

## Commands, events, resources

Everything that is not attach, input, or output is a `COMMAND` (`0x31`)
with a sub-tag and a `COMMAND_RESULT`, or an `EVENT` (`0xB3`) the client
subscribed to. Commands include listing sessions, spawning and killing
panes, `GET_SCREEN`, tags, organization moves, agent session streams,
approvals. Events include pane lifecycle, agent state changes, asks,
bells, and command boundaries from shell integration.

`PERF_CONTROL` (command tag `0x39`) is gated on
`features_ext.PERF_CONTROL` (`0x80`). Its positional body is one op byte:
trace start (`0`), trace stop (`1`), heap accounting on (`2`), or off (`3`).
Undefined ops decode and receive `INVALID_COMMAND`. The ordinary owner
socket alone can use it: traces write files on the server host and heap
accounting affects every allocation. Delegated and remote grants cannot
enable it. Older servers do not advertise the bit; clients check before
sending. Reports remain schema 1, with optional `rss_bytes` and `alloc`
JSON fields that old readers ignore.

Panes are **resources** with a **kind**: `Terminal` or `AgentSession` so
far, each with its own methods and events. A client tolerates kinds it
does not know. The kind catalog in `phux-protocol::kinds` lists every
method with the authorization verb it needs; the generated
[kinds reference](./reference/kinds.md) renders it.

`GET_METADATA` / `SET_METADATA` / `SUBSCRIBE_METADATA` (`0x50`-`0x54`)
attach small keyed values to resources. Some keys are plain storage; some
are read by the server (`phux.agent/v1` is the agent record).
`ORGANIZATION` gates typed server-owned organization commands;
`phux.tui.layout/v1` is a legacy layout adapter, not a competing source of
membership or arrangement authority. See [organization](./organization.md).

An `AgentSession` has an immutable, one-level Terminal parent. Purging the
parent closes its children atomically; closing a child never kills its
parent. Process exit under Retain is not purge: see
[activations and exit policy](./organization.md#activations-and-reconciliation).

### File transfer

`PUT_FILE` uses a live Terminal to select the destination host, not to grant
arbitrary filesystem writes. The server chooses the sandbox root and name;
offsets and overlapping bytes continue the same principal's immutable
upload. Only whole-file digest verification, sync and rename publish a final
path. Digest mismatch discards the partial file and sidecar: restart rather
than trying to repair retained corrupt bytes. Completed artifacts belong to
the user and are not silently TTL-deleted; quotas refuse with a remedy.
Keep payloads and completed paths out of routine telemetry. Transfer files
through this lane, not fragmented PTY input. The [upload implementation and
tests](../crates/phux-server/src/runtime/upload.rs) own continuation and
quota behavior; [CLI](./reference/cli.md#phux-upload) and
[environment](./reference/environment.md) own syntax and limits.

`GET_FILE` is the pull, behind `features_ext.FILE_DOWNLOAD`. It addresses a
machine, not a pane: an optional `host` names a satellite of the serving hub,
and no `host` reads the server itself, the way `PATH_QUERY` browses. The
client names an absolute path and the server reads any regular file its own
user can read, in chunks of at most 8 MiB. That is deliberately no narrower
than the server grants already: a consumer holding `INPUT` on this server can
type `cat` into any of its shells, so the command is classified `INPUT` on
the server. A hub relays a named host only when that satellite advertises
`FILE_DOWNLOAD`; otherwise it refuses `UNSUPPORTED_SATELLITE_ROUTE` and
forwards nothing, so an older satellite never sees an unknown tag. Each
chunk is one stateless request that returns the file's current size, the
bytes, and, on the chunk that reaches the end, a whole-file SHA-256; a
client resumes from its own offset and must not expose a file whose digest
differs. There is no size cap and no sandbox on the reading side; the
destination disk is the client's. The disk read runs in the bulk command
lane beside `PUT_FILE`, off the connection dispatcher. The
[download implementation](../crates/phux-server/src/runtime/download.rs) and
the [client helper](../crates/phux-client/src/download.rs) own the read and
the exclusive, verify-then-publish write;
[`phux download`](./reference/cli.md#phux-download) owns syntax.

### Checkout methods

`CHECKOUT` (command tag `0x3c`, behind `features_ext.CHECKOUT` = `0x400`)
reads and stages the Git checkout a Terminal is in: status with per-file
line counts, one file's diff, a page of history, branches, a directory
listing, the start of a file, the branch's pull request and checks (through
the host's `gh`), and stage or unstage. It names a Terminal and one typed
request; the server that owns the Terminal resolves the checkout from the
Terminal's live working directory, so a client never names a host path.
Paths in requests and answers are relative to the checkout's top level; an
absolute path, `..`, anything under `.git`, or a path crossing a symbolic
link is refused. The command is classified `INPUT` on the named Terminal,
since an `INPUT` holder can already run `git diff` or `git add` there.

The request and its answer cross the wire as bounded JSON inside the command
envelope (requests at most 64 KiB, answers at most 4 MiB, text at most
1 MiB and cut flagged `truncated`). Answers skip unknown fields and fold
unknown enum values to `unknown`, so a newer server can add detail. A
refusal is a typed answer (`not_a_repository`, `invalid_path`, `timeout`,
`gh_unavailable`, `no_pull_request`, ...), not a command error; a missing
Terminal or a denied grant stays a command error. Git and `gh` run off the
dispatcher under a deadline and an output cap, with
`GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0` and `GIT_LITERAL_PATHSPECS=1`.
A hub relays a satellite Terminal's checkout only to a satellite that
advertises `CHECKOUT`; otherwise it refuses `UNSUPPORTED_SATELLITE_ROUTE`.
The [request and answer types](../crates/phux-protocol/src/checkout.rs), the
[server handler](../crates/phux-server/src/runtime/checkout.rs), and the
shared [`phux-git`](../crates/phux-git/src/checkout.rs) module own the
details.

## Minimal client

1. Framing and tagged fields, including skipping unknown ids.
2. `HELLO` offering the synthesized raw profile and no compression; check
   `HELLO_OK`.
3. `ATTACH`; handle `ATTACHED`, the bootstrap frames, `ATTACH_READY`,
   `BOOTSTRAP_TOMBSTONE` and replacement, `RESOURCE_OUTPUT` with exact
   generation/sequence checks, `RESOURCE_CLOSED`, `ERROR`. Publish only at
   the readiness boundary above; raw/bootstrap never send `FRAME_ACK`.
4. Send `PING` for liveness and handle `PONG`. Send `INPUT_KEY` and
   `VIEWPORT_RESIZE`. `DETACH` to leave.
5. Feed terminal bytes to an emulator. Tolerate unknown kinds, events, and
   close reasons.

## Changing the wire

- Allocate a new type byte or sub-tag in `wire/frame/mod.rs`; duplicates
  fail to compile.
- Add field ids in `wire/field.rs`, append-only.
- Gate new behavior on a feature bit in `caps.rs`. Do not bump the minor.
  A shipped capability bit is never withdrawn or repointed.
- Classify any new command in `kinds.rs`, or dispatch denies it.
- Regenerate the schema (`PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p
  phux-protocol wire_schema_matches`), fill in any `?` types, and
  `just docs-gen` for the kinds page.
- Add roundtrip and snapshot tests next to the existing ones in
  `crates/phux-protocol/tests/it/`.
- Say in the PR what an older client sees.
