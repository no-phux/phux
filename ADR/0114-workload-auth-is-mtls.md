---
audience: contributors
stability: stable
last-reviewed: 2026-09-12
---

# 0114 — Workload authentication is mTLS; the bespoke proof protocol is retired unshipped

**TL;DR.** ADR-0098's `phux-workload/v1` mutual-Ed25519-proof handshake is
retired before implementation — it never grew a codec, registry, or
classifier, and QUIC already ships the mutual authentication it was
rebuilding. Authentication becomes mTLS client certs on QUIC (kernel-uid
on owner UDS, bearer token as outer admission only). What survives from
0098 is the *authorization* half — closed grants, one enforcement seam,
live revocation — bound to the mTLS identity. `workload-auth.md` is
rewritten to this effect.

Status: Accepted
Date: 2026-09-12

## Context

ADR-0098 specified a bespoke mutual-auth profile (`HELLO` →
`WORKLOAD_CHALLENGE` → `WORKLOAD_RESPONSE` → `HELLO_OK`, Ed25519 proofs,
TLS-exporter / pid channel binding, pinned fingerprints, canonical
transcripts) sitting directly above a transport layer that already
performs mutual authentication. Careful design — and a second
mutual-auth protocol every future implementer must get exactly right.

Since 0098: **nothing was built** (the spec marks it "no codec, policy
implementation, registry, or classifier yet"; the only code is the
`PolicyEngine::authorize_hello` seam with `PermissivePolicy`, kept per
ADR-0072). **QUIC ships mTLS** — quinn + rustls 0.23, already in the
tree, verify client certs with no new crypto dependency. And **ADR-0031
already pointed here**: it rejected mTLS as the first step (heavier
pairing UX) and recommended it "as the v0.2 hardening once QUIC's cert
model is settled" — which has since settled (self-signed server cert +
fingerprint pin, `phux-dial/src/tls.rs`). `phux pair` already mints
credentials over a QR/link flow; enrolling a client cert is the same
flow with a better secret.

## Decision

**Authentication: use the transport's.**

- **QUIC (and `wss` where offered): mTLS client certificates.** Server
  requests a client cert at the TLS layer, verified against the phux CA
  minted on first routable listen (same auto-provisioning as today's
  server cert). The identity (fingerprint / SAN credential id) is
  stamped into `PeerIdentity` and handed to `PolicyEngine` — the seam
  0098's stages 4–5 were going to fill.
- **Owner UDS: kernel-uid, unchanged.** No proof handshake on UDS;
  0098's "paired UDS with pid binding" dies with the profile.
- **Bearer token: outer admission only.** The pairing token stays as
  the establishment gate (preamble / `Authorization` header) — "may
  knock," never "may act." This keeps 0098's own rule (transport
  admission cannot substitute for authority) and deletes the machinery
  it was defending against.
- **SSH-stdio: unchanged from 0098** — no channel binding, no paired
  admission.

**Authorization: keep 0098's closed half**, re-targeted at the mTLS
identity: closed verb/selector grants, grant∩registry-ceiling effective
authority, one pre-routing classifier (new handlers, satellite
forwarding, all-or-nothing multi-subject ops), live revocation that
terminates connections (keeping 0098's supersession of 0031's
survive-until-drop).

**Deleted, not deprecated:** the `phux-workload/v1` HELLO fields,
`0x84`/`0x04` frames, the `WORKLOAD_AUTH` bit, the proof profile. They
never shipped, so nothing is compatible with them; the discriminants
return to the reserved pool (retired-unshipped).

**Relay.** It terminates TLS on both legs for SNI routing, so a client
cert does not survive to the server: mTLS is per-hop (consumer↔relay,
tunnel↔server), and authority across a relay is *route* authority (the
tunnel's enrollment), intersected as usual. Per-client authority across
relays would need a delegation token — explicitly future work. The
relay learns no new frames either way.

**`phux pair` and the CA.** First routable listen mints a CA beside
the server cert (same state dir, same owner-only perms). `pair`
enrolls a client (mints keypair+cert with SAN = credential id, or signs
a client CSR), records id + scope ceiling, delivers material over the
already-paired channel, shown once. `rotate|revoke` work on credential
ids as today; the CA fingerprint replaces the leaf pin, so CA rotation
is explicit re-pairing.

## Why

- The handshake proves possession-of-key-bound-to-channel better than
  anything bolted above it: the binding *is* the session, and rustls's
  verifier is reviewed code we don't re-review. Every line of
  transcript construction was a line to defend forever.
- Deleting an unshipped spec is the cheapest correction available;
  each 0098 implementation stage would have concreted the shape.
- The authorization half was always the valuable half, and it is
  independent of how the peer proved its name.

## Tradeoffs

- **Cert provisioning is heavier than a token** (CSR-or-mint step in
  `pair`, private-key custody on the client). Accepted — priced by 0031
  as the v0.2 hardening; the token stays as the outer gate so cert-less
  peers fail closed at admission.
- **No attenuated authority on owner UDS.** Kernel identity keeps full
  operator authority, as today; local-agent attenuation needs a new
  mechanism (pid attestation), not this ADR.
- **Relay reduces end-to-end authority to route authority.** The
  honest reading; the alternative (end-to-end TLS through the relay)
  forfeits SNI routing, the relay's reason to exist.
- **rustls client-verifier configuration is fiddly** (verifier
  builder, rotation story). Bounded, well-trodden — not protocol
  design.

## Alternatives

- **Ship 0098 as specified.** Rejected: permanent custom handshake for
  marginal expressiveness over mTLS (endpoint-neutral proofs incl.
  pid-bound UDS) serving no deployment we have.
- **Bearer-only (revert to pre-0098 0031).** Rejected: replayable and
  unscoped; 0098's critique stands. Kept as admission, killed as
  authority.
- **SPIFFE / Biscuit / macaroons for authorization.** Deferred: the
  verb/selector matrix is small enough to own; swapping the grant
  format later doesn't move the enforcement seam. Reach for caveats
  when delegation becomes real.

## Related

- ADR-0098 — amended: proof profile retired unshipped; closed-scope
  authorization retained, re-targeted at the mTLS identity.
- ADR-0031 — bearer remote story kept as admission; its "mTLS as v0.2
  hardening" recommendation collected here.
- ADR-0072 — the `PolicyEngine` seam this fills.
- ADR-0091 — the cert-provisioning story the CA extends.
- ADR-0113 — mTLS identity is per connection; streams inherit it.
- `docs/spec/workload-auth.md` — rewritten by the spec bead.
