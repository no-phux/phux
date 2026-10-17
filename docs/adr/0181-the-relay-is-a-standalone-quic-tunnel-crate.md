---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0181 — The relay is a standalone QUIC tunnel crate

**TL;DR.** The relay moves into `crates/quictun`, which depends on no phux
crate. Its ALPNs, connection-ID tag, and state directory are a `Protocol`
and a config a deployment supplies. `phux-relay` shrinks to phux's
binding of it. `quictun` also owns the tunnel wire, both client legs, leaf
pinning, and the cert, secret-file, and send-window modules `phux-dial`
re-exports. Nothing on the wire changes.

Status: Accepted
Date: 2026-10-17

## Context

[ADR-0057](./0057-minimal-reference-relay.md) Decision 2 put the relay in
`crates/phux-relay` and let it depend on `phux-protocol` for its two ALPNs.
It also came to depend on `phux-dial` for certificate provisioning,
secret-file checks, the send window, and the `phxT` tunnel tag. Since
[ADR-0180](./0180-the-relay-is-blind.md) the relay forwards only ciphertext
and reads nothing but its own handshakes and the tunnel preamble. Nothing
about it is phux's except those constants. The owner asked (2026-10-17)
for it to be a crate that is completely independent of phux: a tunnel in
the class of [opentunnel](https://github.com/anomalyco/opentunnel) that a
phux-operated or third-party service can run for any application, built
on QUIC.

## Decision

1. **`crates/quictun` holds the relay and depends on no phux crate.** It
   has no `phux-*` dependency, normal or dev, and names no phux ALPN,
   path, or tag. Features: `relay` (default) is the relay; `provision`
   is `cert` and `secret_file`; `testing` is `DropProxy`. With no
   features it is the tunnel wire, the client legs, pinning, and the send
   window, which is what a client build needs.
2. **A deployment is a `Protocol`.** It names the tunnel ALPN, one or more
   consumer ALPNs, and the `CidTag`. A relay refuses to bind a protocol
   whose tunnel ALPN is empty or is also a consumer ALPN, because legs are
   still decided by ALPN alone (ADR-0051 invariant 7). `RelayConfig`
   carries the protocol, and `RelayConfig::in_dir` fixes the three file
   names ADR-0057 Decision 4 set.
3. **`quictun` owns the tunnel wire.** That covers the stream-0 preamble
   (`len: u32 BE` + token, 256-byte bound), the close codes `0x01`–`0x05`,
   and the 20-byte tagged connection ID. It also has both client legs:
   `connect::Tunnel` for a service and `connect::dial_route` for a
   consumer, which pin the relay's leaf with `pin`. phux's server
   connector and dialer keep their own client code, which speaks the
   same wire.
4. **`phux-relay` is phux's binding.** It supplies `protocol()`
   (`phux-relay/1`, `phux-quic/1`, `phxT`), the paths under phux's state
   directory, and re-exports. The `phux relay` verb, the testkit
   harness, and the relay contract tests are unchanged in behavior.
5. **One home per shared module.** `cert`, `secret_file`, `window`, and
   `DropProxy` move from `phux-dial` into `quictun`. `phux-dial`
   re-exports them under the same paths, and `phux_dial::quic` takes
   `TUNNEL_CID_TAG` and `AUTH_FAILED_CODE` from `quictun`.

## Why

- The dependency graph enforces independence: `quictun` cannot import
  phux, so a phux concept cannot creep back into it.
- Making the protocol a value is all it takes to make the relay generic.
  Each constant was already used in exactly one place.
- Owning both client legs means the crate tests its own wire end to end
  (`crates/quictun/tests/relay.rs`, with a protocol that names no
  application) without phux's dialer.

## Tradeoffs

- **Two client implementations of one wire.** `phux-dial` keeps its own
  QUIC dialer, which classifies errors and runs the inner session, beside
  `quictun::connect`. The preamble and the tag are defined once, in
  `quictun`. The close-code values are also checked by phux's relay
  contract tests.
- **Two pin verifiers.** `quictun::pin` pins a leaf only. `phux_dial::tls`
  keeps its CA pinning and client identities. That is about seventy lines
  of overlap, in exchange for leaving phux's trust model out of `quictun`.
- **Still single-tenant.** ADR-0057 Decision 1's scope fence stands.
  Accounts, TCP or HTTPS ingress for clients without QUIC, and public
  certificates are not decided here.

## Alternatives

- **A separate repository now.** Deferred: the relay contract and e2e
  tests run against phux's server in this workspace. With no phux
  dependency, extracting the crate later is a move, not a rewrite.
- **Keep `phux-relay` and feature-gate the phux parts.** Rejected: the
  crate would still name phux, and the dependency would remain in its
  manifest.

## Related

- Supersedes in part [ADR-0057](./0057-minimal-reference-relay.md)
  (Decision 2: the relay's home and its one phux dependency). Builds on
  [ADR-0051](./0051-outbound-dial-out-connector-transport.md),
  [ADR-0052](./0052-connector-route-identity-and-config.md), and
  [ADR-0180](./0180-the-relay-is-blind.md).
- Evidence: `crates/quictun/tests/relay.rs` (tunnel, two consumer ALPNs,
  refused token, unknown and offline routes, reclaim) and
  `crates/phux-relay/src/lib.rs` (phux's protocol is the wire the server
  and dialer use).
