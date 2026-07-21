---
audience: contributors
stability: stable
last-reviewed: 2026-07-21
---

# 0051 — Outbound dial-out (connector) transport mode

**TL;DR.** A server behind NAT can dial out to a self-hosted relay and hold
one persistent QUIC tunnel; the relay bridges each consumer in as one
bidirectional stream, splicing bytes opaquely. Auth reuses bearer-token plus
fingerprint pinning on both legs, and the server still verifies every
consumer's own token. This ADR fixes the tunnel shape and blesses a
test-only spike; no production code or spec bytes change yet.

Status: Accepted
Date: 2026-07-15

> **Update 2026-07-21:** accepted on merge of PR #238 per bead phux-1cfx.
> Open questions 1, 4, and 5 are settled by
> [ADR-0052](./0052-connector-route-identity-and-config.md); the
> productization epic is bead phux-8lyr.

## Context

[ADR-0037](./0037-overlay-network-reachability.md) dissolved reachability
below the wire and scoped relay and reverse tunnels out — but it marked the
outbound reverse tunnel "Deferred, not rejected," promising the ADR-0007
invariants keep a relay drop-in additive. This ADR (bead phux-1cfx) walks
through that door for the self-hosting audience: a relay anyone can run
themselves, with no overlay install on the consumer side.

Every mechanism already exists in-tree: outbound QUIC dialing with pinned
cert and token preamble (`phux-dial`), the supervised outbound-link pattern
with backoff and fail-closed planning (`hub/link.rs`,
[ADR-0038](./0038-hub-satellite-auth.md)), byte splicing (`stdio-bridge`),
and consumer auth ([ADR-0031](./0031-remote-consumer-auth-and-encryption.md)).
The missing piece is purely directional: today the server only accepts.

## Decision

1. **Topology.** A *connector* inside the server dials a *relay* and holds
   one persistent QUIC connection under hub-link supervision (backoff
   500 ms/30 s, fail-closed `plan_link`-style gate, token file re-read per
   redial). Consumers dial the relay, which bridges each admitted consumer
   through the held connection. The relay is a rendezvous, not a peer: no
   sessions, no minted frames, no acks.
2. **Stream discipline (the load-bearing shape).** The connector leg gets
   its own ALPN, **`phux-relay/1`** — reserved normatively here and now
   owned by `phux_protocol::policy::QUIC_RELAY_ALPN` (bead phux-zwuz).
   Connector-initiated stream 0 carries only the connector's auth preamble,
   reserved for future control use. **Every relay-initiated bidi stream is
   exactly one bridged consumer** — QUIC stream-ID parity disambiguates
   control from consumers with zero in-band protocol. This discipline is
   the single hardest thing to retrofit: a flat pipe would make
   multiplexing, per-consumer identity, and inner E2E channels a
   connector-leg wire break against deployed relays.
3. **What the tunnel carries.** The identical length-prefixed frame stream,
   byte-opaque at the relay past its own handshakes —
   [ADR-0007](./0007-mosh-class-transport-and-satellites.md) hub semantics,
   one layer down: the relay does not even *parse*. No new frame types, no
   `docs/spec/` change, no version bump; HELLO and frame dispatch run
   unmodified.
4. **Auth: two independent tokens, zero new primitives.**
   - *Connector to relay:* inverted ADR-0038 pairing. The relay enrolls
     (mints an opaque 32-byte token, shows its cert SHA-256 fingerprint
     once, out-of-band); the connector dials with `CertTrust::Pinned` and
     a token file (0o600, never inline or printed), fail-closed off-loopback.
   - *Consumer:* authorization is the **server-minted** ADR-0031 pairing
     token: each consumer stream begins with the consumer's bearer-token
     preamble, passed through the relay opaquely and verified against the
     server's own `TokenStore`. Relay auth never substitutes for server
     auth: a compromised relay holds no credential the server accepts.
     (Relay-side admission gating is optional defense-in-depth.)
5. **Identity.** No new variant. The relay sits below L1 exactly like
   ADR-0037's overlay — reachability, not topology; `LOCAL`/`SATELLITE`
   tagging is unaffected. A bridged consumer's `PeerIdentity` is an
   ordinary remote consumer (`transport: Quic`); `source_addr` honestly
   carries the relay's address (client-address attestation deferred).
