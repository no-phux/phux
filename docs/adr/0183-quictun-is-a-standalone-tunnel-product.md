---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0183 — quictun is a standalone tunnel product

**TL;DR.** `quictun` ships a binary (`relay`, `pair`, `expose`,
`connect`) that forwards TCP through a relay with TLS ending at the
service. The relay gains a TCP ingress that routes TLS clients by their
ClientHello server name, per-route consumer caps, and a protocol-chosen
half-close rule. phux's relay behaves as before, except that a finished
bridge now lets the consumer close first.

Status: Accepted
Date: 2026-10-17

## Context

[ADR-0181](./0181-the-relay-is-a-standalone-quic-tunnel-crate.md) made the
relay a crate with no phux dependency. The owner then asked (2026-10-17)
for the follow-ups that make it a real tunnel, in the class of
[opentunnel](https://github.com/anomalyco/opentunnel): a binary, a way in
for clients that cannot speak QUIC, TCP's half-close, and room for more
than one tenant. [ADR-0057](./0057-minimal-reference-relay.md) Decision 1
fenced the reference relay to one tenant, two verbs, and a connection
cap. That fence was phux's, written for phux's relay. This ADR moves it
for `quictun` and leaves it in place for `phux relay`.

## Decision

1. **A binary.** `quictun relay`, `pair`, `expose`, and `connect`, built
   with the default `cli` feature and speaking `Protocol::quictun()`:
   `quictun-tunnel/1`, `quictun/1`, the tag `qtnT`, and TCP half-close.
   State lives in `$XDG_STATE_HOME/quictun`, or under `--state`. Arguments
   are parsed with std alone, so the binary adds no dependency.
2. **TLS ends at the service.** `expose` terminates TLS on every bridged
   stream. It uses a self-signed certificate kept in its state directory
   and prints that certificate's pin, or it uses a certificate the
   operator supplies (`--cert`/`--key`). `connect` pins the service
   certificate. Even a plaintext TCP service therefore crosses the relay
   as ciphertext. `--passthrough` on both ends forwards the stream raw,
   for a target that runs TLS itself.
3. **TCP ingress.** With `tcp_ingress`, the relay listens on TCP. It
   reads a client's TLS ClientHello far enough to learn the server name,
   maps `R.<domain>` (or a bare `R` without a domain) to route `R`, and
   forwards the hello and everything after it unchanged over a fresh
   stream on the route's tunnel. The relay holds no key here either. A
   browser reaches an exposed service when that service presents a
   certificate the browser trusts for `R.<domain>`. A hello that is not
   TLS, names no server, or names no enrolled route with a live tunnel is
   dropped. While a TCP consumer reads its hello it holds only a handshake
   permit and its source's share, as a QUIC handshake does. It takes a
   connection slot only once the hello names a live route, so peers that
   connect and stall cannot lock out tunnels. The hello must arrive within
   the consumer stream deadline (5 s, 16 KiB at most). Bridged sockets use
   TCP keepalive, so a vanished peer frees its slot. A finished bridge
   reads leftover input before it closes, so the close is a FIN and not a
   reset that could cut off the reply.
4. **Half-close is the protocol's choice.** `Protocol::half_close` is
   either `EndsBridge`, the existing rule where either direction's end
   ends the bridge, or `Propagate`, which forwards each FIN and keeps the
   other direction flowing until it ends too. `Protocol::new` defaults to
   `EndsBridge`, so `phux-relay` is unchanged. `Protocol::quictun()` uses
   `Propagate`.
5. **A finished bridge lingers for the consumer's close.** Dropping a
   QUIC connection discards stream data quinn has not delivered yet. A
   finished bridge therefore waits up to 2 s for the consumer to close
   before dropping it, and a forwarder waits for its last bytes to be
   acknowledged. This fixes a latent truncation of a reply's tail, which
   phux's relay could hit too.
6. **Tenancy is per route; accounts are not the relay's.**
   `max_conns_per_route` caps how many consumers one route may have
   bridged at once (the QUIC and TCP legs together), so one tenant cannot
   take every slot. A unit is taken only once the consumer opens its
   stream, so a handshake-only consumer never holds one. A refused QUIC
   consumer is closed with `OVER_CAP`. Consumers are unauthenticated at
   the relay, so anyone who knows a route's name can fill its cap: the cap
   protects the other routes, not the route itself.
   Accounts, billing, and who may mint which route stay outside the
   relay. As [ADR-0180](./0180-the-relay-is-blind.md) Decision 4 already
   set for phux, a hosted deployment's backend writes the route-token
   store, and the relay re-reads it per handshake.

## Why

- Every piece reuses the relay that already exists. TCP ingress is the
  QUIC consumer leg with a different first read, `forward` is two copy
  loops around the client legs, and the cap is the per-source counter
  keyed by route.
- Terminating TLS in `expose` by default gives raw TCP services the same
  property ADR-0180 gave phux: the relay operator holds ciphertext.
- Keeping accounts out of the relay keeps its admission decisions down to
  a token and a server name, which are the ones it can make without
  seeing content.

## Tradeoffs

- **The ClientHello is parsed in the clear on the TCP leg.** The relay
  reads one more structure there, through rustls' `Acceptor`, but still
  decrypts nothing.
- **TCP ingress needs a public certificate at the service** for browsers.
  quictun does not obtain one (no ACME). The operator brings it.
- **One QUIC connection per forwarded TCP connection** in `connect`.
  Relay routes stay single-stream, as ADR-0051 set, so a connection
  cannot multiplex several consumers.
- **The linger holds a slot up to 2 s longer** after a bridge whose
  consumer does not close promptly.

## Alternatives

- **ACME in the relay or in `expose`.** Deferred: it is a large surface
  (challenges, renewal, storage). Operator-supplied certificates cover
  the case now.
- **Multiplexing consumers on one QUIC connection.** Rejected for now: it
  needs a consumer-group envelope on the tunnel wire, which ADR-0051 kept
  out.
- **Making `Propagate` the default.** Rejected: it would change phux's
  relay, where a finished side should free the bridge.

## Related

- Supersedes in part [ADR-0057](./0057-minimal-reference-relay.md)
  (Decision 1, the scope fence, for `quictun` only). Builds on
  [ADR-0180](./0180-the-relay-is-blind.md) and
  [ADR-0181](./0181-the-relay-is-a-standalone-quic-tunnel-crate.md).
- Evidence is in `crates/quictun/tests/relay.rs`:
  - TCP ingress end to end, plus refused names;
  - the per-route cap and the unit returned when a consumer leaves;
  - a reply read after the consumer's FIN.

  `crates/quictun/tests/cli.rs` runs the real binary: `pair`, `relay`,
  `expose`, and `connect`, with a TCP client that half-closes before it
  reads.
