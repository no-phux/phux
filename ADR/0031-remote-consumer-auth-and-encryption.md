---
audience: contributors
stability: stable
last-reviewed: 2026-08-02
---

# 0031 — Remote-consumer authentication and encryption (no SSH tunnel)

**TL;DR.** A remote consumer (the native mobile app) needs to reach a single
phux server over a network without an SSH tunnel and without the full
federation hub. Adopt **TLS (rustls) over the existing WebSocket transport,
authenticated by a pairing-issued bearer token** carried in HELLO. No homegrown
crypto, no new wire frames: auth and encryption stay a transport concern. Mutual
TLS and SSH-envelope reuse are the rejected alternatives.

Status: Accepted
Date: 2026-06-09

## Context

phux speaks one wire over several transports behind the `transport.rs` frame
seam (`Incoming`/`FrameReader`/`FrameWriter`). UDS is the local default; trust
is the OS user, kernel-enforced by socket permissions
([operations.md](../docs/operations.md) "Local trust model"). The WebSocket
transport (`WsListener`: TCP + RFC 6455, one binary message per frame,
`PHUX_WS_ADDR`) exists for the local browser client but is **plaintext and
unauthenticated** — it stamps `PeerIdentity { uid: 0, … }` on every connection.
Safe on loopback; unsafe to expose to a phone over a network.

The only secure remote path today is SSH-tunnelled `phux server --stdio`
(operations.md "Federation trust model"): SSH supplies auth and encryption,
phux delegates entirely. A mobile app cannot reasonably carry an SSH-client UX
(key management, host-key TOFU, agent forwarding).

The documented long-arc remote path is QUIC + mutual TLS for v0.2+ federation
(ADR-0007), satellite-framed and larger than this need. This ADR closes the
**nearer-term** question the mobile driver forces: how does one remote consumer
attach to one server, authenticated and encrypted, without SSH and without
waiting for federation? It must not preclude the QUIC story — and may fold into
it if QUIC lands first.

Constraint (CONTRIBUTING.md): **no homegrown crypto.** Lean on a vetted TLS
stack and a token; do not invent a handshake.

## Decision

**Wrap the existing WebSocket transport in server-side TLS (`tokio-rustls`),
and authenticate the peer with an opaque bearer token established out-of-band by
a pairing step.** Concretely:

- **Encryption: TLS 1.3 via rustls** terminated at `WsListener` before the
  RFC 6455 upgrade (`wss://`). The per-frame binary-message codec is unchanged
  underneath. This is a new dependency (see Tradeoffs).
- **The bind address is the toggle — zero remote-mode setup.** A loopback
  `PHUX_WS_ADDR` stays plaintext (the dev path); a routable address is treated
  as exposing the server, so phux auto-provisions a persisted self-signed
  certificate (rcgen) and reads the default token store. No openssl, no manual
  PEM. An operator-supplied cert overrides the generated one; `PHUX_WS_SECURE=1`
  forces the secure path on loopback for local testing.
- **Authentication: a pairing token.** A `phux pair` control verb mints a
  high-entropy token (32 bytes from the OS CSPRNG), shown once as a QR / short
  code **together with the server's certificate fingerprint** so the pin is
  authenticated out-of-band, not blind-TOFU'd. The scannable form (phux-a9s)
  is a one-tap deep-link,
  `phux://connect?url=<ws(s)-url>[&name=<n>][&fp=<sha256>]&token=<hex>`,
  printed as text when the server address is known (`--host`, or a detected
  overlay address plus the `PHUX_WS_ADDR` port) and rendered as a Unicode
  half-block terminal QR by `phux pair --qr`. A remote consumer parsing the
  link must accept this exact shape (`url` mandatory, `name`/`fp` optional).
  The consumer presents the token
  in the **WebSocket upgrade request** (`Authorization: Bearer <token>`), where
  TLS already protects it; the server compares it in constant time and **rejects
  the handshake** (HTTP 401) before any phux frame is read. Verified at every
  connection attempt against the set read at listener start, so removing a
  token's line takes effect on restart (hot-reload is future work). Tokens are
  per-device and may carry an expiry (`Capability.expires_at`).
- **Identity upgrade.** A WebSocket peer that passes TLS + token is no longer
  the anonymous `uid: 0` stamp: its per-device record maps to a `ConsumerId`
  (used in audit + capability scoping), while `PeerIdentity` carries
  `transport: WebSocket` + the already-populated `source_addr` and a
  token-attestation marker (`mcp_host_key` is the existing attestation slot).
- **No wire-spec change.** The token rides the WebSocket handshake and TLS sits
  below the frame seam, so the phux frame catalog is untouched; this is
  transport + handshake policy, not protocol.
