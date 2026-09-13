---
audience: contributors
stability: stable
last-reviewed: 2026-06-27
---

# 0037 — Overlay-network reachability for remote self-host consumers

**TL;DR.** The sanctioned way to reach a self-hosted phux server from a
phone on another network is a **WireGuard overlay** (Tailscale, Headscale,
or raw WireGuard/Nebula/Netbird) — an L3 substrate that hands the client a
routable IP. phux dials it as plain `wss://`/QUIC with zero new code:
reachability is solved *below* the wire. phux stays **overlay-agnostic**
(depends on none of them) so the fully-OSS path (Headscale/WireGuard) is
first-class, not a downgrade. Relay / rendezvous / NAT hole-punching are
deliberately **not built here** — they are the enterprise platform's scope,
and the ADR-0007 transport-trait + URI-shaped-identity invariants keep that
door open.

Status: Accepted (forward-compat)
Date: 2026-06-27

## Context

Everything a remote consumer needs *except reachability* already ships:
`wss://` (TLS 1.3) and QUIC transports are wired end-to-end, `phux pair`
mints a bearer token + cert fingerprint, and a non-loopback bind
auto-provisions TLS and requires the token (ADR-0031). The mobile client
dials `wss://host:port` directly with SHA-256 cert pinning
(phux-mobile ADR-0009).

The gap is purely **packet reachability**: the phone opens a *direct*
connection to `host:port`, so it works only when the phone can route to the
server's address — same LAN, or a VPN. A self-hosted server behind
NAT/CGNAT/firewall has no inbound-reachable address. There is no NAT
traversal, relay, or rendezvous code on either side, by design — the mobile
diagnostic even says "the phone must reach the Mac's address directly."

The audience for this repo is **open-source developers self-hosting** their
own server and control plane. A managed/enterprise platform that solves
reachability *for* non-technical users (hosted relay, always-on tenants) is
explicitly **out of scope of this repo** (see phux-mobile `docs/CLOUD.md`),
but this decision must not foreclose it.

## Decision

**Adopt a WireGuard-class overlay network as the reachability layer for
remote self-host consumers, and require no phux protocol or transport change
to support it.** Concretely:

- **The overlay is an L3 substrate, not a phux feature.** Tailscale /
  Headscale / WireGuard give the phone a routable address (a `100.x` IP or a
  MagicDNS `*.ts.net` name). phux dials it exactly as it dials a LAN
  address. The existing "non-loopback bind ⇒ secure path" logic already
  trips TLS + token on a tailnet IP; cert pinning is on the *fingerprint*,
  not the hostname, so MagicDNS names work unchanged.
- **phux stays overlay-agnostic.** phux takes a hard dependency on **none**
  of these and special-cases none of them below the docs/UX layer. It sees
  an IP. Tailscale is the frictionless on-ramp; Headscale and raw
  WireGuard/Nebula/Netbird are the fully-OSS, self-hostable paths, and they
  work identically. This is the OSS-ecosystem guarantee: no first-party
  coordination service is required to connect from anywhere.
- **The only phux-side work is docs + a small UX convenience.** Document the
  overlay path in `operations.md`; soften the mobile "Same network?"
  diagnostic to point at it; and (optional) have `phux pair` surface the
  detected tailnet/MagicDNS address so the operator copies a working remote
  URL instead of guessing a `100.x` IP. The mobile app already auto-files
  tailnet servers into a workspace (phux-mobile ADR-0016).
- **Relay / rendezvous / hole-punching are not built here.** Hosted relays,
  STUN/TURN, ICE, and reverse tunnels that would let a *non-overlay* client
  reach a NAT'd server are enterprise-platform scope, deferred per ADR-0007.

## Why

- **It dissolves the problem below the wire.** The cleanest fix is the one
  that needs no phux code: an overlay makes the server reachable, and phux
  is none the wiser. Less surface, less crypto, nothing to maintain.
- **It is the most secure option available.** WireGuard gives mutual-auth,
  forward-secret L3 encryption through CGNAT, *under* phux's own TLS+token —
  defense in depth, from a vetted stack, honoring the no-homegrown-crypto
  rule (CONTRIBUTING.md).
- **Overlay-agnosticism keeps the OSS story strong.** A self-hoster who will
  not depend on Tailscale Inc. runs Headscale or raw WireGuard and loses
  nothing. phux endorses the *category*, not a vendor.
- **Doing nothing at the protocol layer is what keeps enterprise open.**
  Coupling to a specific overlay below docs/UX is the one move that would
  foreclose the future hosted-relay path. By treating reachability as an
  external L3 concern, the ADR-0007 invariants (transport is a trait; no
  domain code names a concrete transport; identities are URI-shaped with
  `LOCAL`/`SATELLITE` tags reserved) remain intact, so a relay/hub drops in
  additively when the enterprise platform wants it.

## Tradeoffs

- **The user must install an overlay on both ends.** Fair for OSS devs (many
  already run Tailscale); wrong for non-technical end users — which is
  exactly why the hosted-relay path exists as the enterprise offering. The
  two are complementary, not competing.
- **Trust extends to the overlay's coordination plane.** Tailscale's
  control plane (or your Headscale instance) mediates key exchange. Mitigated
  by phux's own TLS+token riding *on top*: a compromised coordinator still
  cannot present phux's pinned cert or the bearer token.
- **No in-repo "connect from anywhere" button.** Reachability lives outside
  phux, so the app cannot make it one-tap without adopting the very
  relay/account infrastructure this ADR defers. Accepted: a docs answer now,
  a product answer later.

## Alternatives considered

- **Outbound reverse tunnel** (server dials a relay that exposes a public
  host; e.g. cloudflared-style or a phux-run relay). Solves reachability with
  zero user network config, but requires running/depending on a relay
  service — that *is* the enterprise/CLOUD.md product. Deferred, not
  rejected.
- **P2P NAT hole-punching (STUN/ICE over QUIC).** Most "phux-native";
  QUIC migration/keep-alive already exist. Rejected for now: needs a
  signaling server *and* a TURN relay fallback for symmetric/CGNAT, i.e. the
  largest build for partial reliability — enterprise-scope if ever.
- **Mutual-TLS / SSH tunnel only.** SSH re-imports the client UX the mobile
  driver exists to avoid (ADR-0031); it remains a valid manual path for
  operators but is not the recommended answer.

## Related

- ADR-0007 — transport-as-trait + URI-shaped identities; the invariants this
  ADR relies on to keep the enterprise relay path open.
- ADR-0031 — remote-consumer TLS + bearer-token auth; the secure path an
  overlay address already trips.
- phux-mobile ADR-0016 — connection workspaces auto-file tailnet servers.
- phux-mobile `docs/CLOUD.md` — the managed/relay offering this defers to.
- `docs/operations.md` — remote-consumer trust boundary; gains the
  "connect from anywhere" overlay section.
