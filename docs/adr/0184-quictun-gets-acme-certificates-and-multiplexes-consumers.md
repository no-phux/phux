---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0184 — quictun gets ACME certificates and multiplexes consumers

**TL;DR.** `quictun expose --acme-domain` obtains and renews a public
certificate itself. It uses TLS-ALPN-01, validated through the relay's TCP
ingress, so the relay changes nothing and the key never leaves the service.
`Protocol::consumer_streams` lets one consumer connection carry many
streams, each bridged onto its own tunnel stream, and `quictun connect`
carries every local connection on one QUIC connection. phux's relay keeps
one stream per consumer.

Status: Accepted
Date: 2026-10-17

## Context

[ADR-0183](./0183-quictun-is-a-standalone-tunnel-product.md) left two gaps,
which the owner asked to close (2026-10-17):

- **Certificates.** Browsers reaching a route through the TCP ingress need
  a certificate they trust, and the operator had to bring one.
- **One connection per consumer.** Every `connect` forward dialed its own
  QUIC connection. That costs a handshake each time and loses QUIC's main
  advantage, independent streams, because ADR-0051 kept relay routes
  single-stream. The reason was that a service treating each tunnel
  stream as its own authenticated consumer (phux) must not see a second
  stream from one connection.

## Decision

1. **ACME by TLS-ALPN-01, answered by the service.** The client is a
   small RFC 8555 client in `quictun::acme`:
   - It signs JWS requests with ES256 under a P-256 account key, kept
     owner-only in `<state>/acme/`.
   - It validates by RFC 8737's TLS-ALPN-01.
   - It finalizes with a CSR for a fresh key.
   - It runs one HTTP/1.1 request per TLS connection.

   The service's certificate resolver (`AcmeCerts`) serves a challenge
   certificate to a hello offering `acme-tls/1` and the current
   certificate to everyone else. A validation connection ends at its
   handshake and is never forwarded. The CA reaches the service the way
   any TCP consumer does: through the relay's TCP ingress, routed by SNI.
   The relay must therefore listen on TCP 443. `maintain` loads the stored
   certificate, renews it 30 days before expiry, checks every 12 hours,
   and backs off from 1 minute to 6 hours on failure.
2. **No HTTP-01 or DNS-01.** HTTP-01 would need the relay to route plain
   HTTP by `Host`, a second ingress. DNS-01 needs DNS-provider APIs.
   TLS-ALPN-01 needs nothing new. One consequence is no wildcard
   certificates, which only DNS-01 can obtain. Each `expose` gets one for
   its own name.
3. **`connect --service-name` verifies by name.** `ServiceTrust::Name`
   verifies the service against Mozilla's roots plus any `--service-ca`.
   Renewal changes the leaf, which would break `ServiceTrust::Pin`.
4. **Multiplexing is the protocol's choice.** `Protocol::consumer_streams`
   is `1` for `Protocol::new`, so phux's relay keeps bridging only the
   first stream and refusing the rest. `Protocol::quictun()` uses 256. In
   that mode every stream a consumer opens gets its own tunnel stream and
   its own route unit (ADR-0183 Decision 6), up to that many at once:
   - The relay enforces the limit through quinn's
     `max_concurrent_bidi_streams` on consumer connections.
   - A refused stream is reset with `OVER_CAP` or `ROUTE_OFFLINE`, and the
     connection stays up.
   - A tunnel accepts up to 4096 concurrent streams from the relay.
5. **`connect` multiplexes.** `Route::multiplex` carries every local
   connection as a stream on one cached connection. It redials once when
   that connection dies, and forwards that find it dead together share one
   redial. A finished stream waits for its data to be acknowledged but
   leaves the connection open.

## Why

- With TLS-ALPN-01 the relay's routing is the validation path, so ACME
  needs no new relay surface and no new trust. The relay still holds no
  key, and the CA talks to the service end to end, as a browser would.
- The client uses only crates already in the tree (`ring`, `rcgen`,
  `x509-parser`, `serde_json`, `base64`, `webpki-roots`, `tokio-rustls`).
  It follows phux-server's precedent of a hand-rolled HTTP/1.1 exchange,
  and adds no crate to the graph.
- Multiplexing is what QUIC is for. Making it a protocol property keeps
  phux's single-stream invariant exactly where phux needs it.

## Tradeoffs

- **The relay must hold TCP 443** for the CA to validate. That is also
  where browsers arrive, so a browser-facing relay already does.
- **A hand-rolled ACME client.** It covers what quictun uses: one name,
  TLS-ALPN-01, no External Account Binding, no revocation, no ARI
  renewal hints. A CA that requires more (EAB) is not supported yet.
- **A multiplexed consumer is one unauthenticated peer with up to 256
  streams.** The per-route cap still counts streams, not connections, so
  the per-route accounting holds. The per-connection stream limit bounds
  one peer.

## Alternatives

- **Depend on `instant-acme` or `rustls-acme`.** Rejected for now: each
  brings an HTTP stack (hyper-rustls) or async runtime glue the tree does
  not have, for an exchange of a few small JSON requests.
- **A consumer-group envelope on the tunnel wire,** so phux could
  multiplex too. Deferred: it changes the tunnel wire and phux's
  connector, and phux already has per-Terminal streams on direct QUIC
  (ADR-0115).

## Related

- Builds on [ADR-0183](./0183-quictun-is-a-standalone-tunnel-product.md),
  [ADR-0181](./0181-the-relay-is-a-standalone-quic-tunnel-crate.md), and
  [ADR-0051](./0051-outbound-dial-out-connector-transport.md). It changes
  nothing for phux.
- Evidence in `crates/quictun/tests/acme.rs`: a mock CA verifies every JWS,
  refuses one nonce with `badNonce`, builds its URLs from `Host`, and
  validates TLS-ALPN-01 through the relay's TCP ingress. The test then
  sends eight concurrent `connect` forwards, name-verified, over one QUIC
  connection.
- The client was also run against Pebble, Let's Encrypt's test CA, using
  the real binary: relay, `expose --acme-domain`, then `curl` through the
  ingress and through `connect`. That run found the `Host` header that
  dropped its port, which the mock now checks.
