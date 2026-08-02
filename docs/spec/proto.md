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
errors, security delegation to the transport, and the per-tier
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
  §5 Version negotiation and [L1.md](./L1.md)).
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
- **Standard I/O of an SSH command**, for remote attaches and for
  federation hubs dialing `ssh://` satellites ([ADR-0007]). The dialing
  side invokes `ssh host phux stdio-bridge`; the remote bridge process
  splices its stdin/stdout to the server's Unix domain socket on `host`,
  byte-transparently, so the identical framing flows over the SSH
  channel. Authentication and confidentiality are SSH's (the
  transport-responsibility rule below): the bridge holds an ordinary
  local UDS connection under the socket's owner-only permissions, and no
  bearer token is carried on this transport.
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

The transport is responsible for authentication and confidentiality.
The protocol assumes both. Servers MUST NOT accept connections on
transports that lack peer authentication appropriate to the deployment.

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
within the payload as defined per-message and per-field.

---

## 6. Version negotiation

The protocol uses semantic versioning: `major.minor.patch`. This
document specifies version `0.6.0`.

- **Major** and **minor** versions identify the implemented wire contract. The
  reference wire currently carries one concrete version rather than a range,
  so peers MUST have equal `major.minor` values. A peer encountering an unknown
  message type at that version MUST log and drop the message. A peer
  encountering a **field id** it does not recognize within a known message MUST
  skip that field by its declared length (the field-tagged TLV extensibility
  rule of [appendix-encoding.md](./appendix-encoding.md)).
- **Patch** version changes are editorial or behavior-preserving and MUST NOT
  change encoded bytes. Peers with equal `major.minor` values MAY differ in
  patch.

### 6.1 The HELLO handshake

Every connection opens with a HELLO exchange. The client speaks first:

```
Client → Server:  HELLO {
    version: Version,
    client_caps: ClientCapabilities,   // includes layers: bitset<Layer>
}

Server → Client:  HELLO_OK {
    version: Version,
    server_caps: ServerCapabilities,   // includes layers: bitset<Layer>
    server_id: bytes,
}
```

`server_id` is an opaque 128-bit server-incarnation value. It MUST remain
stable across connections to the same in-memory server state and MUST change
whenever reconnect-safety state is lost, including a normal restart or a
graceful re-exec that does not preserve that state. Consumers MUST compare it
as opaque bytes and MUST NOT derive host identity from it.

The current wire encodes one concrete client version. The server MUST accept it
only when its `major.minor` equals the server's supported `major.minor`, then
echo the server's current patch in `HELLO_OK`. If they differ, the server MUST
send `ERROR { code: VERSION_INCOMPATIBLE }` naming both versions and the older
peer to upgrade, then close before processing `ATTACH` or other stateful frames.

The `layers` bit-field on `ClientCapabilities` and `ServerCapabilities`
declares which conformance tiers (§11 Conformance) each side speaks. Per
[ADR-0015](../../ADR/0015-protocol-layering.md) §"Conformance tiers":

- The client's `layers` lists what it wants. L1 is always implied; a
  client MAY omit higher tiers (an agent SDK declares L1 only).
- The server's `layers` (in `HELLO_OK`) lists what it implements. L1
  is always implemented; the server MAY mount L3 or not. L2 is never
  mounted (no collection tier).
- The **negotiated tier set** is the intersection of the two `layers`
  bit-fields. The server MUST NOT send messages from tiers outside
  the intersection, and the client MUST NOT send messages from tiers
  outside the intersection. Decoders MUST treat the receipt of an
  out-of-tier message as a protocol error.

After `HELLO_OK`, the negotiated version and tier set govern the rest
of the connection. Sending HELLO twice on the same connection is an
error.

### 6.2 Capability negotiation

Capabilities are advertised once, at HELLO time, and apply for the life
of the connection. They are not renegotiated.

