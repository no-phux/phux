---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-13
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
- **WebSocket** — carries the same frames to browser consumers. `phux-web`
  ([the web consumer](../consumers/web.md), per ADR-0025) speaks the exact
  framing over WebSocket and projects engine state locally; one binary
  message carries exactly one encoded frame, and a message whose size
  disagrees with its declared length is a framing violation. Native attach
  can also use this lane with `phux attach --ws`, the TCP fallback when
  UDP/QUIC is blocked. **Keepalive / idle:** TCP has no transport-level idle
  detection, so this lane carries the contract itself — the native consumer
  originates an RFC 6455 ping after `WS_PING_INTERVAL` (10s) of silence and
  treats `WS_LIVENESS_TIMEOUT` (30s) with nothing inbound as a disconnect
  (`phux_dial::ws::WsKeepalive`), the same interval/timeout pair the QUIC
  lane gets from quinn and the hub link applies to its own WS satellites.
  Client-originated because every RFC 6455 peer must answer a ping, so it
  needs nothing of the server.
- **QUIC** (via `quinn`, ADR-0007) — for remote clients. When both peers
  negotiate `QUIC_STREAMS` (advertised on QUIC only, [ADR-0115](../adr/0115-quic-stream-per-terminal.md)),
  the connection is one **control** stream plus one client-opened bidi
  stream per attached Terminal. Control carries HELLO, COMMAND, attach,
  lifecycle, keepalive, and the bearer preamble where required. The
  client opens every Terminal stream. **Routing:** the
  client writes `STREAM_BIND` as the first bytes of each Terminal stream;
  the server never opens one. That stream then carries only that
  Terminal's `RESOURCE_OUTPUT`, `BOOTSTRAP_*`, `HISTORY_*`, `FRAME_ACK`,
  and `INPUT_*`. A relay splices each consumer-opened stream onto a
  fresh tunnel stream without parsing frames (`docs/spec/proto.md`
  §4.1–§4.2). Hub satellite links still speak one frame stream — the
  hub never opens `STREAM_BIND`. **Ordering:** frames on one QUIC stream
  stay ordered (a keystroke stays ahead of its echo); frames on different
  streams have no order and cannot head-of-line block each other. There
  is no connection-wide frame order. Without the bit — and on UDS,
  WebSocket, WebTransport, and SSH-stdio — the single-stream shape is
  the whole contract. Normative mapping: `docs/spec/proto.md` §4.2 and
  `docs/spec/L1.md` §4.9. TLS 1.3 is
  intrinsic; a routable listener authenticates each attachment
  with a bearer-token preamble (ADR-0031 parity with the `wss://` path),
  reusing the same persisted self-signed cert and token store. Opt-in via
  `phux server --quic <HOST:PORT>`; connection migration and 0-RTT
  resumption are inherent to the stack, with a roaming-aware client the
  follow-up. **Backpressure:** every QUIC writer whose output can outrun its
  path — the server's QUIC and WebTransport writers, and `phux-relay`'s
  consumer-facing leg — holds quinn's send window to the congestion window
  plus 16 KiB of unsent slack (TCP's `NOTSENT_LOWAT` rule) rather than
  quinn's 10 MB default, re-reading the window before every partial write.
  That policy lives once, in `phux_dial::window` (`SendWindow`, one per
  connection and shared by every stream on it, and the `TrackedSend`
  writer). A link slower than a pane's output therefore blocks the writer
  within about a round trip. The stall backs up into the attach pump, which
  measures lag in time rather than in broadcast slots: a live chunk older
  than 250ms when the pump dequeues it (`runtime::pump::STALE_OUTPUT_BUDGET`,
  measured from the later of the PTY read and the current generation's
  publication, so a chunk that merely waited behind a draining bootstrap is
  not counted as late) fences the generation and requests an in-band resync
  to a fresh checkpoint — the same path a dropped broadcast window takes. A
  slow remote consumer skips frames instead of queueing seconds of output in
  front of its own keystroke echoes. That resync is addressed to the one
  pump that fell behind (`ResyncAudience::Only`, keyed by client and
  stream): every other consumer of the pane skips it and keeps its
  generation, so a slow remote attach never re-bootstraps the local TUI, a
  recorder, or a cockpit beside it. Only a reflow is still broadcast to
  every consumer. **Through a relay**, the same stall has to cross the
  relay: when the relay-to-consumer hop is the slow one, the relay's splice
  blocks on its tracked consumer send and stops reading its tunnel stream,
  and because each tunnel stream gets only 64 KiB of receive credit
  (`TUNNEL_STREAM_RECEIVE_WINDOW`, against quinn's 1.25 MB default) the
  server's writer blocks behind it and the pump goes stale just as it would
  on a direct link. Only tunnels are bounded. quinn fixes a connection's
  per-stream window when it is accepted, before the ALPN that tells a tunnel
  from a consumer is known, so the connector's dialer tags its tunnel's
  initial destination connection ID (`phux_dial::quic::TUNNEL_CID_PREFIX`:
  `phxT` followed by 16 random bytes) and the relay, reading that ID off the
  first packet, accepts a tagged connection with the bounded config and
  everything else with quinn's defaults, so a large paste still crosses in
  about a round trip. The tag selects a config, never a role: admission
  stays with the ALPN, and a consumer that copies the tag only shrinks its
  own window. The bound is per stream, so a consumer that stops reading
  holds only its own window and never freezes another consumer on the same
  route. A tunnel from a connector that predates the tag gets quinn's
  defaults, unbounded as before the bound existed, and the relay logs it
  when admitted. Lag through a relay is
  therefore bounded, but a relayed consumer still sits behind up to that
  window more backlog than a direct one (on a 300 kbit/s consumer, roughly
  3 s of on-screen lag against 1.5 s direct). **Throughput ceiling:** each
  bridged consumer moves at most 64 KiB per round trip on the
  server-to-relay hop — about 5.2 Mbit/s at a 100 ms RTT and 1.75 Mbit/s at
  300 ms, less in practice — and output faster than that makes the server
  resync even a healthy consumer. Of 16, 32 and 64 KiB, only 64 KiB carried
  a ~3.5 Mbit/s flood over a 50 ms server-to-relay hop without resyncs.
  With a healthy consumer and a 100 ms server-to-relay RTT, floods of ~0.7
  and ~2.8 Mbit/s ran with no resyncs and near quinn's defaults (p50 71 and
  86 ms against 71 and 72 ms); at 300 ms the faster flood hit the ceiling
  (about 1.4 Mbit/s delivered, one resync a second, p50 lag 350 ms against
  212 ms).
- **WebTransport** (via `wtransport`) — QUIC-class transport for browsers,
  which cannot open raw QUIC connections. An HTTP/3 `CONNECT` session whose
  single bidirectional stream carries the identical length-prefixed frames;
  the HTTP/3 layer is a transport detail below the frame seam. Always TLS
  1.3; a routable listener requires the same `phux pair` bearer token as the
  `wss://` path (ADR-0031), carried in the `CONNECT` request —
  `Authorization: Bearer <hex>` from native consumers, or `?token=<hex>` on
  the session URL from browsers (the JS `WebTransport` API cannot set request
  headers) — and refused with HTTP 403 before the session exists. Shares the
  persisted certificate and token store with the WebSocket and QUIC
  listeners; binds its own UDP socket because browsers offer only the `h3`
  ALPN while the raw-QUIC endpoint advertises the phux-private one. Opt-in
  via `phux server --webtransport <HOST:PORT>` or `PHUX_WT_ADDR`; `phux-web`
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

Every transport ADR-0007 designed exists. See ADR-0007 for the
forward-compat constraints that still govern them (URI-shaped session IDs,
hub-and-spoke satellite topology, per-pane encoder isolation).

## Status

All five byte streams exist: UDS, WebSocket, QUIC, WebTransport, and
SSH-stdio. Relay and WebTransport writers share the QUIC send-window cap.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Roaming-aware client that uses QUIC connection migration | The stack supports migration and 0-RTT; the attach client does not yet drive them. | [ADR-0007](../adr/0007-mosh-class-transport-and-satellites.md) | not scheduled |
