---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-08-02
---

# proto — connection lifecycle, framing, and protocol meta

**TL;DR.** The protocol-meta tier. Every consumer that completes a
HELLO speaks this surface: transport assumptions, length-prefixed
framing, version and capability negotiation, lifecycle frames
(DETACH / SUBSCRIBE / PING), per-Terminal flow control, structured
errors, transport security plus optional workload proof, and the per-tier
conformance contract.

---

## Conventions

Throughout the spec:

- Multi-byte integers are **big-endian** on the wire.
- `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64` denote fixed-width
  integers.
- `varint` is unsigned [LEB128]: 7 data bits per byte, MSB set on
  continuation. Encoders MUST emit the minimum-length encoding. Decoders
  MUST reject non-canonical encodings (length-extended representations).
  Varints carry the field-tagged TLV envelope — `field_id`, `wire_type`
  lengths — per [appendix-encoding.md](./appendix-encoding.md) §1.
- `bytes` is `u32 length || raw bytes`, the length a big-endian count of
  the raw bytes that follow. This is a **leaf primitive**: it length-
  prefixes a `str` / `bytes` value sitting inside a field's positional
  value, distinct from the varint length the TLV envelope uses for a
  `BYTES` wire-type field (appendix-encoding.md §1).
- `str` is `bytes` whose contents are valid UTF-8.
- `bool` is `u8` with `0` for false, `1` for true, all other values
  reserved.
- `optional<T>` is `bool present || T value` (where `T` is only present
  if `bool` is `1`).
- Field IDs and message IDs are stable: once assigned, they never change
  meaning.

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in [RFC 2119].

[LEB128]: https://en.wikipedia.org/wiki/LEB128
[RFC 2119]: https://datatracker.ietf.org/doc/html/rfc2119

---

## 1. Introduction

phux is a terminal multiplexer. A long-lived server owns **Terminals**:
each Terminal backs one PTY and one libghostty grid. Clients attach to
the server over a reliable byte stream and present Terminals to users —
as a TUI inside another terminal, as a native GUI, as an agent harness,
or as something else entirely. The Terminal is the wire's primary
primitive; everything else is an optional layered service on top of it.

The protocol described here is the contract between server and client.
The wire is **asymmetric**:

- **Server → Client (Terminal content):** VT bytes. The server
  forwards the byte stream produced by each Terminal's PTY (after
  canonical parsing into the server's `libghostty_vt::Terminal` for
  state ownership, and after per-client capability downsampling — see
  §6 Version negotiation and [L1.md](./L1.md)).
- **Client → Server (input events):** structured `KeyEvent`,
  `MouseEvent`, `FocusEvent`, paste, and viewport messages — never raw
  VT bytes ([input.md](./input.md)).

A `libghostty_vt::Terminal` runs on **both** ends. The server's
Terminal is the canonical state (authoritative grid, scrollback,
cursor, modes). The client parses the received VT bytes into its own
local Terminal for rendering. Cell data, cursor position, and Terminal
modes are queried out of libghostty's `Terminal` API on each end; they
are not separate wire concepts.

This is the protocol's defining trait. Everything else follows from
it. See [ADR-0013] for the design rationale.

The protocol is organized in tiers per
[ADR-0015](../../ADR/0015-protocol-layering.md): **L1** (Terminal substrate,
MUST) and **L3** (Metadata storage, OPTIONAL service). The **L2** range is
reserved but carries no messages — there is no collection tier
([ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md);
see [L2.md](./L2.md)). The Terminal is the wire's
primary identity ([ADR-0016](../../ADR/0016-terminal-id-as-wire-primary.md));
session-window-pane-layout-focus vocabulary is a convention of the
reference TUI consumer, not a wire concept
([ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md)). See those
ADRs for the rationale that shapes this document.

[ADR-0013]: ../../ADR/0013-libghostty-bytes-on-wire.md

---

## 2. Terminology

| Term | Definition |
|------|------------|
| **Server** | A long-lived process owning all multiplexer state for one operating-system user. |
| **Client** | A process that attaches to a server, presenting Terminals to a user. |
| **Terminal** | A managed terminal: one PTY, one `libghostty_vt::Terminal` parsing its bytes, one stable `TerminalId`. The L1 substrate primitive (ADR-0015, ADR-0016). |
| **Group** | A named set of Terminals. Not a wire tier: membership and names are L3 metadata plus client logic, and atomic teardown is the L1 `KILL_TERMINALS` op (ADR-0030; see [L2.md](./L2.md)). `GroupId` survives only as an opaque grouping key. |
| **Metadata** | An L3 optional service: a typed key-value store the server hosts but does not interpret (ADR-0015 §"L3"). |
| **Frame** | A server-emitted `TERMINAL_OUTPUT` carrying a contiguous batch of VT bytes for one Terminal, identified by a monotonically increasing per-Terminal `seq`. |
| **Grid** | The two-dimensional cell matrix that is a Terminal's visible viewport. |
| **Scrollback** | Lines that have scrolled out of the grid but are retained for review. |
| **Cell** | One character position in a grid: a grapheme cluster plus rendering attributes. |
| **Tier** | A conformance layer: L1 or L3 (message catalog and §11 Conformance below). The L2 range is reserved but unused (see [L2.md](./L2.md)). |
| **Substrate consumer** | A consumer that speaks only L1: an agent, a recorder, a CI orchestrator. Sees Terminals; never sees Metadata. |
| **Reference TUI** | The first-party tmux-shaped consumer. Speaks L1+L3. Session, window, pane, layout, and focus are this consumer's conventions, implemented as L3 metadata; they are not wire concepts (ADR-0017). |

---

## 3. Architecture overview

```
┌────────────────────────────┐                  ┌─────────────────────────┐
│        phux server         │ ◄─── transport ►│      phux client        │
│                            │                  │                         │
│  L1: Terminals             │ TERMINAL_OUTPUT  │  Renderer               │
│  ├─ PTY                    │  (VT bytes, S→C) │  ├─ Terminal            │
│  └─ libghostty Terminal    │  ───────────────►│  │   (libghostty-vt;    │
│     (canonical)            │                  │  │    local parse for   │
│                            │     INPUT_KEY    │  │    rendering)        │
│  L3: Metadata    (opt)     │  ◄───────────────│  └─ Render loop         │
│  (L2 reserved, unused)     │                  │     (per-row dirty)     │
└────────────────────────────┘                  └─────────────────────────┘
```

The server is authoritative for all state. L1 (Terminal substrate) is
always on; L3 (Metadata) is an optional service the server may or may
not mount, and consumers opt in via `HELLO.layers`. The L2 range is
reserved but unused (no collection tier; see [L2.md](./L2.md)). The client's local libghostty `Terminal` is a mirror,
fed by the server's downsampled VT byte stream; the client's renderer
uses libghostty's `RenderState` per-row dirty tracking for efficient
redraw. The server is the only source of truth.

---

## 4. Transport

The protocol runs over any reliable, ordered, bidirectional, octet-
oriented byte stream. This version defines these concrete transports:

- **Unix domain socket** of type `SOCK_STREAM`, for local clients.
- **Standard I/O of an SSH command**, historically used for remote attaches and
  federation hubs dialing `ssh://` satellites ([ADR-0007]). The dialing side
  invokes `ssh host phux stdio-bridge`; the bridge splices stdin/stdout to the
  server UDS byte-transparently. SSH still supplies transport authentication and
  confidentiality, but exposes no independently verifiable workload channel
  binding. ADR-0098's closed policy modes therefore admit no SSH-stdio
  connection after their cutover; §10 owns the superseding security rule.
- **QUIC** (`quic://host:port`), for remote clients ([ADR-0007]). A
  single bidirectional QUIC stream carries the identical framing — a
  reliable, ordered octet stream, satisfying the property above. TLS 1.3
  is intrinsic to QUIC, so confidentiality is never optional; a routable
  listener additionally requires a per-attachment bearer token (§10).
  QUIC mandates ALPN: both ends MUST offer the exact protocol id
  `phux-quic/1` (`QUIC_ALPN` in `phux-protocol`) or the TLS handshake
  fails — a stray non-phux QUIC client never reaches the frame layer.

Future protocol versions MAY define additional transports (for example,
a UDP-based resilient transport in the style of Mosh). Such transports
MUST satisfy the reliable/ordered/bidirectional property; if they do
not, they require a new major protocol version.

[ADR-0007]: ../../ADR/0007-mosh-class-transport-and-satellites.md

### 4.1 Relay tunnel (QUIC)

A relay ([ADR-0051], [ADR-0057]) forwards a consumer's QUIC connection
to a server that dialed out to the relay, without ever parsing phux
frames. One relay endpoint serves both legs; the negotiated ALPN alone
— never the byte stream — decides the role:

- **Connector leg** — ALPN `phux-relay/1` (`QUIC_RELAY_ALPN` in
  `phux-protocol`; it MUST NOT equal `QUIC_ALPN`, ADR-0051 invariant 7).
  The dialing server opens one bidirectional stream and writes a
  length-prefixed auth preamble (`len: u32` big-endian + raw token
  bytes, at most 256 bytes, within 5 seconds), then keeps stream 0
  silent. Any byte on stream 0 after the preamble is a protocol
  violation; richer relay dialogue requires an ALPN bump, never in-band
  bytes (ADR-0051 invariant 4).
- **Consumer leg** — ALPN `phux-quic/1`, routed by TLS SNI to the
  enrolled route of the same name. An unknown or absent SNI is refused
  at the TLS layer (the certificate resolver declines; no phux-shaped
  error). The relay blind-splices the consumer's stream to the tunnel,
  including the consumer's own §10 bearer preamble — end-to-end
  authentication stays between consumer and server.

A relay refuses with these QUIC application close codes:

| Code   | Name                 | Meaning                                             |
|--------|----------------------|-----------------------------------------------------|
| `0x01` | `AUTH_FAILED`        | bad, missing, or unknown tunnel token (connector leg; mirrors the server listener's auth refusal) |
| `0x02` | `ROUTE_OFFLINE`      | enrolled route, no live tunnel — handshake completes, then app-close (distinguishes "server down" from "unknown route", which never gets past TLS) |
| `0x03` | `RECLAIMED`          | tunnel superseded by a newer claim on the same route (last-writer-wins) |
| `0x04` | `PROTOCOL_VIOLATION` | bytes on stream 0 after the auth preamble           |
| `0x05` | `OVER_CAP`           | relay at its connection cap — handshake completes, then app-close; existing connections unaffected |

The relay is a transport concern per the responsibility rule above: it
adds no frame, field, tag, or error code to the protocol.

[ADR-0051]: ../../ADR/0051-outbound-dial-out-connector-transport.md
[ADR-0057]: ../../ADR/0057-minimal-reference-relay.md

The transport is responsible for confidentiality, integrity, and its baseline
peer/server evidence. The protocol assumes all three and servers MUST reject a
transport that lacks them. Under paired policy, §6.1.1 additionally supplies
workload identity and scoped authority; transport admission cannot substitute.

---

## 5. Framing

Every message on the wire is a length-prefixed frame:

```
 0               1               2               3
 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7
+---------------+---------------+---------------+---------------+
|                       length (u32, BE)                        |
+---------------+-----------------------------------------------+
|   type (u8)   |                  payload ...                  |
+---------------+-----------------------------------------------+
|                          ... payload                          |
+-------------------------------------------+-------------------+
                                            |  (end of frame)
                                            +
```

- `length` is the number of bytes following the length field — i.e. the
  `type` byte plus the payload. A frame is therefore `4 + length` bytes
  total.
- `length` MUST be at least `1` (for the `type` byte) and at most
  `16_777_216` (16 MiB). A peer receiving a frame with `length` outside
  this range MUST send `ERROR { code: FRAME_TOO_LARGE }` and close the
  transport.
- `type` is the message discriminant. See the per-tier message catalogs
  in [L1.md](./L1.md), [L2.md](./L2.md), [L3.md](./L3.md), and the
  proto-tier catalog in §7.1 below.
- The payload format is determined by `type`.

There is no second framing layer. Application-level structure is encoded
within the payload as defined per-message and per-field. On a
message-oriented transport, where one message carries exactly one frame, a
message whose size disagrees with the `length` it declares — including one
too short to hold the length field — is the same framing violation and
receives the same treatment: `ERROR { code: FRAME_TOO_LARGE }`, then close.

---

## 6. Version and profile negotiation

This document specifies protocol `0.8.0`. Major/minor identify the wire
contract and MUST match exactly; patch differences are allowed and never change
encoded bytes. Protocol `0.7.x` and `0.8.x` reject each other. Every stateful
connection, including same-UID Unix sockets, performs HELLO. `PING` is the only
frame permitted before HELLO.

### 6.1 HELLO / HELLO_OK

The client speaks first. Both bodies are field-tagged TLV:

```
HELLO {
    client_name: str,                 // field 1
    protocol_major: u16,              // field 2
    protocol_minor: u16,              // field 3
    protocol_patch: u16,              // field 4
    client_caps: ClientCapabilities,  // field 5, required positional sub-record
    workload_profile: optional<str>,  // field 6; §6.1.1
    workload_client_nonce: optional<bytes32>, // field 7; §6.1.1
}

HELLO_OK {
    protocol_major: u16,              // field 1
    protocol_minor: u16,              // field 2
    protocol_patch: u16,              // field 3
    server_caps: ServerCapabilities,  // field 4
    server_id: bytes,                 // field 5
    selected_profile: BootstrapProfile, // field 6, required
    max_chunk_bytes: u32,             // field 7, required
    max_history_page_bytes: u32,      // field 8, required
    workload_grant: optional<WorkloadGrant>, // field 9; §6.1.1
}
```

`server_id` is an authenticated opaque server-incarnation value. It remains
stable only while reconnect-safety state is preserved and changes on any
restart/re-exec that loses that state. Consumers compare bytes only.

The server accepts HELLO only when `major.minor` matches, then returns its
current patch. Otherwise it sends fatal `VERSION_INCOMPATIBLE` and closes before
state. It intersects `layers`, selects exactly one profile by §6.2, and selects
each byte bound as `min(client, server)`. Missing required 0.7 fields, zero
bounds, or bounds over their hard caps are `MALFORMED_MESSAGE`. No profile is
`CODEC_UNAVAILABLE`.

Unknown top-level field ids are skipped by declared length. Required known
fields do not acquire legacy defaults: the clean cutover relies on the
major/minor admission gate rather than decoding a 0.6 HELLO shape as 0.7.
HELLO twice is a protocol error.

### 6.1.1 phux-workload/v1 authentication

<!-- impl-status: spec-only; probe: WorkloadChallenge,WorkloadResponse,WORKLOAD_AUTH -->
> **Status: spec-only.** The terminal mapping of the workload-auth profile has
> no codec, policy implementation, registry, or classifier yet.

The two HELLO workload fields SHALL be both absent or both present. When
present they carry the exact profile string `phux-workload/v1` and a fresh
32-byte client nonce. A server using paired policy checks `major.minor` first,
then maps the endpoint-neutral profile in
[workload-auth.md](./workload-auth.md) as follows:

```text
HELLO
  -> WORKLOAD_CHALLENGE (S -> C, 0x84)
  -> WORKLOAD_RESPONSE  (C -> S, 0x04)
  -> HELLO_OK
```

The terminal service string is `phux-terminal`. No frame may interleave between
challenge and response. HELLO_OK field 9 carries the strict `WorkloadGrant`
image defined by the profile, and `server_id` SHALL equal the 16-byte
incarnation signed in the challenge. A paired client requires
`ServerFeature::WORKLOAD_AUTH`, a valid challenge, and the grant; receiving
HELLO_OK without them is downgrade, not permission to continue.

Version rejection precedes workload-offer parsing, key lookup, and proof
generation. Authentication-frame fields are strict and canonical: unknown or
duplicate fields, missing fields, non-minimal varints, unknown scope bits or
selector tags, and nested or body trailing bytes are fatal
`MALFORMED_MESSAGE`. Ordinary HELLO fields retain the extensible TLV rule above.

The exact challenge/response fields, transcript bytes, channel bindings,
ScopeSet encoding, policy modes, total client-frame/command classification,
denial semantics, and live revocation are normative in
[workload-auth.md](./workload-auth.md). The workload profile authenticates an
endpoint connection; it does not merge the terminal protocol with the separate
durable coordinator endpoint
([ADR-0092](../../ADR/0092-durable-work-coordinator-authority.md)).

The `layers` intersection retains ADR-0015 semantics: L1 is mandatory, L2 is
reserved/unmounted, and L3 is optional. Neither peer sends out-of-intersection
tier frames.

### 6.2 Capability and synchronization profile negotiation

```
OutputMode = enum (u8) {
    Raw       = 0,   // compatibility preference only
    StateSync = 1,
}

BootstrapProfileKind = bitset (u8) {
    SynthesizedVtRaw        = 0x02,
    SynthesizedVtStateSync  = 0x04,
    NativeState             = 0x08,
    // 0x01 is permanently retired: incomplete pre-bounded-history NativeState.
}

EngineCodecSet = bitset (u64) {
    LibghosttyCheckpointV2 = 1 << 2,
}

ServerFeature = bitset (u32) {
    ACKNOWLEDGED_INPUT = 0x00000010, // APPLY_INPUT (L1.md §6.2.1; ADR-0053)
    FILE_UPLOAD        = 0x00000020, // PUT_FILE (L1.md §6.2.2; ADR-0059)
    MOVE_TERMINAL      = 0x00000040, // MOVE_TERMINAL (L1.md §3.1; ADR-0056)
    TERMINAL_REPLY     = 0x00000080, // INPUT_TERMINAL_REPLY (L1.md §3.4; ADR-0070)
    SHUTDOWN           = 0x00000100, // SHUTDOWN (L1.md §5.1)
    SPAWN_INITIAL_SIZE = 0x00000200, // SPAWN_TERMINAL.initial_size (L1.md §3.1)
    REPORT_AGENT_STATE = 0x00000400, // REPORT_AGENT_STATE (L1.md §5.1; ADR-0085)
    GET_PERF           = 0x00000800, // GET_PERF (L1.md §5.1; ADR-0096)
    WORKLOAD_AUTH      = 0x00001000, // phux-workload/v1 (§6.1.1; ADR-0098)
}

EngineFeatureSet = bitset (u32) {
    CONTINUATION            = 0x00000001,
    READY_BOUNDARY          = 0x00000002,
    HISTORY_PAGES           = 0x00000004,
    BOUNDED_HISTORY_CONTROL = 0x00000008,
}

BootstrapProfile = tagged_union {
    SynthesizedVtRaw,                      // tag 1
    SynthesizedVtStateSync,                // tag 2
    NativeState {                          // tag 3
        codec: EngineCodec,                // exact u8 version: v2 = 2
        features: EngineFeatureSet,
    },
    // tag 0 is permanently retired: incomplete pre-bounded-history NativeState.
}
```

The three profile variants are the complete mode matrix. Native always means
exact checkpoint plus byte-identical raw PTY continuation; there is no native
StateSync value to encode. `OutputMode` chooses a preferred synthesized profile
only. Native is selected first when both peers advertise it, share an exact
codec, and the feature intersection contains all four required v2 features,
including `BOUNDED_HISTORY_CONTROL`. The current native offer bit/tag are
`0x08`/`3`; legacy native bit/tag `0x01`/`0` are permanently retired and ignored,
so mixed peers fall back to a commonly advertised synthesized
profile or fail with `CODEC_UNAVAILABLE` before attach. Otherwise the selected
synthesized variant must be in both advertised sets. No fallback occurs after
HELLO_OK.

`ClientCapabilities` is one positional sub-record inside HELLO field 5. Protocol
0.8 fixes this exact order:

```
color: u8
layers: u8
images: u8
kbd_protocols: u8
hyperlinks: u8
output_mode: u8
default_colors_present: u8
default_colors: foreground_rgb24 || background_rgb24  // iff present = 1
bootstrap_profiles: u8
native_codecs: u64
native_features: u32
max_chunk_bytes: u32
max_history_page_bytes: u32
```

Unknown set bits are ignored. Unknown enum tags, truncated records, and palette
presence other than 0/1 are malformed. Bounds are nonzero and at most 8 MiB
each. Reference advertisements are 256 KiB bootstrap chunks and 1 MiB history
pages. HELLO_OK repeats the negotiated limits. A receiver rejects a chunk/page
above the negotiated bound before allocating it; opaque cursors are at most
4 KiB.

`ServerCapabilities` remains a positional prefix: `layers: u8` followed by
optional `features: u32`. A one-byte legacy value therefore decodes with an
empty feature set. `ACKNOWLEDGED_INPUT = 0x10`, `FILE_UPLOAD = 0x20`,
`MOVE_TERMINAL = 0x40`, `TERMINAL_REPLY = 0x80`, `SHUTDOWN = 0x100`,
`SPAWN_INITIAL_SIZE = 0x200`, `REPORT_AGENT_STATE = 0x400`,
`GET_PERF = 0x800`, and `WORKLOAD_AUTH = 0x1000`; unknown feature bits are
ignored. A client MUST use the corresponding frame only when its feature is
advertised. In particular, the absence of `TERMINAL_REPLY` in an
otherwise valid `HELLO_OK` is authoritative: that server does not accept
`INPUT_TERMINAL_REPLY`.

`SPAWN_INITIAL_SIZE = 0x200` is the one bit that gates a field rather than a
frame, and its unadvertised case is degrading rather than dangerous: a server
without it skips the unknown field id by length and spawns at its default,
which is what happened before the field existed. A client SHOULD still
require the bit, because it is what distinguishes "the pane already has the
geometry I asked for" from "the pane is at some default and my follow-up
`TERMINAL_RESIZE` is load-bearing."

Color/image/keyboard/hyperlink rewriting applies only to synthesized
compatibility profiles. For `NativeState`, `BOOTSTRAP_CHUNK`,
`HISTORY_PAGE.payload`, cursors, and subsequent `TERMINAL_OUTPUT.bytes` are
engine-owned opaque bytes and MUST remain byte-identical across server,
transport, recorder, and federation relay. phux never scans or rewrites native
records or raw live bytes.

### 6.3 Extension discipline: capabilities add, versions break

§6 and §6.1 make `major.minor` equality a hard admission test with no
grace window: a peer one minor apart is rejected at HELLO, before any
stateful frame. The consequence is a rule about *how the wire grows*, and
it is normative for anyone proposing a change to this document.

- A `minor` bump is a **fleet-wide break**. Every consumer and every server
  in a deployment change together or stop talking to each other. There is
  no version range, no dual-speaking peer, and no deprecation window in
  which an old client keeps working with reduced function.
- Therefore a new frame type, a new `Command` tag, a new `ErrorCode`, or a
  new field on an existing message MUST be introduced as an **optional,
  negotiated capability** whenever the wire admits one — a `ServerFeature`
  bit, a `ClientCapabilities` byte, or an additive field id that decoders
  skip by length ([appendix-encoding.md](./appendix-encoding.md)) — and
  MUST NOT be introduced by bumping `minor` if a capability-gated shape
  exists. Peers that do not implement the capability keep interoperating
  unchanged, which is the property a version bump destroys.
- A `minor` bump is reserved for changes that **cannot** be expressed
  additively: renumbering a tag, changing the meaning of bytes already on
  the wire, reallocating a freed tag to unrelated behavior
  ([appendix-reserved.md §2](./appendix-reserved.md)), or removing a
  message peers depend on. Proposing one is proposing a synchronized
  upgrade of every deployment, and the PR SHALL say so explicitly.
- A capability bit is a permanent contract of its own. Once advertised it
  is not withdrawn or re-pointed at different behavior; the `CC_FRONTEND`
  slot above was reclaimed only because it had never shipped.

The practical consequence is that a design which "needs a wire change"
should first be re-derived over the frames that already exist. Session
recording is the worked example: a server-side recorder wanted a new
command tag and a new feature bit, and was rejected in favor of a
consumer-side projection over the existing `ATTACH_TERMINAL` observer
contract, precisely because the durability it bought did not justify a
fleet-wide break. See
[ADR-0061](../../ADR/0061-capabilities-add-versions-break.md) for the
decision and [ADR-0060](../../ADR/0060-self-contained-session-recording.md)
for that cost analysis.

### 6.4 Frame compression

An optional, negotiated, **decode-invisible** transform. It changes how many
bytes a frame costs on the wire and nothing else: the frame a receiver
dispatches is byte-for-byte the frame the sender encoded, which is what keeps
it compatible with §6.2's rule that native records remain byte-identical
across server, transport, recorder, and federation relay.

```
Compression = enum (u8) {
    None    = 0,
    Deflate = 1,   // raw DEFLATE, RFC 1951 (no zlib or gzip wrapper)
}

CompressionSet = bitset (u8) {
    DEFLATE = 0x01,
}
```

**Negotiation.** `HELLO` carries an optional top-level field `compression`
(field id 6, `u8` `CompressionSet`): the algorithms the client can inflate.
`HELLO_OK` answers with an optional top-level field `compression` (field id 9,
`u8` `Compression`): the single algorithm the server selected. Both are
additive field ids, skipped by declared length by a peer that does not know
them, per §6.3 — they are *not* members of the `ClientCapabilities` /
`ServerCapabilities` sub-records, whose byte order §6.2 fixes exactly.

An absent or zero field means no compression, and that is the compatibility
value on both sides. The server MUST NOT select an algorithm the client did
not offer, and MUST NOT emit `FRAME_COMPRESSED` when it selected `None`.
Unknown offer bits are ignored; an unknown selection is read as `None`, which
surfaces as a decode error on the first wrapped frame rather than as a silent
misreading of payload bytes.

**The envelope.**

```
FRAME_COMPRESSED {                  // 0x9A, S -> C
    algorithm: u8,                  // field 1, required
    uncompressed_len: u32,          // field 2, required
    payload: bytes,                 // field 3, required
}
```

`payload` is the compressed image of one complete **inner frame body**: its
type byte followed by its payload, i.e. everything a frame carries after the
length prefix. The receiver inflates it and dispatches the result exactly as
if those bytes had arrived unwrapped.

A sender MUST NOT wrap a frame whose body exceeds the larger of the two
negotiated payload bounds (`max_chunk_bytes`, `max_history_page_bytes`) plus a
64 KiB allowance for the inner frame's own ids, cursors and field headers. The
envelope is specified for the payload-bearing frames those bounds govern, and
the limit is what lets a receiver size its allocation from the handshake
rather than from a number the sender chose.

A receiver MUST:

- reject `uncompressed_len` of zero or above that bound **before** allocating.
  Bounding by the §5 frame cap alone would let a peer spend a few hundred
  bytes of payload to make the receiver allocate 16 MiB, repeatedly; bounding
  by the negotiated limits makes the worst case proportional to what the
  connection agreed to carry, and smallest for the peer that asked for the
  smallest bounds;
- inflate to **exactly** `uncompressed_len` bytes, rejecting both a stream
  that ends short and one that would run long — a decoder that accepted a
  short inflate would dispatch a truncated body, which can decode as a
  different message;
- reject an inner type byte of `FRAME_COMPRESSED`. Nesting has no use and
  each layer multiplies the work one received frame costs.

Each failure is `ERROR { MALFORMED_MESSAGE }` followed by close.

**Per frame, not per connection.** Wrapping is the sender's choice for each
frame. A sender SHOULD leave small frames unwrapped — a keystroke echo must
never pay a compressor — and MUST leave a frame unwrapped when the transform
does not shrink it. A receiver therefore sees wrapped and unwrapped frames
interleaved on one connection and treats that as normal.

**When to offer.** Compression trades CPU on both peers for bytes on the wire,
so a consumer SHOULD offer it only when the wire is worth paying for. The
reference TUI offers on its remote transports and offers nothing over the
local Unix socket, where the bytes never leave the machine. The payload this
exists for is the bootstrap prefix: a native checkpoint is whole engine pages
at a fixed width per cell, which is hundreds of kilobytes for one pane and
deflates by an order of magnitude.

---

## 7. Message catalog (proto tier)

Messages are identified by a single `u8`. The space is partitioned:

- `0x00 – 0x7F`: client-originated.
- `0x80 – 0xFF`: server-originated.

Within each half:

- `0x01 – 0x0F` / `0x80 – 0x8F`: connection lifecycle.
- `0x10 – 0x2F` / `0x90 – 0xAF`: high-frequency / hot path.
- `0x30 – 0x3F` / `0xC0 – 0xCF`: control plane.
- `0x40 – 0x4F` / `0xB0 – 0xBF`: events and signals.
- `0x7F` / `0xFF`: PING / PONG.

The catalog is organized by **tier** per
[ADR-0015](../../ADR/0015-protocol-layering.md):

- **proto** — protocol meta (lifecycle, flow control, errors).
  Required of every consumer that completes a HELLO. Not tier-
  specific. Defined here.
- **L1** — Terminal substrate. Every conforming consumer speaks L1
  (§11). Carries `TerminalId` per
  [ADR-0016](../../ADR/0016-terminal-id-as-wire-primary.md). See
  [L1.md](./L1.md).
- **L2** — reserved, no messages. There is no collection tier. See
  [L2.md](./L2.md).
- **L3** — Metadata storage. Optional service. See [L3.md](./L3.md).
- **cmd** — typed command messages. Carry an L1 or L3 payload
  depending on the variant (see each tier's commands section).

The **Status** column tracks reference-implementation coverage in this
repository. It does not constrain a conforming implementation — a consumer
MUST NOT read it as permission to skip a `spec-only` message it does
receive — but it is not decoration either: `just docs-check`'s
`impl-status` gate resolves every status cell in this document, in
[L1.md](./L1.md), [L3.md](./L3.md), and in
[appendix-reserved.md](./appendix-reserved.md) against the wire constants
in `crates/phux-protocol/src/wire/`, and fails when a cell and the codec
disagree in either direction. The same marker carries prose sections that
have no catalog row; the mechanism is defined in
[docs/CONVENTIONS.md](../CONVENTIONS.md).

- `shipped` — message is in [`phux_protocol::wire::frame::FrameKind`]
  and round-trips through the encoder/decoder.
- `partial` — message is on the wire but at least one end does not
  yet produce or consume it (e.g. the client does not yet emit
  `VIEWPORT_RESIZE` even though the frame round-trips).
- `spec-only` — defined here, no codec entry yet.
- `TBD` — message family is reserved at this tier but not yet
  wire-allocated. Discriminant byte will be assigned if and when the
  message ships. Decoders MUST NOT speculatively assume any particular
  discriminant slot.

[`phux_protocol::wire::frame::FrameKind`]: ../../crates/phux-protocol/src/wire/frame/kind.rs

### 7.1 proto frames — connection lifecycle and flow control

| ID    | Direction | Name              | Reference          | Status    |
|-------|-----------|-------------------|--------------------|-----------|
| 0x01  | C → S     | `HELLO`           | §6.1               | shipped   |
| 0x02  | C → S     | `ATTACH`          | [L1.md §replay](./L1.md) | shipped |
| 0x03  | C → S     | `DETACH`          | §7.2               | shipped   |
| 0x04  | C → S     | `WORKLOAD_RESPONSE`| §6.1.1             | spec-only |
| 0x16  | C → S     | `HISTORY_REQUEST` | [L1.md §history](./L1.md) | shipped |
| 0x21  | C → S     | `FRAME_ACK`       | §8                 | shipped   |
| 0x31  | C → S     | `COMMAND`         | [L1.md §5](./L1.md)| shipped   |
| 0x40  | C → S     | `SUBSCRIBE`       | §7.3               | spec-only |
| 0x7F  | C → S     | `PING`            | §7.4               | shipped   |
| 0x80  | S → C     | `HELLO_OK`        | §6.1               | shipped   |
| 0x81  | S → C     | `ATTACHED`        | [L1.md §replay](./L1.md) | shipped |
| 0x82  | S → C     | `DETACHED`        | §7.2               | shipped   |
| 0x83  | S → C     | `ATTACH_READY`    | [L1.md §replay](./L1.md) | shipped |
| 0x84  | S → C     | `WORKLOAD_CHALLENGE`| §6.1.1            | spec-only |
| 0x9A  | S → C     | `FRAME_COMPRESSED`| §6.4               | shipped   |
| 0xC1  | S → C     | `ERROR`           | §9                 | shipped   |
| 0xC2  | S → C     | `COMMAND_RESULT`  | [L1.md §5](./L1.md)| shipped   |
| 0xFF  | S → C     | `PONG`            | §7.4               | shipped   |

The `COMMAND` / `COMMAND_RESULT` envelope (§5, per
[ADR-0021](../../ADR/0021-control-plane-commands.md)) round-trips
through the codec. The wire carries `KILL_TERMINAL` (tag 0x03),
`GET_STATE` (tag 0x05), `KILL_TERMINALS` (tag 0x09),
`DETACH_CLIENTS` (tag 0x13), `APPLY_INPUT` (tag 0x14), and `PUT_FILE`
(tag 0x15), plus the agent-convenience commands `GET_SCREEN` (tag 0x07),
`ROUTE_INPUT` (tag 0x08), `GET_TERMINAL_STATE` (tag 0x0c), and
`SUBSCRIBE_TERMINAL_EVENTS` (tag 0x0d); the remaining §5.1 catalog entries
are reserved and decode as `UnknownEnumValue` until allocated.

`DETACH_CLIENTS { session: optional<str> }` force-detaches clients from
*outside* the attach UI (backs `phux detach`): its body is a presence
byte followed, when set, by a `u32`-length-prefixed UTF-8 session name.
`session = Some(name)` detaches every client attached to that session;
`session = None` detaches every attached client on the server. Each
target receives a `DETACHED` frame (§7.2) and its attachment is torn
down server-side, so its TUI exits cleanly. This is distinct from the
`DETACH` frame, which detaches only the sending connection. The reply is
`COMMAND_RESULT { OkWith(Json(count)) }` where `count` is the number of
clients detached; an unknown session name detaches nobody and reports
`0` (not an error). Scope: only session-attached clients (`ATTACH`
consumers) are targeted; terminal-level subscribers (`ATTACH_TERMINAL`)
have their own detach verb (`DETACH_TERMINAL`) and are not swept.
Authorization is not implied by socket reachability. Under local policy the
owner UDS receives the explicit operator grant. Under paired policy,
`DETACH_CLIENTS` requires `SIGNAL` on the resolved Group or Global selector
before the command handler runs
([workload-auth.md §7](./workload-auth.md)).

`KILL_TERMINALS { ids: Vec<TerminalId> }` is the one atomic
multi-terminal teardown operation
([ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)):
its body is a `u16` count followed by that many tagged `TerminalId`s,
applied all-or-nothing under the server's single `Mutex<ServerState>`
lock. The session-vocabulary verbs `CREATE_SESSION` and
`KILL_COLLECTION` that earlier drafts placed on L1 are removed per
ADR-0030: create decomposes into `SPAWN` plus an L3 metadata key, and
group teardown is `KILL_TERMINALS`. Group lifecycle is L3 metadata plus
client logic, not a wire tier; see [L3.md](./L3.md). The agent-surface
commands are engine-convenience snapshots over the shared engine, not a
normative structured wire contract (ADR-0030); the structured agent
state is a local projection exposed via the CLI and a versioned JSON
schema, owned by [../consumers/agents.md](../consumers/agents.md).

### 7.2 DETACH / DETACHED

`DETACH` (client → server) signals the client is leaving cleanly.

```
DETACH { }
```

`DETACHED` (server → client) is sent when the server is ending the
session, the client's attach was forcibly closed, or after a successful
`DETACH` is acknowledged. After `DETACHED`, the server MUST close the
transport.

```
DETACHED {
    reason:  optional<DetachReason>,   // field id 1
    message: optional<str>,            // field id 2
}

DetachReason = enum {
    REQUESTED         = 0,  // a detach was asked for: the client's own
                            //   DETACH, or an operator's DETACH_CLIENTS
                            //   sweep on its behalf
    SERVER_SHUTDOWN   = 1,
    SESSION_KILLED    = 2,  // legacy name; retained for wire compat.
                            //   Means "the group the attach was rooted
                            //   in was torn down" (now a KILL_TERMINALS
                            //   over its members; see L2.md / ADR-0030).
    REPLACED          = 3,  // another client took over an exclusive attach
    PROTOCOL_ERROR    = 4,
    AUTHENTICATION_FAILED = 5,  // phux-workload/v1 admission failed
    AUTHORIZATION_REVOKED = 6,  // live workload registry authority withdrawn
    AUTHORIZATION_EXPIRED = 7,  // live workload grant reached expires_at
    INTERNAL_ERROR    = 255,
}
```

<!-- impl-status: spec-only; probe: AUTHENTICATION_FAILED,AUTHORIZATION_REVOKED,AUTHORIZATION_EXPIRED -->
> **Status: spec-only.** Detach reason values 5 through 7 land with the
> `phux-workload/v1` handshake and live-revocation implementation.

Both fields are optional-absent, which is what makes them additive under
§6.3: a server that predates `0.7.0-draft.7` encodes an empty `DETACHED`
body, and one that states no reason encodes the same empty body, so the
common case is byte-identical to what every `0.7.0` peer already emits.

- A sender SHOULD state a `reason` whenever it knows one. It MUST NOT
  encode `reason` merely to fill the field: an absent reason is honest,
  a wrong one is not.
- A consumer MUST treat an absent `reason` as *unstated*. It MUST NOT
  infer `REQUESTED` — "the server did not say" and "you asked for this"
  are different endings, and only one of them is worth explaining to a
  user.
- A consumer MUST tolerate a `reason` value it does not recognise,
  treating it exactly as absent. Failing the frame is forbidden:
  `DETACHED` plus transport close is the only ending a consumer may act
  on (§9), so refusing to decode it converts an explained ending into an
  unexplained transport error — and would make every later
  `DetachReason` allocation a fleet-wide break rather than an additive
  one.
- `message` is diagnostic text for a human or a log. A consumer MUST NOT
  parse it or condition behavior on it; `reason` is the contract.

`DetachReason` values are allocated sequentially from `0`, with `255`
reserved for `INTERNAL_ERROR`; a new value is additive and needs no
version bump ([ADR-0061](../../ADR/0061-capabilities-add-versions-break.md)).

<!-- impl-status: partial; probe: DetachReason -->
> **Status: partial.** The frame, both fields, and the consumer surface
> are shipped. The reference server states `REQUESTED` (for a client's
> `DETACH` and for a `DETACH_CLIENTS` sweep) and `SESSION_KILLED` (when
> the group an attach was rooted in is reaped). Server-wide cancellation gives
> each connection a bounded drain through `SERVER_SHUTDOWN` before close;
> fatal protocol paths order their final `ERROR`, `PROTOCOL_ERROR`, and close.
> It does not emit `REPLACED`, because role takeover is unimplemented (§7.1),
> or `INTERNAL_ERROR`, because the reference server has no internal-failure
> path that deliberately closes an otherwise valid connection. Transport
> failure can still prevent delivery, so consumers must keep treating a bare
> disconnect as an ending.

### 7.3 SUBSCRIBE

<!-- impl-status: spec-only; probe: TYPE_SUBSCRIBE -->
> **Status: spec-only.** Discriminant `0x40` is reserved and nothing decodes
> it. Per-Terminal event opt-in exists separately as `SUBSCRIBE_EVENTS`
> (`0x41`) and `SUBSCRIBE_TERMINAL_EVENTS` (command tag `0x0d`).

Reserved for opting in/out of notification streams (e.g. only the focused
client should receive `BELL` for inactive Terminals). Format not yet
defined.

### 7.4 PING / PONG

```
PING { nonce: u64 }
PONG { nonce: u64 }
```

A peer receiving `PING` MUST respond with `PONG` carrying the same nonce
within a reasonable interval. PING/PONG is liveness only — clients and
servers MAY use it for keepalive; absence of pongs SHOULD NOT be
interpreted as anything other than a transport failure.

---

## 8. Generation-scoped flow control

### 8.1 Live sequence and pacing

The Terminal actor stamps one checked, non-wrapping `u64` sequence before
broadcast. `TERMINAL_OUTPUT` is scoped by
`(terminal_id, stream_id, bootstrap_id, seq)`. A bootstrap cut is inclusive:
its checkpoint covers `seq <= base_seq`; subscribed duplicates at or below the
cut are discarded; only contiguous `seq > base_seq` is queued and released.
The first live frame after that generation's `BOOTSTRAP_READY` is
`base_seq + 1`.

Servers MAY coalesce adjacent bytes while preserving sequence/content order and
MUST bound every per-client queue. Native raw bytes are never rewritten.
Compatibility rewriting follows §6.2. A gap, duplicate, wrap, age/byte overflow,
resize, or relay discontinuity produces `BOOTSTRAP_TOMBSTONE`, not a silent
drop; no old-generation data or history status follows the tombstone.

### 8.2 READY and acknowledgement timing

There is no client ACK for bootstrap or raw live output. The reliable ordered
transport and protocol `BOOTSTRAP_READY` are the publication fence: a client
publishes staged state when it consumes READY, and ordered raw output may follow
immediately. This intentionally avoids one RTT per pane.

`FRAME_ACK` is valid only for `SynthesizedVtStateSync`:

```
FRAME_ACK {
    terminal_id: TerminalId, // field 1
    seq: u64,                // field 2, cumulative
    stream_id: StreamId,     // field 3
    bootstrap_id: BootstrapId, // field 4
}
```

The client sends it only after applying all StateSync transitions through
`seq = N` to the published compatibility terminal. It is cumulative within the
exact `(terminal, stream, bootstrap)` tuple. A raw-profile ACK or an ACK for a
tombstoned/wrong generation is `MALFORMED_MESSAGE`; it never gates native raw
release.

### 8.3 Bounds, isolation, and fairness

Bootstrap chunks and history pages obey HELLO_OK's negotiated limits before
allocation. History has at most one outstanding request per logical stream,
explicit scroll demand outranks prefetch, and all history work ranks below live
writes and READY-prefix bootstrap work. Multi-pane attach sends BEGIN in stable
snapshot order and chunks in bounded round-robin turns. A pane opens its live
queue at its own READY rather than waiting for other panes or history.

Capture leases, post-cut raw queues (bytes and age), outbound queues, history
work, and local caches are bounded independently per consumer. Overflow
tombstones the affected generation; other consumers and panes continue.

---

## 9. Errors

Errors carry a structured code and a human-readable message:

```
ERROR {
    request_id: optional<u32>,   // present if the error is associated with a COMMAND
    code: ErrorCode,
    message: str,
}

ErrorCode = enum {
    VERSION_INCOMPATIBLE = 1,
    UNKNOWN_MESSAGE_TYPE = 2,
    MALFORMED_MESSAGE    = 3,
    FRAME_TOO_LARGE      = 4,
    // 5 was reserved for withdrawn OUT_OF_TIER and is never reused.
    CODEC_UNAVAILABLE    = 6,
    NOT_ATTACHED         = 100,
    ALREADY_ATTACHED     = 101,
    SESSION_NOT_FOUND    = 102,  // shipped name; the requested session
                                 //   (now an L3 grouping) does not exist
    WINDOW_NOT_FOUND     = 103,  // shipped name; the requested window
                                 //   (a TUI L3 convention) does not exist
    TERMINAL_NOT_FOUND   = 104,  // renamed from PANE_NOT_FOUND per ADR-0016
    CLIENT_NOT_FOUND     = 105,
    UNSUPPORTED_SATELLITE_ROUTE = 106,  // no route for a SATELLITE-tagged id:
                                 //   the server is not a federation hub, or
                                 //   the host is absent from its satellite
                                 //   registry (a configuration refusal)
    SATELLITE_UNREACHABLE = 107, // ADR-0007: the hub knows the satellite but
                                 //   its outbound link is down, dialing,
                                 //   refused fail-closed (ADR-0038), or was
                                 //   lost before the relayed reply arrived.
                                 //   Transient — a retry may succeed once the
                                 //   hub's link supervisor reconnects.

    INVALID_COMMAND      = 200,
    PERMISSION_DENIED    = 201,
    RESOURCE_EXHAUSTED   = 202,
    UNSAFE_PASTE         = 203,  // APPLY_INPUT rejected an unsafe paste before
                                 //   writing any batch bytes
    INPUT_LEASE_HELD     = 204,  // ADR-0033: cooperative ACQUIRE_INPUT lost
                                 //   to an existing input-lease holder
    INPUT_DELIVERY_UNKNOWN = 205,// APPLY_INPUT reached PTY handoff but write /
                                 //   flush completion is indeterminate
    CANONICAL_LIMIT_EXCEEDED = 206, // phux-mjmc: the pane is in canonical
                                 //   (ICANON) mode and the batch's encoded
                                 //   bytes contain a line longer than the
                                 //   pane's canonical-line limit with no
                                 //   terminator; refused before any bytes
                                 //   were written, distinct from 205 (that
                                 //   code means delivery is unconfirmed,
                                 //   this one means delivery is known unsafe)
    INPUT_NOT_WRITTEN    = 207, // phux-w7z2.60: APPLY_INPUT was refused, or
                                 //   its pending write abandoned, at a point
                                 //   provably before any live PTY writer saw
                                 //   it (no PTY, a writer-side queue full or
                                 //   closed, or the pane's actor gone before
                                 //   handoff); distinct from 205 the same way
                                 //   206 is: 205 means delivery could not be
                                 //   confirmed, this one means delivery is
                                 //   known never to have been attempted

    INTERNAL_ERROR       = 65535,
}
```

This catalog tracks the shipped `#[non_exhaustive]` `ErrorCode` enum in
`phux-protocol`. `CODEC_UNAVAILABLE` is fatal during HELLO: there is no shared
explicit profile, exact native codec, or required feature intersection.
`OUT_OF_TIER = 5` was reserved by an earlier specification but never shipped;
the slot stays retired rather than being reused. Unknown codes are surfaced,
never mapped to a placeholder.

A fatal **protocol** error MUST be followed by
`DETACHED { reason: PROTOCOL_ERROR }` and transport close. Workload
authentication failure, revocation, and expiry use their specific §7.2 reasons
([workload-auth.md §8](./workload-auth.md)); they are fatal policy outcomes, not
protocol errors.

Receipt of an `ERROR` is not itself an ending. A consumer MUST NOT treat the
receipt of an `ERROR` as terminating its attach or its connection. A
connection ends by `DETACHED` followed by transport close, and by nothing
else; until one of those arrives the consumer SHOULD surface the message and
continue processing frames. This is the receiver half of the sender
obligation above: the sender decides fatality and announces that decision
with `DETACHED`, never with the code alone. A server legitimately emits the
same code both fatally and non-fatally — `MALFORMED_MESSAGE` for a frame it
cannot decode closes the connection, while `MALFORMED_MESSAGE` for a value
inside a COMMAND does not — so no receiver-side table of "fatal codes" is
sound.

What a code does tell a receiver is its scope: how far to degrade.

| Scope | Codes | What the receiver keeps |
|---|---|---|
| Terminal | `UNKNOWN_MESSAGE_TYPE`, `MALFORMED_MESSAGE`, `CODEC_UNAVAILABLE`, `TERMINAL_NOT_FOUND`, `UNSUPPORTED_SATELLITE_ROUTE`, `SATELLITE_UNREACHABLE`, `RESOURCE_EXHAUSTED`, `INTERNAL_ERROR` | every other Terminal, the layout, and the attach |
| Request | `NOT_ATTACHED`, `ALREADY_ATTACHED`, `SESSION_NOT_FOUND`, `WINDOW_NOT_FOUND`, `CLIENT_NOT_FOUND`, `UNSAFE_PASTE`, `INPUT_LEASE_HELD`, `INPUT_DELIVERY_UNKNOWN`, `CANONICAL_LIMIT_EXCEEDED`, authenticated operation `PERMISSION_DENIED` | all projected state; the correlated request owns the outcome |
| Connection | `VERSION_INCOMPATIBLE`, `FRAME_TOO_LARGE`, `INVALID_COMMAND`, admission/revocation/expiry `PERMISSION_DENIED` | nothing beyond the frames still in flight |

`Connection` scope means the consumer SHOULD expect the server to close the
connection, not that the consumer closes it: the consumer keeps reading so
that the frames already in flight — including the `DETACHED` itself — are
observed rather than discarded.

`PERMISSION_DENIED` is deliberately contextual. During workload admission or
live revocation it is connection-fatal and is followed by a typed `DETACHED`.
After authentication, a scope miss is operation-scoped: a correlated request
receives the denial, no effect occurs, and the connection remains active. A
denied fire-and-forget frame is dropped and may receive a rate-limited
uncorrelated denial. The complete rule is in
[workload-auth.md §8](./workload-auth.md).

An `ERROR` carries no Terminal id, so a `Terminal`-scoped code with no
`request_id` names a failure the consumer cannot attribute to one Terminal.
It MUST still be surfaced rather than dropped. A consumer MUST treat a code
it does not recognise as `Terminal`-scoped: that is the reading that degrades
least and preserves the attach.

---

## 10. Security

Transport security is always the outer boundary. It provides confidentiality,
integrity, and baseline peer/server evidence:

- **Unix sockets:** local policy relies on filesystem permissions (mode `0600`,
  owned by the user). Servers MUST refuse broader permissions. Paired policy
  additionally requires workload proof and a kernel-authenticated uid/gid/pid
  channel binding.
- **SSH:** the SSH session provides transport authentication and channel
  confidentiality, but a stdio stream exposes no independently verifiable §6.1.1
  binding. No closed policy mode admits SSH-stdio after ADR-0098's cutover; a
  later workload profile must define `SSH_SESSION` before it can return.
- **QUIC and WSS:** TLS 1.3 provides confidentiality and server identity. A
  routable listener also keeps its bearer/certificate transport gate, then
  paired policy authenticates and scopes the workload with the exporter from
  that exact TLS connection.

[ADR-0098](../../ADR/0098-workload-proof-and-closed-scope-authority.md)
explicitly amends the earlier “no in-band auth” doctrine from
[ADR-0031](../../ADR/0031-remote-consumer-auth-and-encryption.md). The protocol
still defines no reusable cookie or bearer token. It now defines the
`phux-workload/v1` proof exchange because per-operation authority must be signed
over the negotiated endpoint, server incarnation, channel, requested scopes,
and expiry. Transport admission alone never satisfies paired policy. The exact
policy modes and secret boundaries are in
[workload-auth.md §9](./workload-auth.md).

---

## 11. Conformance

Conformance is **per-tier** per
[ADR-0015](../../ADR/0015-protocol-layering.md). An implementation
declares the tiers it speaks via `HELLO.layers` (§6.1) and must
satisfy the conformance requirements for each declared tier, plus
the protocol-meta requirements common to all consumers.

### 11.1 Common requirements (all consumers)

Every conforming consumer:

1. Frames every message per §5.
2. Performs the §6.1 HELLO handshake for protocol 0.7, including explicit
   profile/codec/features and nonzero negotiated bounds.
3. Rejects 0.6 shapes rather than applying compatibility defaults.
4. Skips unknown top-level TLV fields by declared length.
5. Implements `HELLO`, `HELLO_OK`, `ATTACH`, `ATTACHED`, `ATTACH_READY`,
   `DETACH`, `DETACHED`, `PING`, `PONG`, `ERROR`, `COMMAND`, and
   `COMMAND_RESULT`.

### 11.2 L1 conformance (REQUIRED — Terminal substrate)

Every conforming consumer additionally implements:

- **Terminal content:** generation-bound `TERMINAL_OUTPUT`,
  `BOOTSTRAP_BEGIN`, `BOOTSTRAP_CHUNK`, `BOOTSTRAP_READY`,
  `BOOTSTRAP_TOMBSTONE`, NativeState-only `HISTORY_REQUEST`, `HISTORY_PAGE`,
  `HISTORY_TOMBSTONE`, `HISTORY_REJECTED`, and StateSync-only `FRAME_ACK`.
- **Terminal lifecycle:** `TERMINAL_OPENED`, `TERMINAL_CLOSED`.
- **Structured events:** `TERMINAL_EVENT`, `BELL`; `ALERT` is recommended.
- **Input:** `INPUT_KEY`, `INPUT_PASTE`, `VIEWPORT_RESIZE`;
  `INPUT_MOUSE`, `INPUT_FOCUS`, `INPUT_TERMINAL_REPLY`, and `INPUT_RAW` are recommended.
- **L1 commands:** `SPAWN`, `ATTACH_TERMINAL`, `DETACH_TERMINAL`,
  `KILL_TERMINAL`, `RESIZE_TERMINAL`.

A pure L1 consumer (an agent, a recorder, a CI orchestrator) sets
`HELLO.layers = { L1 }`. The server MUST omit all L2 and L3 messages
to that consumer. The consumer MUST NOT send L2 or L3 messages.

See [L1.md](./L1.md) and [input.md](./input.md) for the frame
definitions.

### 11.3 L1+L3 conformance (RECOMMENDED for GUIs and shared TUIs)

A consumer that additionally declares `L3` in `HELLO.layers` MUST
implement, in addition to §11.1 and §11.2:

- **Metadata commands:** `GET_METADATA`, `SET_METADATA`,
  `DELETE_METADATA`, `LIST_METADATA`.
- **Metadata events:** `METADATA_CHANGED { scope, key }` and an
  appropriate `SUBSCRIBE_METADATA` subscription mechanism.

The server MUST implement L3 storage scoped by `MetadataScope`
(see [L3.md](./L3.md)). Values are opaque bytes; the server enforces
nothing beyond size limits.

### 11.4 L2 — reserved, no collection tier

There is no L2 collection lifecycle tier. The `L2` bit and discriminant
range stay reserved so the three-tier numbering is not reused, but no L2
message is allocated and no consumer declares `L2`. Group membership and
names are L3 metadata plus client logic; the one atomic need,
multi-terminal teardown, is the single L1 operation `KILL_TERMINALS`
(§7.1). See [L2.md](./L2.md) for the full statement and
[L3.md](./L3.md) for the grouping conventions that replace it.

The reference TUI is therefore an L1+L3 consumer (§11.3); it builds
sessions, windows, and panes from L3 metadata, not from a wire tier.

### 11.5 Out-of-tier messages

A peer receiving a message outside the negotiated tier intersection sends
`MALFORMED_MESSAGE` and may close with `PROTOCOL_ERROR`. The former
`OUT_OF_TIER = 5` proposal never shipped and is permanently retired; a future
optional tier allocates a new error code rather than reusing it.

### 11.6 Test suite

The reference test suite for this specification will live at
`crates/phux-protocol/tests/` and at `tests/conformance/` in the
implementation repository. Per-tier conformance suites are tracked
separately.