```
Layer = bitset (u8) {
    L1 = 0x01,   // Terminal substrate (always implemented; MUST be set)
    L2 = 0x02,   // reserved, unused — no collection tier (L2.md)
    L3 = 0x04,   // Metadata storage (optional service)
}

OutputMode = enum (u8) {
    Raw = 0,        // raw PTY byte broadcast (default; byte-faithful human path)
    StateSync = 1,  // per-consumer synthesized grid-delta tick (ADR-0018)
}

ClientCapabilities {
    kbd_protocols: bitset<KeyboardProtocol>,
    mouse_protocols: bitset<MouseProtocol>,
    color: ColorSupport,           // TrueColor | Indexed256 | Indexed16
    images: bitset<ImageProtocol>, // Sixel | KittyGraphics | Iterm2
    hyperlinks: bool,
    unicode_version: u8,
    rendering: RenderingMode,      // Diff | VtReplay (deprecated; see prose below)
    layers: bitset<Layer>,         // tiers the client speaks (§11; ADR-0015)
    output_mode: OutputMode,       // emitter the consumer wants
    default_colors: optional<{     // outer terminal OSC 10/11 defaults
        foreground: rgb24,
        background: rgb24,
    }>,
}

ServerCapabilities {
    layers: bitset<Layer>,         // tiers the server implements (§11; ADR-0015)
    features: bitset<ServerFeature>, // optional trailing u32
}

ServerFeature = bitset (u32) {
    ACKNOWLEDGED_INPUT = 0x00000010, // APPLY_INPUT (L1.md §6.2.1; ADR-0053)
    FILE_UPLOAD       = 0x00000020, // PUT_FILE (L1.md §6.2.2; ADR-0059)
}
```

`ServerCapabilities` is a positional prefix: `layers` is the first byte and
`features` is an optional trailing `u32` big-endian bitset. A one-byte legacy
value therefore decodes with an empty feature set. Decoders MUST ignore unknown
feature bits. A client MUST use `APPLY_INPUT` only when
`ACKNOWLEDGED_INPUT` is advertised and MUST use `PUT_FILE` only when
`FILE_UPLOAD` is advertised.

The HELLO body is field-tagged TLV per
[appendix-encoding.md](./appendix-encoding.md): `client_name`, the version
triple, and the `ClientCapabilities` blob each ride as a separate tagged
field. `ClientCapabilities` itself is a nested positional, big-endian,
length-prefixed sub-record carried inside its field's value, with the field
order `color`, `layers`, `images`, `kbd_protocols`, `hyperlinks`,
`output_mode`, then `default_colors` as a presence byte followed by foreground
and background `R,G,B` bytes when present. A decoder MUST accept every prefix
of that caps sub-record and
apply defaults for missing trailing bytes — a value that stops before
`output_mode` decodes as `OutputMode::Raw`, and an unknown `output_mode` tag
also decodes as `Raw` — and an absent `ClientCapabilities` *field* decodes to
the default capabilities. New capability bytes append after `output_mode`
inside the same field.

`default_colors` lets an interactive client report the effective foreground
and background returned by OSC 10/11 on its outer terminal. The server SHOULD
install them as the canonical Terminal's default colors before parsing child
output, so OSC 10/11 queries from programs inside phux receive the same answer
as they do outside it. This affects theme derivation, not SGR downsampling.
When several clients share a Terminal, the most recently attached client that
advertises `default_colors` is authoritative; an attach that omits the field
MUST NOT erase an established palette. Non-TTY and legacy clients omit it.

`output_mode` lets a consumer choose, per connection, which server emitter
serves its attached Terminals: `Raw` (the default) keeps the byte-faithful
low-latency PTY broadcast that interactive shells and TUIs rely on, while
`StateSync` opts into the per-consumer synthesized grid-delta tick
(ADR-0018) suited to agents and remote state-sync consumers. The server
suppresses the raw broadcast for a `StateSync` consumer so exactly one
emitter serves it. Raw stays the human default because synthesized ticks
add a visible local-typing latency floor and can lose byte-exact styling.

