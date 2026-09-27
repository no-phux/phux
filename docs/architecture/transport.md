---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-18
---

# Transport abstraction

**TL;DR.** One length-prefixed frame codec rides five byte streams: Unix
domain socket, WebSocket, QUIC, WebTransport, and SSH-stdio. There is no
shared `Transport` trait. The server's accept loop is generic over a
crate-private `Incoming` listener that yields a `FrameReader` / `FrameWriter`
pair per connection; the client wraps its three lanes in `FrameReader` /
`FrameWriter` enums behind one `Connection`; outbound TLS establishment for
QUIC and WebSocket is the `phux-dial` crate, shared by the attach loop and the
federation hub.

---

The seam is the **frame**, not a trait object. Every transport delivers
complete encoded frames (`docs/spec/proto.md` §5, owned by
`phux_protocol::wire::framing`); everything above that seam — the per-client
dispatch loop, the `FrameKind` codec, attach and bootstrap lifecycles — is
written once and never names a concrete stream.

## Where the seam lives in code

**Server side** (`phux-server::transport`). Three crate-private traits:

- `FrameReader::read_frame` yields one complete encoded frame (length prefix
  included) or `None` at end of stream; `FrameWriter::write_frame` writes one
  pre-encoded frame, with a batched default for back-to-back frames.
- `Incoming` is the listener shape the accept loop in `runtime::client` is
  generic over: `accept()` returns a `(Reader, Writer, ConnectionIdentity)`
  triple, plus per-listener error disposition (whether a rejected peer is
  logged, rate-limited, or fatal). Concrete listeners are `UdsListener`,
  `WsListener` (plain TCP or TLS via `ServerStream`), `transport::quic::
  QuicListener`, and `transport::webtransport::WtListener` (feature
  `webtransport`, on by default). Each pairs with its own reader and writer
  types. `transport::tls` owns the persisted self-signed certificate, SAN
  coverage checks, and the rustls / quinn / wtransport server configs the
  TLS listeners share.

**Client side** (`phux-client::attach::connection`). `Dial` names the lane
(`Uds(PathBuf)`, `Quic(QuicDial)`, `Ws(WsDial)`); `Connection` holds a
`FrameReader` and `FrameWriter` enum with one variant per lane and does
HELLO negotiation, length-prefixed I/O, and the bootstrap-profile bookkeeping
over them. `phux-tui` drives that connection; every headless verb and
`phux-mcp` reach it through `phux-client`.

**On-demand QUIC listeners** (ADR-0120). Besides the listeners bound at
startup, a server opens a QUIC listener at runtime when its owner sends
`OPEN_LISTENER` over the Unix socket (`runtime/ephemeral_listener.rs`). It is
an ordinary QUIC listener except in two ways. It admits one in-memory token
instead of the pairing store (`QuicAdmission::Listener`), and it closes once
no connection has used it for its linger. `phux attach --ssh` drives it
through `phux bootstrap` over ssh, so ssh carries the bootstrap and never the
session.

**Hub side** (`phux-server::hub::link`). The federation hub dials satellites
through its own crate-private `LinkTransport` / `LinkConn` pair
(`connect`, `send_frame`, `recv_frame`); `NetLinkConn` has one variant per
lane, including the SSH child. The link supervisor, backoff, and relay
mailbox are written against those traits.

A new stream type is therefore additive: implement the reader/writer pair
(and `Incoming` or `LinkConn` as appropriate), and nothing above the frame
seam changes.

## Streams that exist today

- **Unix domain socket** — the local server/client link and the default
  path for a server and the clients attached to it on the same host.
  `$XDG_RUNTIME_DIR/phux/phux.sock`, owner-only directory (see
  [process-model.md](./process-model.md)).
- **WebSocket** — the same frames for browser consumers
  ([`phux-web`](../consumers/web.md)) and for `phux attach --ws`, the TCP
  fallback when UDP is blocked. One binary message is exactly one frame.
  The client pings after 10s of silence and disconnects after 30s with
  nothing inbound (`phux_dial::ws::WsKeepalive`), matching QUIC's idle pair.