- **No silent downgrade.** Plaintext is reachable only on loopback; a routable
  bind always takes the TLS+token path. There is no configuration in which
  remote traffic crosses the wire in clear.

## Why

- **Smallest trust-boundary move that is actually safe.** TLS 1.3 gives
  confidentiality, integrity, and forward secrecy from a vetted stack; the
  bearer token gives authentication and revocation. Together they replace the
  SSH envelope for the one-server-one-phone case without an SSH-client UX. A
  passive observer cannot replay the token because it never crosses the wire in
  clear.
- **It reuses the seam we already have.** The frame codec, dispatch loop, and
  `PeerIdentity` plumbing are transport-agnostic by design (ADR-0007 invariant
  1). The change lives almost entirely in `transport.rs`; no domain code learns
  about it.
- **Pairing matches the device.** A QR/short-code flow is the idiom a phone
  expects; SSH key distribution is not. The token is device-bound and
  independently revocable, which SSH-tunnel reuse cannot offer per-consumer.
- **Forward-compatible with QUIC.** TLS identity + token authorization is the
  same conceptual model mutual-TLS QUIC will use; the pairing/token store and
  the `PeerIdentity` upgrade carry over. If QUIC (phux-84yt) lands first, this
  becomes the `wss` fallback, not wasted work.

## Tradeoffs

- **New crypto dependencies.** Neither rustls nor quinn is in the tree today,
  and `tokio-tungstenite` is pinned with TLS features off. This adds
  `rustls`/`tokio-rustls` (TLS), `rustls-pemfile`, `subtle` (constant-time
  compare), `getrandom`, `sha2`/`hex` (fingerprint), and `rcgen` (auto
  self-signed cert) — all `ring`-backed to stay cmake-free. Justified because
  they are the vetted no-homegrown-crypto substrate, and rustls is the same
  stack ADR-0007's QUIC will need. Owned as a dep-justification in the PR.
- **Bearer token = the secret is the credential.** Anyone holding the token is
  the device until revoked — weaker than a private key that never leaves the
  device (mutual TLS). Mitigations: high entropy, TLS-only transport, one-time
  display, per-device revocation, optional expiry. Accepted for v0.1-remote in
  exchange for a pairing UX a phone can drive. Client-side storage is the OS
  keychain/secure enclave; server-side token records are `0o600`, like log sinks.
- **First-pair MITM window.** With a self-signed cert and trust-on-first-use, an
  active MITM at the *first* connection can present its own cert and capture the
  token. Mitigation: the QR shows the cert fingerprint alongside the token, so
  the pin is verified out-of-band on first contact rather than blindly accepted.
  The cert is auto-generated and persisted (so the fingerprint is stable across
  restarts once pinned); an operator may substitute a CA-issued cert.
- **Trust boundary widens past the OS user.** UDS trust is "same UID, kernel
  enforced." This admits a network peer whose proof is a token over TLS — a
  larger attack surface. It engages only on a routable bind, so the loopback
  default posture is unchanged.

## Alternatives

**Mutual TLS (client certificates).** Each device gets a key/cert; the server
verifies the client cert at the TLS layer; no application token. Strongest — the
credential is a private key that never traverses the wire, and it is exactly
where QUIC federation is going. Rejected as the *first* step because
provisioning a client cert onto a phone is a heavier pairing UX than a token,
and the token approach upgrades to mTLS later without a wire change. Recommended
as the v0.2 hardening once QUIC's cert model is settled.

**Reuse the SSH envelope** (embed an SSH client in the app). Rejected: it
re-imports the entire SSH-client UX the mobile driver exists to avoid (key
management, host-key TOFU), adds a large consumer dependency, and still leaves
phux doing zero authentication of its own. It buys nothing TLS+token doesn't, at
higher consumer complexity.

**Plaintext WS + token only.** Rejected outright: token and terminal bytes
would cross the network in clear — no confidentiality, and the token is
trivially replayable. Violates the no-plaintext-remote requirement.

## OutputMode guidance for remote consumers

A remote phone link is high-latency and may be lossy, so a remote consumer
SHOULD request `OutputMode::StateSync` (ADR-0018) at HELLO rather than the
default `OutputMode::Raw`: StateSync coalesces floods and paces per-consumer
RTT, which Raw (byte-faithful, lowest latency on a fast local link) does not.
Documented for operators in operations.md.

## Related

- ADR-0007 — transport-as-trait, QUIC + mutual TLS as the v0.2+ federation path.
- ADR-0018 — lazy state synchronization; basis for the OutputMode guidance.
- [operations.md](../docs/operations.md) — the remote-consumer trust boundary.
- phux-84yt — QUIC transport (federation epic phux-klxy); the option this defers to.
