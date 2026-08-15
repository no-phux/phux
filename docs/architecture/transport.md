---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-07-10
---

# Transport abstraction

**TL;DR.** The wire codec sits behind an async `Transport` trait on both
ends, so the same framing rides any byte stream. Five implementations exist
today: a Unix-domain-socket transport for local server/client links, a
WebSocket transport that carries the identical codec to browser consumers,
a QUIC transport for remote clients, a WebTransport (HTTP/3 over QUIC)
transport that gives browsers QUIC-class transport, and an SSH-stdio
transport that splices the codec through `ssh HOST phux stdio-bridge` (the
federation hub's `ssh://` dial path). Outbound establishment for the TLS
transports is shared in `phux-dial` (attach loop and hub dialer alike). No
domain module names a concrete transport type; all I/O goes through the
trait.

---

The wire codec sits behind an `async Transport` trait on both server and
client. No domain module in `phux-server` or `phux-client` names a concrete
transport type; all I/O goes through the trait, which keeps new transports
additive rather than invasive.

## Implementations that exist today

- **`UnixSocketTransport`** — the local server/client link. This is the
  default path for a server and the clients attached to it on the same host.
- **WebSocket transport** — carries the same wire codec to browser
  consumers. `phux-web` ([the web consumer](../consumers/web.md), per
  ADR-0025) speaks the exact framing over WebSocket and projects engine
  state locally; the bytes on the wire are identical to the UDS path, only
  the byte stream underneath differs. Native attach can also use this path
  with `phux attach --ws`, making it the TCP fallback when UDP/QUIC is
  blocked. **Keepalive / idle:** TCP has no transport-level idle
  detection, so this lane carries the contract itself — the native
  consumer originates an RFC 6455 ping after `WS_PING_INTERVAL` (10s) of
  silence and treats `WS_LIVENESS_TIMEOUT` (30s) with nothing inbound as a
  disconnect (`phux_dial::ws::WsKeepalive`), the same interval/timeout pair
  the QUIC lane gets from quinn and the hub link applies to its own WS
  satellites. Client-originated because every RFC 6455 peer must answer a
  ping, so it needs nothing of the server. Without it a half-open socket —
  the laptop that changed networks — never surfaces as an error and the
  read parks forever.
- **QUIC transport** (via `quinn`, ADR-0007) — for remote clients. Each
  connection opens one bidirectional QUIC stream and frames the identical
  codec over it. TLS 1.3 is intrinsic; a routable listener authenticates
  each attachment with a bearer-token preamble (ADR-0031 parity with the
  `wss://` path), reusing the same persisted self-signed cert and token
  store. Opt-in via `phux server --quic <HOST:PORT>`; the connection
  migration and 0-RTT resumption that motivate QUIC (the Mosh-class roaming
  UX) are inherent to the stack, with a roaming-aware client the follow-up.
- **WebTransport transport** (via `wtransport`) — QUIC-class transport for
  browsers, which cannot open raw QUIC connections. An HTTP/3 `CONNECT`
  session whose single bidirectional stream carries the identical
  length-prefixed frames the UDS and QUIC paths use; the HTTP/3 layer is a
  transport detail below the frame seam, so nothing on the wire changes.
  Always TLS 1.3 (QUIC mandates it); a routable listener requires the same
  `phux pair` bearer token as the `wss://` path (ADR-0031), carried in the
  `CONNECT` request — `Authorization: Bearer <hex>` from native consumers,
  or `?token=<hex>` on the session URL from browsers (the JS `WebTransport`
  API cannot set request headers) — and refused with HTTP 403 before the
  session exists. Shares the persisted certificate and token store with the
  WebSocket and QUIC listeners; binds its own UDP socket because browsers
  offer only the `h3` ALPN while the raw-QUIC endpoint advertises the
  phux-private one. Opt-in via `phux server --webtransport <HOST:PORT>` or
  `PHUX_WT_ADDR`; `phux-web` dials it first and falls back to WebSocket.
  Feature-gated in `phux-server` as `webtransport` (on by default).
- **SSH-stdio transport** (ADR-0007, phux-v45.9) — frames the wire codec
  over a child SSH process's stdin/stdout. The dialing side spawns the
  system `ssh` binary (`$PHUX_SSH` overrides the program — an
  OpenSSH-compatible wrapper, or a stub in tests) running the remote
  `phux stdio-bridge` verb, which splices its stdin/stdout
  byte-transparently to the server's Unix socket on that host. SSH
  supplies authentication and encryption; the bridge holds an ordinary
  local UDS connection under the socket's owner-only permissions, so no
  bearer token or certificate pin is involved (ADR-0038 addendum). First
  consumer: the federation hub dialing `ssh://` satellite endpoints. The
  hub spawns ssh with `BatchMode=yes` (never an interactive prompt), a
  `--`-guarded, charset-validated argv (endpoint parts that could read as
  ssh options are rejected at hub-table validation), and treats the child
  exiting as a dropped link, feeding the same capped-backoff redial loop
  as the QUIC/WS paths. **Keepalive / idle:** liveness for ssh links lives
  at the SSH layer, not in-band — the hub dials with
  `ServerAliveInterval` / `ServerAliveCountMax` derived from the same
  interval/timeout constants the WS path uses, so a silent partition
  makes the ssh child exit and the exit is the ordinary disconnect
  signal. The bridged phux stream stays byte-transparent (no link ping
  rides it), mirroring how QUIC delegates the same contract to quinn.

All five run the same codec. A consumer that can frame the codec over a
stream is a peer regardless of which stream it uses.

## Outbound dialing is shared

The client-side establishment of the two TLS remote transports — TLS 1.3
with a fingerprint-pinned (or loopback skip-verify) certificate verifier,
plus the ADR-0031 bearer token — lives in the `phux-dial` crate, consumed
by both the `phux-client` attach loop and the federation hub's outbound
link supervisors (`phux server --hub` dials each enabled satellite as an
ordinary remote consumer per ADR-0038, with reconnect and capped
exponential backoff; see `phux-server::hub::link`). `phux-dial` stops at
the established byte stream; framing stays behind the transport trait on
each end. The SSH-stdio path does not go through `phux-dial` — its
"establishment" is a child-process spawn, and its trust stack is SSH's,
not rustls — but it feeds the same link supervisors, backoff, and
per-satellite status reporting on the hub.

**Redialing is per-lane.** The consumer-side reconnect window
(`phux::commands::attach`) picks its deadline and retry cadence from the
`Dial` variant, because a dropped link means different things on the two
kinds of lane. On UDS the server *process* went away and the ADR-0032
re-exec brings it back in under a second, so the client polls flat every
100ms for 10s. On `--ws` / `--quic` the usual cause is the client's own
network changing — and each probe is a real TLS handshake — so those
lanes wait 60s with exponential backoff from 500ms to 8s. Same shape as
the hub's capped-backoff redial, tuned for an interactive attach.

## The hub relays frames over its links

While a satellite link is up, the hub routes frames over it
(`phux-server::hub::relay`, ADR-0007 §4): a frame targeting
`TerminalId::Satellite { host, id }` is rewritten to the satellite's
`Local { id }` space and forwarded verbatim — the hub never re-encodes VT
bytes — and return-leg responses and subscribed streams are re-tagged
`Local -> Satellite { host, id }` before reaching the consumer. Each link
owns a bounded relay mailbox (producers `try_send` and fail fast), its own
link-side `COMMAND.request_id` remap, and a proxy-subscription registry;
return-leg fan-out `try_send`s into each consumer's bounded outbound
mailbox so one slow consumer never stalls the link. While the link is down
(dialing, backoff, ADR-0038 fail-closed refusal) the supervisor drains the
mailbox and fails every request with the typed `SatelliteUnreachable`
error; a satellite disconnect fails in-flight commands the same way and
pushes one typed error to every proxy-subscribed consumer before the
registry clears. A satellite that dies *silently* is bounded too: each
relayed command carries a hub-side deadline resolving to the same typed
error, and every link enforces a keepalive / idle contract — QUIC via the
transport (`phux-dial` sets `keep_alive_interval` / `max_idle_timeout`),
WebSocket via hub-originated pings plus an inbound-idle limit in
`phux-server::hub::link`, SSH-stdio via the SSH layer's
`ServerAliveInterval` / `ServerAliveCountMax` on the dial argv (a peer
that stops answering probes exits the ssh child — the drop signal) — so
a partition without FIN/RST is torn down like an ordinary disconnect
instead of pinning consumers on a link that still looks connected.
Normative routing semantics: `docs/spec/L1.md` §9.1.

With SSH-stdio built, every transport ADR-0007 designed exists. See
ADR-0007 for the forward-compat constraints that still govern them
(URI-shaped session IDs, hub-and-spoke satellite topology, per-pane
encoder isolation).
