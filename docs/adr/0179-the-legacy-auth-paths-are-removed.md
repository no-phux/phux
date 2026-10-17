---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0179 — The transitional and legacy authentication paths are removed

**TL;DR.** `phux server` has two modes, `local` and `paired`, and nothing
else. No mode is `local`; a remote listener, relay connector, or workload
authority location with no mode refuses to start. `PHUX_WORKLOAD_MTLS`,
the legacy token-store migration, CA learning on first connect, the leaf
beside a CA pin, the plain relayed stream, and the self-signed fallback are
gone. Old clients get named errors.

Status: Accepted
Date: 2026-10-17

## Context

`workload-auth.md` §8 named two closed modes but the reference server kept
a third, unnamed one: no `[policy] mode` beside a remote listener gave every
remote connection the owner's full grant, with a startup warning. It stayed
so phones that had not adopted ticket enrollment (ADR-0154), relayed
consumers without the server's CA, and browsers kept working (phux-pjc5.6).
Around it sat older shims: `PHUX_WORKLOAD_MTLS` as a second switch for
`paired`, `phux pair --migrate-legacy` for pre-versioning token stores, a
client that learned the CA from its first leaf-pinned connection
(ADR-0153 item 5), a CA pin that also accepted the exact leaf beside it
(item 4), a self-signed server leaf when the CA could not issue (item 1),
and relayed consumers on a plain stream outside `paired` (ADR-0154 item 5).
Phones now enroll; the owner decided (2026-10-17) to remove all of it.

## Decision

1. **Two modes.** No mode resolves to `local`. No mode beside a remote
   listener or relay connector is a startup error
   (`UnsetWithRemoteListener`) naming `[policy] mode = "paired"`, as a named
   workload authority location already was. The overlay listener auto-binds
   only under `paired`. `PermissivePolicy` is deleted; the default engine is
   the `local` `ScopedPolicy`. `phux service install` with a remote listener
   (and so `phux host add`) writes `mode = "paired"` when none is set and
   refuses `local`, so the unit it installs can start.
2. **One switch.** `PHUX_WORKLOAD_MTLS` is retired: a server that sees it
   refuses to start (`RetiredWorkloadMtlsEnv`) and names `[policy] mode`.
3. **No legacy store.** A token store of anonymous lines is refused as
   `LegacyStore` with the remedy (remove it, pair again);
   `phux pair --migrate-legacy` and `phux host add`'s migration rung are
   gone.
4. **Pins are made at pairing, never learned.** `CertTrust` is `SkipVerify`
   (loopback), `Pinned(leaf)` (relays, satellites, a server no CA issued),
   or `Authority { ca }`. A CA pin accepts only leaves that CA issued.
   `AuthorityLearner`, `PinnedLearning`, and the binding's
   `learned_authority()` are removed. Ticket enrollment requires the link's
   `ca`; `phux pair --enroll` and `--relay-route` refuse a certificate no
   CA issued and name `phux workload authority --rotate`.
5. **No self-signed fallback.** A CA that cannot issue the server's first
   certificate fails the listener instead of minting a self-signed leaf.
   A pair that already exists is still never re-issued (ADR-0091).
6. **Embedders keep the seam.** `ServerConfig::policy_engine` (ADR-0072)
   still installs an engine; with no mode that is the `Embedded` posture,
   whose listeners require no certificate. `phux server` never injects one,
   so no configuration reaches it. Transport and routing tests and the
   browser demo server use it (`phux_server_testkit::embedder`).

The bearer token stays what ADR-0116 made it: outer admission inside
`paired`, never authority. No wire byte changes.

## Why

- A posture that hands remote peers the owner's grant is the exact hole
  §8 exists to close; keeping it "until clients catch up" had no end date.
- Each shim was a second way to reach the same state, and every second way
  is one more path to audit for a downgrade.
- Learning a CA from a connection is trust on first use, even when the
  leaf authenticates it; pinning at pairing reuses the channel the operator
  already trusts.

## Tradeoffs

- **Deployments break loudly.** A host with a listener and no mode stops
  starting until it sets `paired`; leaf-only phones keep their leaf pin but
  cannot enroll until they scan a link with `ca`; relayed consumers need a
  relay link with `ca`; pre-versioning token stores must be re-paired.
- **Browsers cannot reach a `paired` server** over WebTransport until
  phux-web holds a key; over WSS only with a browser-installed client
  certificate.
- **Federation over QUIC/WSS** needs the hub to present an enrolled
  certificate (`PHUX_WORKLOAD_CERT`/`KEY` in its environment); `ssh://`
  satellites are unaffected.
- **Pre-ADR-0153 servers stay leaf-pinned** until their operator rotates.

## Alternatives

- **Default to `paired`.** Rejected: every first start would mint a CA,
  and a broken CA would stop a server that serves only its owner.
- **Keep the transitional posture behind a flag.** Rejected: a flag is a
  third mode with a name.
- **Re-issue pre-CA server certificates automatically.** Rejected: it
  silently breaks every leaf-pinned device; `--rotate` is the explicit
  version of the same act.

## Related

- Supersedes in part [ADR-0153](./0153-clients-pin-the-workload-ca.md)
  (items 1, 4, 5) and [ADR-0154](./0154-devices-enroll-with-a-ticket-over-their-own-alpn.md)
  (item 5's plain stream outside `paired`).
- [ADR-0116](./0116-workload-auth-is-mtls.md), [ADR-0072](./0072-prune-policy-vocabulary-keep-the-seam.md).
- `docs/spec/workload-auth.md` §2, §8, §8.2.
