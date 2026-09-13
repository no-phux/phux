---
audience: contributors
stability: stable
last-reviewed: 2026-07-10
---

# 0038 — Hub-to-satellite authentication

**TL;DR.** When a federation hub dials a satellite, it authenticates as an
ordinary ADR-0031 remote consumer: a pairing-issued bearer token minted by
`phux pair` **on the satellite host**, over TLS pinned to the satellite's
certificate fingerprint. The hub's satellite registry stores the token **by
file path** (never inline in `config.toml`) plus the fingerprint pin.
SSH-derived identity is deferred, not rejected.

Status: Accepted
Date: 2026-07-09

## Context

The satellite registry (`[[satellites]]` in `config.toml`,
`SatelliteConfigEntry`) declares the remote phux servers a hub can route to
(ADR-0007). Before the hub dialer lands (phux-v45.3), one question must be
closed: **how does the hub prove to a satellite that it is allowed to
attach?** A satellite is a full phux server holding live PTYs; an
unauthenticated hub link would hand terminal control to anyone who can reach
the satellite's port.

Two auth stacks already exist in the tree:

- **ADR-0031 remote-consumer auth**: TLS 1.3 (rustls) with the server's
  self-signed leaf certificate pinned by SHA-256 fingerprint, plus an opaque
  32-byte bearer token minted by `phux pair` into a line-oriented,
  owner-only (`0o600`) token store the server verifies in constant time.
  Both `wss://` and QUIC transports carry it today; the attach CLI refuses
  routable endpoints without a pin and token.
- **The SSH envelope**: `ssh://` satellite endpoints could tunnel
  `phux server --stdio` and let SSH supply identity, as local operators do
  manually (operations.md "Federation trust model").

The hub is a daemon: whatever credential it presents must be storable and
readable without a human present, which rules out interactive prompts and
makes at-rest handling part of the decision.

## Decision

**The hub authenticates to a satellite exactly as an ADR-0031 remote
consumer — pairing-issued bearer token plus TLS/QUIC certificate-fingerprint
pinning. No satellite-specific auth mechanism is introduced.**

- **Issuance flow.** The satellite operator runs `phux pair` on the
  satellite host. It mints a token into the satellite's own token store
  (append, `0o600`) and prints the token once alongside the satellite's
  certificate SHA-256 fingerprint — before the satellite server ever starts.
  The operator copies the token into a hub-local file with owner-only
  permissions (one hex token, same line-oriented shape as the server store)
  and registers both on the hub:
  `phux satellite add lab quic://lab:8788 --token-file PATH
  --cert-fingerprint FP`.
- **Storage mirrors the existing pattern.** Tokens live in files today
  (server store, `phux pair` output); the hub references its copy **by
  path** via a new `token-file` key on the registry entry. The secret never
  enters `config.toml`, never crosses the CLI as an argument, and is never
  printed by `phux satellite list`. The fingerprint pin is not a secret and
  is stored inline as `cert-fingerprint`.
- **Threat model.** In scope: an on-path network attacker between hub and
  satellite (defeated by TLS 1.3 plus the out-of-band fingerprint pin — no
  blind TOFU), a token thief reading hub disk casually (owner-only file,
  outside the freely-copied config), and a decommissioned hub (satellite
  operator deletes its token line). Out of scope: a fully compromised hub
  host — it legitimately holds bearer tokens for every satellite it routes
  to; per-satellite tokens keep that blast radius revocable per link.
- **Rotation.** `phux pair` appends, so old and new tokens are valid
  simultaneously: mint a new token on the satellite, update the hub's token
  file, reconnect, then delete the old line from the satellite store
  (effective at listener restart, per ADR-0031). Certificate rotation is an
  operator event (the auto-generated cert is persisted and stable); when it
  happens, re-run `phux satellite add` with the new `cert-fingerprint`.
- **Fail closed.** The dialer (phux-v45.3) must refuse a routable satellite
  endpoint whose entry lacks a pin or token, mirroring
  `phux attach --quic/--ws`. Loopback endpoints keep the loopback dev
  carve-out.

## Why

- **The hub *is* a remote consumer.** A satellite cannot distinguish "hub"
  from "phone" and should not: both are peers attaching over TLS with a
  paired credential. One auth stack means the satellite side needs **zero
  new code** — the token store, constant-time verify, and pinned-TLS dialer
  all exist and are tested.
- **No new crypto, no new dependencies.** ADR-0031's stack was chosen under
  the no-homegrown-crypto rule; reusing it inherits that vetting.
- **Per-link revocation.** Each hub→satellite pairing is one token line on
  the satellite; a satellite operator can cut off one hub without touching
  other consumers.
- **File-path indirection matches how the repo already handles this
  secret** and keeps `config.toml` freely diffable, committable, and
  printable (`phux config show`) without leaking a credential.

## Tradeoffs

- **Manual copy step.** The operator moves the token from the satellite's
  `phux pair` output to a hub file by hand (or over SSH). Acceptable: it is
  the same one-time ceremony the mobile pairing flow uses, and automating it
  is dialer/UX work, not an auth-model question.
- **Bearer token at rest on the hub.** Anyone who can read the hub's token
  file is the hub. Same tradeoff ADR-0031 accepted, with the same
  mitigations (owner-only file, high entropy, per-link revocation); an mTLS
  upgrade remains open.
- **Restart-bound revocation.** Removing a token line takes effect at the
  satellite listener's next start (ADR-0031's v0.1 semantics). Hot-reload is
  future work there, and this ADR inherits it.
- **`ssh://` endpoints carry redundant machinery.** A satellite reached
  through an SSH tunnel already has SSH auth underneath; the token is then
  belt-and-suspenders. Accepted so all endpoint schemes share one model.

## Alternatives

**SSH-derived identity** (hub authenticates via the operator's SSH key;
satellite trusts the tunnel). **Deferred, not rejected.** It only covers
`ssh://` endpoints — QUIC and `wss://`, the actual ADR-0007 federation
transports, have no SSH channel — so it cannot be *the* model, only a
transport-specific bypass. It also leaves phux doing zero authentication of
its own (the objection that rejected SSH reuse in ADR-0031) and offers no
per-link revocation distinct from shell access. Revisit if `ssh://` dialing
lands and the double-auth proves annoying.

**Mutual TLS (hub client certificate).** Strongest: the credential never
leaves the hub. Rejected as the first step for the same reason as ADR-0031 —
heavier provisioning for no wire change later. It is the natural v0.2
hardening once QUIC's cert model settles, and daemon-to-daemon is where it
should land first.

**Token inline in `config.toml`.** Rejected: the config file is not
secret-grade — it is committed to dotfiles, synced, and echoed by tooling.
The server-side store already established "tokens live in dedicated `0o600`
files"; the hub follows it.

**One hub-wide shared token.** Rejected: revoking one satellite link would
require re-pairing every satellite, and a leak anywhere is a leak
everywhere.

## Related

- ADR-0031 — the remote-consumer auth stack this reuses.
- ADR-0007 — federation transports and the satellite concept.
- ADR-0037 — overlay reachability; how a hub reaches a NAT'd satellite.
- phux-v45.3 — the hub dialer that consumes this auth material.
