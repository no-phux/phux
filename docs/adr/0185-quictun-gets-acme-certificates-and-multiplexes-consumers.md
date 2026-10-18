---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0185 — quictun gets ACME certificates and multiplexes consumers

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
   - It finalizes with a CSR for a fresh key, with no subject. Boulder
     counts a CSR's common name as one of its names, so rcgen's default
     CN would be refused.
   - It runs one HTTP/1.1 request per TLS connection.

   The service's certificate resolver (`AcmeCerts`) serves a challenge
   certificate to a hello offering `acme-tls/1` and the current
   certificate to everyone else. A validation connection ends at its
   handshake and is never forwarded. The CA reaches the service the way
   any TCP consumer does: through the relay's TCP ingress, routed by SNI.
   The relay must therefore listen on TCP 443.

   `maintain` loads the stored certificate and renews it once a third of
   its lifetime is left: 30 days for a 90-day certificate, 2 days for a
   6-day one. It checks in proportion, at most every 12 hours. Issuance
   waits until `expose` reports its tunnel admitted, meaning still open
   2 s after dialing, because validation runs through that tunnel.
   Failures back off from 5 minutes to 6 hours, which stays under Let's
   Encrypt's limit of five failed validations per hour. Polls honor
   `Retry-After`, up to 30 s.

   The key and its chain are stored as one owner-only PEM bundle, replaced
   by a single rename. A newly created account key discards any stored
   account URL.
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
   - A refused stream is reset on its own:
     - `OVER_CAP` when the route is at its cap, or when the tunnel is
       alive but did not open a stream within 5 s;
     - `ROUTE_OFFLINE` when the tunnel is gone, which also closes the
       connection.
   - Each stream gets 256 KiB of receive credit and the connection 4 MiB
     in all, bounding what one unauthenticated peer can make the relay
     buffer. quinn's defaults would allow about 1.25 MB per stream with no
     connection limit.
   - A multiplexed connection with no stream bridged for 30 s is closed, so
     an idle client gives its connection slot back.
   - A tunnel accepts up to 4096 concurrent streams from the relay.
5. **`connect` multiplexes.** `Route::multiplex` carries every local
   connection as a stream on one cached connection:
   - It redials when that connection closes, and forwards that find it
     dead together share one redial.
   - A stream whose TLS handshake fails retires the connection. That
     catches a connection that died without a close reaching the client
     (a relay restart, a network change), which quinn would otherwise
     notice only at its 30 s idle timeout.
   - A finished stream waits for its data to be acknowledged but leaves
     the connection open.
   - `--single-stream` restores one connection per local connection, for
     a relay that bridges a single stream per consumer.

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
  streams.** The per-route cap counts streams, not connections. What one
  peer can make the relay buffer is bounded by the 4 MiB connection
  window, not by the stream count.

## Alternatives

- **Depend on `instant-acme` or `rustls-acme`.** Rejected for now: each
  brings an HTTP stack (hyper-rustls) or async runtime glue the tree does
  not have, for an exchange of a few small JSON requests.
- **A consumer-group envelope on the tunnel wire,** so phux could
  multiplex too. Deferred: it changes the wire and phux's connector.

## Related

- Builds on [ADR-0183](./0183-quictun-is-a-standalone-tunnel-product.md),
  [ADR-0181](./0181-the-relay-is-a-standalone-quic-tunnel-crate.md), and
  [ADR-0051](./0051-outbound-dial-out-connector-transport.md). It changes
  nothing for phux.
- Evidence: `crates/quictun/tests/acme.rs` (a strict mock CA that
  validates TLS-ALPN-01 through the ingress, then eight `connect`
  forwards over one connection) and `tests/relay.rs` (per-stream bridging
  and the idle reaper). The binary was also run against Pebble, which
  found a `Host` header missing its port; the mock now checks that.