Under `StateSync`, `TERMINAL_OUTPUT.bytes` is the minimum-VT transition from
the consumer's reference grid to the live grid, synthesized once per tick and
RTT-paced, so a runaway producer bounds the consumer's re-parse *rate* rather
than streaming every intermediate frame; the resulting grid is equivalent to
what the `Raw` byte stream would produce (ADR-0018,
[ADR-0043](../../ADR/0043-state-diff-output-mode.md)). Whether the server
advances a consumer's reference **on emit** (the emit-once model, correct on a
reliable ordered transport) or **on `FRAME_ACK`** (the loss-tolerant model,
which re-diffs a dropped/un-acked frame against the last-acked reference so it
self-heals) is a **server-side emission strategy** chosen per consumer from the
transport/topology — it needs no `ClientCapabilities` field and changes no wire
bytes (`FRAME_ACK` and `seq` already round-trip). A consumer MUST NOT assume
which strategy serves it; both converge to the same grid.

The former `CC_FRONTEND` feature slot is **reclaimed** per
[ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md). Earlier drafts
reserved it for a server that could "speak tmux control mode as an
alternative frontend." Under ADR-0017 the reference TUI has no
protocol-level privilege, and `tmux control mode` (when added) is one
L1/L3 consumer among several — no capability bit required. The `0x10` slot now
advertises `ACKNOWLEDGED_INPUT`; older decoders ignore its additive trailing
field.

Servers MUST adapt outbound `TERMINAL_OUTPUT` (see [L1.md §state
synchronization](./L1.md)) byte streams to each
client's capabilities. The downsampling is performed as a server-side
**VT byte stream rewrite**, not a per-cell structured transform:

- **Color.** For a client advertising `Indexed256`, the server MUST
  rewrite truecolor SGR sequences (`CSI 38;2;R;G;B m` / `CSI 48;2;R;G;B m`)
  to their indexed equivalents (`CSI 38;5;N m` / `CSI 48;5;N m`) before
  forwarding. For a client advertising `Indexed16`, the server MUST
  further quantize to the standard / bright ANSI ranges
  (`CSI 3N m` / `CSI 9N m` and their background counterparts).
- **Images.** For each image protocol the client does not advertise
  (`Sixel`, `KittyGraphics`, `Iterm2`), the server MUST drop or
  transform the corresponding escape sequences before forwarding so the
  client never receives bytes for a protocol it cannot render.
- **Keyboard protocols.** APC keyboard-reply sequences (kitty keyboard
  protocol, modifyOtherKeys, etc.) MUST be gated to clients advertising
  the matching `kbd_protocols` bit; the server's canonical Terminal
  still processes them locally, but they are stripped from the outbound
  byte stream for clients that did not negotiate the protocol.
- **Hyperlinks (OSC 8) and other terminal features** SHOULD be stripped
  when the corresponding capability bit is unset.

The downsampling MUST be deterministic and MUST NOT alter the visible
grid state on the client beyond what the capability reduction implies.
See [ADR-0013] for the rationale and the byte-stream rewriter design.

The legacy `RenderingMode` field on `ClientCapabilities` (`Diff` vs.
`VtReplay`) is **deprecated** as of this revision: with `TERMINAL_OUTPUT`
carrying VT bytes, every client renders via local libghostty parse —
there is no longer a structured-diff alternative. Decoders MUST accept
the field for forward-compat and SHOULD ignore its value.

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

[`phux_protocol::wire::frame::FrameKind`]: ../../crates/phux-protocol/src/wire/frame.rs

### 7.1 proto frames — connection lifecycle and flow control

| ID    | Direction | Name              | Reference          | Status    |
|-------|-----------|-------------------|--------------------|-----------|
| 0x01  | C → S     | `HELLO`           | §6.1               | shipped   |
| 0x02  | C → S     | `ATTACH`          | [L1.md §replay](./L1.md) | shipped |
| 0x03  | C → S     | `DETACH`          | §7.2               | shipped   |
| 0x21  | C → S     | `FRAME_ACK`       | §8                 | shipped   |
| 0x31  | C → S     | `COMMAND`         | [L1.md §5](./L1.md)| shipped   |
| 0x40  | C → S     | `SUBSCRIBE`       | §7.3               | spec-only |
| 0x7F  | C → S     | `PING`            | §7.4               | shipped   |
| 0x80  | S → C     | `HELLO_OK`        | §6.1               | shipped   |
| 0x81  | S → C     | `ATTACHED`        | [L1.md §replay](./L1.md) | shipped |
| 0x82  | S → C     | `DETACHED`        | §7.2               | shipped   |
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
Authorization matches the rest of the control plane (`KILL_TERMINAL`,
`KILL_TERMINALS`): any peer that can reach the socket may issue it —
transport access (UDS permissions, or the paired-token wss gate) is the
trust boundary.

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
DETACHED { reason: DetachReason, message: str }

