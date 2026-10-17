---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0180 — The relay is blind: every relayed session ends at the server

**TL;DR.** A relay routes on one opaque label, the route in the outer TLS
server name, and splices ciphertext. Every relayed consumer runs TLS 1.3
with the phux server inside its stream, pinning the server's CA, so the
relay never holds a session key. The connector refuses anything else, in
every posture, and clients refuse to dial a route without that pin. Hosted
relay entitlement is a route-token decision, made without seeing content.

Status: Accepted
Date: 2026-10-17

## Context

[ADR-0051](./0051-outbound-dial-out-connector-transport.md) Decision 6 let
both TLS legs end at the relay, which then read VT output, keystrokes, and
paste. ADR-0154 item 5 added an end-to-end session inside the relayed stream
and [ADR-0179](./0179-the-legacy-auth-paths-are-removed.md) required it
under `paired`, but a connector still admitted the plain stream for an
embedder's server, and a client with no server CA pinned still dialed the
plain stream and sent its bearer through the relay. A phux-operated relay,
sold inside the Pro subscription (phux-mobile ADR-0035), must not be able
to read a session (bead phux-9ys5; owner decision 2026-10-17).

The prior art is [opentunnel](https://github.com/anomalyco/opentunnel)
(MIT): its server reads only the ClientHello's SNI, bridges bytes still
encrypted, and the client holds the only key (`docs/protocol.md`
§"Session"); it says plainly that a route name is not authentication and
that the visitor address it reports is only as trustworthy as the service
(`docs/trust-boundary.md`). phux borrows that pattern and not the code:
opentunnel is TCP over a WebSocket bridge with public ACME certificates,
and phux's relay is QUIC with its own pinned CA.

## Decision

1. **The relay routes on the route alone.** A consumer names a route in the
   outer TLS server name (ADR-0052, ADR-0149); the relay matches it to the
   tunnel the route's token claimed and splices one stream. The route is an
   opaque label minted at relay enrollment, not a secret and not
   authentication. The relay still parses nothing past its own handshakes.
2. **Every relayed session is TLS 1.3 ending at the server.** The connector
   admits a bridged stream only when its first byte opens a TLS handshake.
   The server presents its own certificate; the consumer verifies it under
   the CA it pinned at pairing (ADR-0153). Under `paired` the server
   verifies the consumer's workload certificate (ADR-0116). The bearer
   preamble and every frame ride inside. Any other first byte is refused
   before it is read, in every posture, an embedder's included. A server
   that cannot build the session runs no connector.
3. **Clients fail closed.** On QUIC, a TLS server name names a relay route.
   A dial with one is refused unless the server's CA is pinned for that
   relay leaf and route, and nothing (bearer, identity, frame) is written
   before the inner handshake verifies the server. The outer handshake
   pins the relay and presents no certificate. Relay links already carry
   `ca`. The manual forms gain `--ca-fingerprint`: `phux host add NAME
   quic://RELAY --tls-server-name ROUTE --ca-fingerprint CA` (required
   there) and the one-off `phux attach --quic RELAY --tls-server-name ROUTE
   --ca-fingerprint CA`.
4. **Entitlement is decided at the outer tunnel.** The relay's only
   admission decisions are its own: the connector's route token on stream
   0, and the consumer's outer server name matching an enrolled route. A
   hosted relay gates the Pro entitlement there. It mints a route token
   only for an account the account backend reports as holding `phux.pro`,
   and on lapse deletes the route line; the relay re-reads its store, so
   new consumers of that route are refused at TLS at once, and the tunnel
   is refused at its next claim. That check sees an account, a route, and a
   token, never content. The account backend lookup belongs to the hosted
   deployment; `phux-relay`'s interface to it is the route-token store.
5. **What the relay still sees, stated plainly.** Consumer and connector
   addresses and ports; the route; the inner ClientHello, which is
   plaintext and carries the route as its server name, the offered ALPN
   (`phux-quic/1` or `phux-enroll/1`, so an enrollment is visible), and the
   cipher suites; record sizes, direction, and timing, which is enough to
   infer typing cadence and output bursts; connection counts and
   durations; and the connector's route token. It never sees a frame, the
   bearer, the consumer's certificate (TLS 1.3 encrypts it), or the
   server's. It can deny service, and it can drop or delay records; it
   cannot forge, read, or replay one undetected.
6. **Migration.** The relay is unchanged. A server upgraded past this
   refuses plain relayed consumers. Clients paired from a relay link
   already pin the route's CA and are unaffected; an entry registered by
   hand with only the relay's pin now fails with a message naming
   `--ca-fingerprint` and `phux pair --relay-route`. A server whose
   certificate no CA issued cannot mint a relay link (ADR-0179) and must
   rotate first. phux-mobile's relayed hosts need the link's `ca`.

## Why

- End-to-end TLS inside the stream already existed for enrollment and
  `paired`; making it the only shape is a deletion, and it reuses the pins
  and certificates every client already has.
- A relay that holds no key is safe to operate for others: the operator,
  or anyone who takes the relay host, gets ciphertext and metadata, and
  holds no credential the server accepts.
- Refusing at the first byte keeps the check one comparison, and the
  client refusing before it writes means a misconfigured client never
  leaks its bearer to the relay.

## Tradeoffs

- **Two TLS layers through a relay.** Each record is encrypted twice. The
  inner session also bypasses per-Terminal QUIC streams, as relayed
  connections already did (ADR-0154).
- **Metadata stays visible.** Traffic analysis of record timing can
  suggest what is typed. Padding is not attempted here.
- **No direct SNI override on QUIC.** A server name on a QUIC entry now
  means a relay. A pinned direct dial never needed one.
- **Hand registration takes one more value.** The CA pin is now required
  where the relay pin alone used to do.

## Alternatives

- **Depend on opentunnel.** Rejected: its transport (TCP over a WebSocket
  bridge), certificate model (public ACME per tunnel), and server stack do
  not fit a QUIC relay with a private CA, and upstream takes no patches.
- **Pass the consumer's QUIC handshake through, routed on its SNI**, as
  opentunnel does for TLS. Rejected for now: the relay would decrypt QUIC
  Initial packets to read SNI and forward datagrams by connection ID,
  losing the stream discipline (ADR-0051 Decision 2) and the relay's own
  pin, for the same confidentiality the inner session already gives.
- **Keep the plain stream for embedders.** Rejected: an embedder's relay
  operator would read it, and the posture is the server's, not the relay's.
- **An application-layer channel (Noise) instead of TLS.** Rejected: a
  second key system beside the CA pins every client already holds.

## Related

- Bead phux-9ys5. Supersedes in part ADR-0051 (Decision 6) and
  [ADR-0149](./0149-relay-routes-ride-the-tls-server-name-everywhere.md)
  (Decisions 1 and 5); builds on
  [ADR-0154](./0154-devices-enroll-with-a-ticket-over-their-own-alpn.md)
  item 5 and ADR-0179.
- Evidence: `crates/phux-server/tests/federation/relay_blind.rs` taps every
  byte the production relay forwards during a relayed attach, and shows an
  impostor behind the route refused by name before the bearer is sent.