- **QUIC** (via `quinn`, ADR-0007) — for remote clients. Every connection
  starts with one control stream carrying the identical codec. A client that
  offers `HELLO.quic_streams` may negotiate one extra client-opened bidi
  stream per attached Terminal, bound with `STREAM_BIND`, carrying that
  Terminal's output, bootstrap, history, acks, and input; relay/connector
  consumers and hub links stay single-stream (normative:
  [proto.md](../spec/proto.md) §4.2, [L1.md](../spec/L1.md) §4.9;
  [ADR-0115](../adr/0115-quic-stream-per-terminal.md)). TLS 1.3 plus a
  bearer-token preamble on routable listeners (ADR-0031), sharing the cert
  and token store. Opt-in via `phux server --quic <HOST:PORT>`.
  **Backpressure:** every QUIC writer that can outrun its path (server QUIC
  and WebTransport writers, `phux-relay`'s consumer leg) holds quinn's send
  window to the congestion window plus 16 KiB (`phux_dial::window`), so a
  slow link blocks the writer within about a round trip. The attach pump
  measures lag in time: a live chunk older than 250ms
  (`runtime::pump::STALE_OUTPUT_BUDGET`) fences the generation and resyncs
  that one pump to a fresh checkpoint (`ResyncAudience::Only`), so a slow
  remote consumer skips frames instead of queueing seconds of output, and no
  other consumer of the pane re-bootstraps. **Through a relay**, each tunnel
  stream gets only 64 KiB of receive credit
  (`TUNNEL_STREAM_RECEIVE_WINDOW`), so a slow consumer hop back-pressures the
  server writer the same way. The connector tags its tunnel's initial
  connection ID (`phux_dial::quic::TUNNEL_CID_PREFIX`) so the relay can pick
  the bounded config before ALPN is known; the tag selects a config, never a
  role. Untagged tunnels from older connectors get quinn's defaults. The
  64 KiB bound caps each bridged consumer at about 5 Mbit/s at 100 ms RTT;
  faster output resyncs even a healthy consumer. The relay route negotiates
  only the single-stream fallback.
- **WebTransport** (HTTP/3 CONNECT over QUIC) — QUIC-class transport for
  browsers, which cannot open raw QUIC connections. An HTTP/3 `CONNECT`
  session whose single bidirectional stream carries the identical
  length-prefixed frames; the HTTP/3 layer is a transport detail below the
  frame seam. Always TLS 1.3; a routable listener requires the same
  `phux pair` bearer token as the `wss://` path (ADR-0031), carried in the
  `CONNECT` request — `Authorization: Bearer <hex>` from native consumers, or
  `?token=<hex>` on the session URL from browsers (the JS `WebTransport` API
  cannot set request headers) — and refused with HTTP 403 before the session
  exists. Duplicate `Authorization` fields are refused on the CONNECT accept
  path before QPACK is collapsed into a header map. Shares the persisted
  certificate and token store with the WebSocket and QUIC listeners; binds
  its own UDP socket because browsers offer only the `h3` ALPN while the
  raw-QUIC endpoint advertises the phux-private one. Opt-in via
  `phux server --webtransport <HOST:PORT>` or `PHUX_WT_ADDR`; `phux-web`
  dials it first and falls back to WebSocket.
- **SSH-stdio** (ADR-0007) — frames the wire codec over a child SSH
  process's stdin/stdout. The dialing side spawns the system `ssh` binary
  (`$PHUX_SSH` overrides the program) running the remote `phux stdio-bridge`
  verb, which splices its stdin/stdout byte-transparently to the server's
  Unix socket on that host. SSH supplies authentication and encryption; the
  bridge holds an ordinary local UDS connection under the socket's
  owner-only permissions, so no bearer token or certificate pin is involved
  (ADR-0038 addendum). The only dialer today is the federation hub, for
  `ssh://` satellite endpoints: `BatchMode=yes`, a `--`-guarded,
  charset-validated argv (endpoint parts that could read as ssh options are
  rejected at hub-table validation), and the child exiting treated as a
  dropped link feeding the same capped-backoff redial loop as the QUIC/WS
  paths. **Keepalive / idle:** liveness lives at the SSH layer — the hub
  dials with `ServerAliveInterval` / `ServerAliveCountMax` derived from the
  same interval/timeout constants the WS path uses, so a silent partition
  makes the ssh child exit. The bridged phux stream stays byte-transparent.

All five run the same codec. A consumer that can frame the codec over a
stream is a peer regardless of which stream it uses.

## Outbound dialing is shared

The client-side establishment of the two TLS remote lanes — TLS 1.3 with a
fingerprint-pinned (or loopback skip-verify) certificate verifier, plus the
ADR-0031 bearer token — is the `phux-dial` crate (`QuicDial`, `WsDial`,
`CertTrust`), consumed by `phux-client`'s connection and by the hub's link
supervisors (`phux server --hub` dials each enabled satellite as an ordinary
remote consumer per ADR-0038, with reconnect and capped exponential backoff).
`phux-dial` stops at the established byte stream; framing stays with its
callers on each end. The SSH-stdio path does not go through `phux-dial` —
its establishment is a child-process spawn and its trust stack is SSH's,
not rustls — but it feeds the same link supervisors, backoff, and
per-satellite status reporting on the hub.

**Redialing is per-lane.** The consumer-side reconnect window
(`phux::commands::attach`) picks its deadline and retry cadence from the
`Dial` variant, because a dropped link means different things on the two
kinds of lane. On UDS the server *process* went away and the ADR-0032
re-exec brings it back in under a second, so the client polls flat every
100ms for 10s. On `--ws` / `--quic` the usual cause is the client's own
network changing — and each probe is a real TLS handshake — so those lanes
wait 60s with exponential backoff from 500ms to 8s.

## The hub relays frames over its links

While a satellite link is up, the hub routes frames over it
(`phux-server::hub::relay`, ADR-0007 §4): a frame targeting
`ResourceId::Satellite { host, id }` is rewritten to the satellite's
`Local { id }` space and forwarded verbatim — the hub never re-encodes VT
bytes — and return-leg responses and subscribed streams are re-tagged
`Local -> Satellite { host, id }` before reaching the consumer. Each link
owns a bounded relay mailbox (producers `try_send` and fail fast), its own
link-side `COMMAND.request_id` remap, and a proxy-subscription registry;
return-leg fan-out `try_send`s into each consumer's bounded outbound mailbox
so one slow consumer never stalls the link. While the link is down (dialing,
backoff, ADR-0038 fail-closed refusal) the supervisor drains the mailbox and
fails every request with the typed `SatelliteUnreachable` error; a satellite
disconnect fails in-flight commands the same way and pushes one typed error
to every proxy-subscribed consumer before the registry clears. A satellite
that dies *silently* is bounded too: each relayed command carries a hub-side
deadline resolving to the same typed error, and every link enforces a
keepalive / idle contract — QUIC via the transport (`phux-dial` sets
`keep_alive_interval` / `max_idle_timeout`), WebSocket via hub-originated
pings plus an inbound-idle limit in `phux-server::hub::link`, SSH-stdio via
the SSH layer's `ServerAliveInterval` / `ServerAliveCountMax` on the dial
argv — so a partition without FIN/RST is torn down like an ordinary
disconnect. Normative routing semantics: `docs/spec/L1.md` §9.1.

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Roaming-aware client that uses QUIC connection migration | The stack supports migration and 0-RTT; the attach client does not yet drive them. | [ADR-0007](../adr/0007-mosh-class-transport-and-satellites.md) | not scheduled |