DetachReason = enum {
    REQUESTED         = 0,  // client asked
    SERVER_SHUTDOWN   = 1,
    SESSION_KILLED    = 2,  // legacy name; retained for wire compat.
                            //   Means "the group the attach was rooted
                            //   in was torn down" (now a KILL_TERMINALS
                            //   over its members; see L2.md / ADR-0030).
    REPLACED          = 3,  // another client took over an exclusive attach
    PROTOCOL_ERROR    = 4,
    INTERNAL_ERROR    = 255,
}
```

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

## 8. Flow control

### 8.1 Output pacing

The server MUST cap per-Terminal `TERMINAL_OUTPUT` emission at a
configurable refresh rate (default 60 Hz). Between emissions, PTY
bytes are accumulated and shipped as a single coalesced
`TERMINAL_OUTPUT` carrying the batched VT bytes. There is no "every
byte emits a frame" mode; that would not survive a `yes` flood.

Coalescing operates at the byte level: the server concatenates the
PTY's output across the pacing interval into the next
`TERMINAL_OUTPUT`'s `bytes` field. Because libghostty's parser is
deterministic over the full byte stream, coalescing has no observable
effect on the client's local Terminal state beyond timing.

### 8.2 Per-Terminal acknowledgement

Clients acknowledge `TERMINAL_OUTPUT` emissions they have processed
(applied to their local libghostty `Terminal`):

```
FRAME_ACK { terminal_id: TerminalId, seq: u64 }
```

`seq` is the monotonic per-Terminal sequence number from
`TERMINAL_OUTPUT` (see [L1.md §frame model](./L1.md)). An ack is
cumulative: acknowledging
`seq = N` implies all prior `TERMINAL_OUTPUT`s for that Terminal up
to and including `N` have been applied.

The server tracks per-client `last_acked_seq` per Terminal. When
`pending_unacked_bytes` (or equivalently the count of unacked
`TERMINAL_OUTPUT` emissions) for a Terminal exceeds a configurable
`flow_control_threshold` (default: 32 unacked emissions, per-server
configurable, never disable-able), the server:

1. Stops sending live `TERMINAL_OUTPUT` for that Terminal to that client.
2. Drops the queued byte backlog for that Terminal / client.
3. Emits a single `TERMINAL_SNAPSHOT` (see [L1.md §snapshots](./L1.md))
   synthesized from the server's canonical Terminal — `vt_replay_bytes`
   reproduces the current grid on a fresh client Terminal.
4. Resumes live `TERMINAL_OUTPUT` from the post-snapshot byte stream.
   The next `seq` after the snapshot establishes a fresh base
   (see [L1.md §state replay on attach](./L1.md)); clients MUST NOT
   assume `seq` continuity across the snapshot boundary.

This is the playbook Mosh uses, generalized to per-Terminal streams.
It ensures a slow client cannot block the server, and the worst-case
catch-up cost is one snapshot's worth of synthesized VT bytes, not an
unbounded queue of accumulated PTY output.

Scrollback that scrolls off during a backpressure-induced snapshot is
**not** retransmitted to the lagging client; clients that require
gap-free scrollback during heavy output SHOULD configure their server
with a higher `flow_control_threshold` or accept snapshot-driven
truncation. Servers MAY include bounded scrollback in
`TERMINAL_SNAPSHOT.scrollback_bytes` if configured to do so on
backpressure (implementation-defined; not normative).

### 8.3 Per-client isolation

Each connected client has its own outbound queue. A wedged client whose
queue exceeds its bound is forcibly disconnected with
`DETACHED { reason: PROTOCOL_ERROR }`. Other clients are unaffected.

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
    OUT_OF_TIER          = 5,   // RESERVED, not emitted: the L2 tier was
                                //   dissolved (ADR-0030), so no message can
                                //   arrive from an un-negotiated tier. The
                                //   shipped enum does not define this variant;
                                //   the value stays reserved for the §11.5 use.

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

    INTERNAL_ERROR       = 65535,
}
```