6. **Trust honesty.** Both TLS legs terminate at the relay: **the relay
   sees plaintext frames — VT output, keystrokes, paste payloads.** A relay
   host is inside the trust boundary of everyone tunneling through it;
   self-hosting is the only mitigation today. End-to-end encryption is
   deferred; item 3's byte-opacity keeps it an additive drop-in.
7. **Scope: test-only spike, production delta zero.** One black-box test,
   `crates/phux-server/tests/relay_connector_spike.rs`: a stub relay
   (in-test quinn endpoint, pure byte splice), a connector test task
   (a hand-rolled in-test quinn client — string-literal `phux-relay/1`
   ALPN plus a faithful pinned-fingerprint verifier crib, because
   `phux_dial::quic::dial` hardcoded the consumer ALPN before phux-zwuz
   added `dial_with_alpn` — per-stream splice
   onto the server UDS, consumer preamble checked connector-side before
   splicing), and a consumer driving the full handshake. No new deps, no
   production source touched.

## Why

- Reuse over invention: this ADR adds an arrow direction, not a mechanism.
- QUIC-native stream multiplexing isolates consumers without in-band mux
  frames, keeping the relay byte-opaque and `docs/spec/` untouched.
- Two-token separation makes "anyone can run a relay" safe: the relay is a
  reachability service, not an authority.
- Byte-opacity keeps the deferred E2E door open — 0037's move, one layer up.
- The spike de-risks the one real unknown — a two-leg splice preserving
  handshake and frame semantics under existing timeouts.

## Tradeoffs

- The relay sees plaintext; mitigated only by self-hosting until E2E lands.
- A second always-on hop adds latency and a failure domain; supervisor
  backoff means reconnect, not transparent resumption.
- Server-side rate limiting and audit see the relay, not the client.
- Spike-only tolerance: bridged consumers surface with the connector's
  local UDS uid — unacceptable in production (see open questions).
- No user-visible feature ships; deliberately a design record plus proof.

## Alternatives

- **Single flat spliced tunnel** — rejected: retrofitting stream discipline
  is a connector-leg wire break (Decision 2).
- **In-band mux frames over one stream** — rejected: reinvents QUIC
  streams, forces a spec bump, gives the relay a reason to parse.
- **Relay-minted consumer tokens** — rejected: makes the relay an admission
  authority; changing later invalidates deployed credentials.
- **New `RELAY` identity variant** — rejected: conflates reachability with
  topology (ADR-0037 precedent).
- **SSH-class carve-out on the connector leg** — rejected: the relay leg is
  a TLS transport; ADR-0038's token-plus-pin fail-closed rule applies.
- **TURN/MASQUE** — deferred: heavier than needed; precludes nothing later.
- **Do nothing (overlay-only)** — rejected as permanent by 0037 itself.

## Must-not-preclude invariants for the spike

(1) The relay never parses past its own handshakes; (2) exactly one
consumer per relay-initiated bidi stream, never on stream 0; (3) consumer
authorization is server-verified — relay admission never substitutes; (4)
no domain code names the relay; connector I/O stays behind `phux-dial` and
the transport seam; (5) FRAME_ACK stays protocol-layer end-to-end — the
relay never acks; (6) no identity variant added; (7) the connector leg
never reuses `phux-quic/1` — `phux-relay/1` even in the stub.

## Related

- ADR-0007, ADR-0031, ADR-0037, ADR-0038 — all linked above where used.
- Bead phux-1cfx — this spike. Reuse surfaces: `crates/phux-dial`,
  `crates/phux-server/src/hub/link.rs`.

## Open questions

1. Route identity on the relay (how a consumer names which tunneled
   server; client-URI shape) — settle before the implementation bead.
2. Client-address attestation via stream 0 (an auth decision, not plumbing).
3. Head-of-line coupling: streams share the tunnel's congestion and idle
   state — acceptable, or connection-per-consumer on the relay leg?
4. Production landing: sibling of `hub/link.rs`; does `Incoming` gain a
   "reverse accept" impl over `accept_bi()`? Expected shape, not mandated.
5. Config surface: a `[[connector]]` table vs alongside `[satellites]`.
