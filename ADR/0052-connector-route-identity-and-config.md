---
audience: contributors
stability: stable
last-reviewed: 2026-07-25
---

# 0052 — Connector route identity, registration, and config surface

**TL;DR.** Consumers name a tunneled server by TLS SNI at the relay; the
relay routes on it and stays byte-opaque above TLS. Route names bind to
tunnel tokens at relay enrollment, so connector stream 0 still carries only
its auth preamble. Config is a `[[connector]]` array of relay endpoints;
the connector lands as a hub-link sibling whose bridged streams enter the
standard consumer dispatch with real identity and token verification.

Status: Accepted
Date: 2026-07-21

## Context

[ADR-0051](./0051-outbound-dial-out-connector-transport.md) fixed the
tunnel shape and proved it with a spike, but named three questions that
must be settled before the implementation epic (phux-8lyr): how a consumer
names which tunneled server it wants (open question 1), where the production
connector lands and how bridged streams become consumers (open question 4),
and the config surface (open question 5). This ADR settled those three and
the connector implementation landed on 2026-07-25. It deliberately does not
touch 0051's other deferrals: client-address attestation, head-of-line
coupling, and end-to-end encryption through the relay stay deferred.
Shipping the self-hosted reference relay was decided separately by
[ADR-0057](./0057-minimal-reference-relay.md).

Two facts shape the route-identity answer. First, the relay already
terminates both TLS legs (0051 Decision 6), so the consumer's ClientHello
— including its server name — is visible to the relay before any phux
byte exists. Second, consumer-side SNI is already fully plumbed: hostname
dials send the name automatically, and `phux attach --tls-server-name`
covers IP-literal dials. Certificate pinning is fingerprint-based, never
name-based, so the server name is a pure routing label with no trust
coupling.

## Decision

1. **Route identity rides TLS SNI.** A consumer names the tunneled server
   it wants via the TLS server name it offers the relay. The relay routes
   on SNI during its own handshake and never inspects anything above TLS
   — 0051's opacity invariant is preserved exactly. Route names are
   opaque labels; DNS-shaped names are recommended (they compose with
   overlay MagicDNS habits and with pointing real DNS at the relay), but
   the relay treats them as bytes. An unknown or absent SNI is refused at
   the TLS layer: the relay mints no frames, so there is no phux error
   surface at the relay, by design.
2. **Route binding happens at enrollment, not registration.** The relay
   learns which route name a tunnel serves when the tunnel token is
   minted (relay-side, out-of-band), binding token to name one-to-one.
   Connector-initiated stream 0 therefore stays byte-for-byte what 0051
   reserved: the auth preamble, then silence. The spike's deadlock
   finding is normative — registration must transmit bytes, and the
   preamble is those bytes. Multi-route connectors and any richer
   control protocol arrive, if ever, under a bumped ALPN; enrollment
   binding is forward-compatible with both.
3. **Config is a `[[connector]]` array of tables.** Each entry: `relay`
   (`HOST:PORT`), `cert-fingerprint` (the relay's, required for any
   non-loopback relay — fail-closed, the ADR-0038 posture), `token-file`
   (0o600, never inline). An array from day one because a second relay
   must be additive, not a retrofit (ADR-0007's lesson); each entry is
   its own independently supervised link. `phux server --connect
   HOST:PORT` selects an exact configured entry and its credentials, or
   accepts an ad-hoc endpoint only on loopback. Routable ad-hoc entries
   refuse startup because the CLI has no inline secret surface.
4. **The connector lands beside the hub link, and bridged streams enter
   the standard dispatch.** A `connector` module in `phux-server`,
   sibling of `hub/link.rs`, reusing its supervision discipline (backoff
   500 ms/30 s, fail-closed planning, token-file re-read per redial).
   Each relay-initiated bidi stream passes through a reverse-accept
   adapter implementing the same seam the QUIC listener feeds, so a
   bridged consumer is an ordinary consumer: bearer preamble verified
   against the server's `TokenStore` in the normal path, real
   `PeerIdentity` with `transport: Quic` and `source_addr` honestly the
   relay's address. This retires the spike's UDS-uid tolerance; nothing
   downstream of the accept seam can tell bridged from direct.

## Why

- SNI is the only routing signal that costs zero wire change on either
  phux leg and zero parsing at the relay above TLS. Every alternative
  either adds a phux-layer field (wire change) or makes the relay read
  phux bytes (opacity break).
- Enrollment binding keeps stream 0 inert, which is exactly what makes
  future control use — and E2E channels — additive under an ALPN bump.
- The reverse-accept seam is what converts "the spike works" into "the
  feature exists": consumer auth, identity, and dispatch are the already
  -tested production paths, not connector-special copies.
- Fail-closed pin requirements mirror the posture every other TLS leg in
  the tree already has; a relay is not a trust exception.

## Tradeoffs

- SNI is cleartext on the consumer-to-relay wire; route names leak to a
  path observer. Acceptable: names are labels, not secrets, and the same
  observer sees the relay's address anyway.
- One route name per tunnel token constrains a single connector process
  to one advertised identity per relay in v1. Multi-route needs the ALPN
  bump; we accept the wait.
- Enrollment is relay-side and out-of-band, so bringing up a route has a
  manual step OSS phux cannot smooth by itself. The decision bead
  phux-b1ma owns whether an in-tree reference relay changes that.
- `--connect` without config-file support for pin and token would be
  unusable off-loopback; the flag therefore reads those from the config
  entry or refuses, which is mildly surprising for a "quick" flag but
  strictly safer.

## Alternatives

- **Route field in a phux-layer preamble on the consumer stream** —
  rejected: a consumer wire change, and the relay must parse phux bytes
  to route, breaking 0051 invariant 1.
- **Port-per-server on the relay** — rejected: pushes routing into
  deployment config, exhausts ports, and breaks single-endpoint
  ergonomics; SNI gives the same result on one port.
- **Connector announces routes via stream-0 control messages** —
  rejected for v1: requires designing a versioned control protocol now;
  enrollment binding reaches the same v1 capability and stays
  forward-compatible with exactly that protocol later.
- **Singular `[connector]` table** — rejected: the second relay becomes
  a config-format break instead of one more array entry.
- **A new consumer flag for the route** — rejected: `--tls-server-name`
  already exists and already reaches the relay; a second knob for the
  same bytes is surface without capability.

## Must-not-preclude invariants for the implementation

(1) Nothing downstream of the accept seam distinguishes bridged from
direct consumers except `source_addr`; (2) the relay never parses above
TLS — SNI routing included; (3) stream 0 carries the preamble and then
nothing, enforced connector-side; (4) every 0051 invariant continues to
bind; (5) config parsing rejects a non-loopback relay entry lacking a
fingerprint at load time, not dial time.

## Related

- ADR-0051 — tunnel shape, auth model, spike (parent decision).
- ADR-0057 — shipped self-hosted reference relay.
- ADR-0007, ADR-0031, ADR-0037, ADR-0038 — inherited invariants and
  auth posture, linked from 0051.
- Beads: phux-8lyr (epic), phux-tmmb (this ADR), phux-zwuz (ALPN and
  dialer), phux-qf2w (connector module), phux-b1ma (reference relay
  decision).