This catalog tracks the shipped `#[non_exhaustive]` `ErrorCode` enum in
`phux-protocol` (`wire::frame`), which is the source of truth for the
wire bytes. `OUT_OF_TIER = 5` remains reserved but is not emitted because the
L2 tier it guarded was dissolved by
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md).
Codes `102` and `103` ship under the names
`SESSION_NOT_FOUND` / `WINDOW_NOT_FOUND`; the substrate no longer carries
a session or window concept, so the names read as the TUI-convention
lookups they back. Decoders MUST accept the byte values regardless of the
name. Because the enum is `#[non_exhaustive]`, an unknown code value is
surfaced rather than mapped to a placeholder.

A fatal error MUST be followed by `DETACHED { reason: PROTOCOL_ERROR }`
and transport close.

---

## 10. Security

The protocol delegates authentication and confidentiality to the
transport.

- **Unix sockets:** rely on filesystem permissions (mode `0600`, owned
  by the user). Servers MUST refuse to create sockets with broader
  permissions.
- **SSH:** rely on the SSH session's authentication and channel
  confidentiality.
- **QUIC:** TLS 1.3 provides confidentiality and server identity (the
  client pins the self-signed certificate's fingerprint). A routable
  listener authenticates each attachment with a bearer token the client
  sends as the opening preamble of its stream — a transport
  responsibility, per the paragraph below, not a protocol frame.

The protocol does **not** define cookies, tokens, or in-band auth. If a
future deployment requires per-attachment authorization, it is the
transport's responsibility to deliver an authenticated peer identity to
the server.

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
2. Performs the §6.1 HELLO handshake with `versions` consistent with
   §6 ordering and `version` selection, and a non-empty `layers`
   bit-field with the `L1` bit set.
3. Tolerates unknown messages by logging and dropping them (§6).
4. Tolerates unknown trailing fields per the encoding rules
   ([appendix-encoding.md](./appendix-encoding.md)).
5. Implements protocol-meta messages:
   `HELLO`, `HELLO_OK`, `ATTACH`, `ATTACHED`, `DETACH`, `DETACHED`,
   `PING`, `PONG`, `ERROR`, `COMMAND`, `COMMAND_RESULT`.

### 11.2 L1 conformance (REQUIRED — Terminal substrate)

Every conforming consumer additionally implements:

- **Terminal content:** `TERMINAL_OUTPUT`, `TERMINAL_SNAPSHOT`,
  `TERMINAL_RESIZED`, `FRAME_ACK`.
- **Terminal lifecycle:** `TERMINAL_OPENED`, `TERMINAL_CLOSED`.
- **Structured events:** `TERMINAL_EVENT`, `BELL`. (`ALERT` is
  RECOMMENDED.)
- **Input:** `INPUT_KEY`, `INPUT_PASTE`, `VIEWPORT_RESIZE`.
  (`INPUT_MOUSE`, `INPUT_FOCUS`, `INPUT_RAW` are RECOMMENDED.)
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

A peer receiving a message from a tier outside the negotiated
intersection MUST send `ERROR { code: OUT_OF_TIER }` and SHOULD
close the connection with `DETACHED { reason: PROTOCOL_ERROR }`.

In practice no message can trigger this today: the only optional tier
was L2, dissolved by
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md),
so every conforming consumer speaks the same L1 (+ optional L3) surface.
`OUT_OF_TIER = 5` is therefore reserved (§9) and not emitted by the
reference implementation; the rule stands so a future optional tier
reinstates it without a renumber.

### 11.6 Test suite

The reference test suite for this specification will live at
`crates/phux-protocol/tests/` and at `tests/conformance/` in the
implementation repository. Per-tier conformance suites are tracked
separately.
