---
audience: contributors
stability: stable
last-reviewed: 2026-08-07
---

# 0072 — Prune the policy vocabulary, keep the authorization seam

**TL;DR.** The permissive-only policy scaffolding is two decisions, not
one. Prune the unreferenced authorization vocabulary out of the published
`phux-protocol` crate now, while removing it is free. Keep `phux-server`'s
HELLO authorization seam, its permissive default, and its refusal path,
because the paired-workload feature must implement exactly that seam.

Status: Proposed
Date: 2026-08-07

## Context

A 2026-08-02 audit (bead phux-3djs) found the policy surface inert:
`PolicyEngine` had one implementation (`PermissivePolicy`, allow-all), the
runtime's `policy_bundle` was written `None` at both writers, and
`AuditSink`, `InputProvenance`, and four `authorize_*` methods had zero
callers — roughly 350 LOC in `phux-server` plus roughly 250 LOC of `pub`
types in `phux-protocol`. The bead asked: activate, or prune?

Two facts the bead did not have decide it.

First, ADR-0071 freezes the *consumer surface* at 1.0 while `phux-protocol`
stays on its own `0.x` line. Dead `pub` types in that crate are free to
delete today and load-bearing after 1.0. The window is now.

Second, bead phux-pjc5 — paired workload authentication and scoped policy
enforcement, a P0 that must "replace permissive default policy" — is
deferred to post-1.0 and will need an authorization seam at HELLO. Deleting
a seam a known feature rebuilds is churn, not cleanup.

So the surface splits along a line the audit's LOC count obscured. Verified
against the source: the seam **is wired end to end** — every transport
stamps a `PeerIdentity`, `handle_client` calls `authorize_hello` with it
once per connection, and an `Err` sends `ERROR { PermissionDenied }` and
closes. What is missing is only a non-permissive implementation. The rest
of the surface is genuinely unreachable: no encoder, decoder, or call site
names `AuditEvent`, `TaggedInput`, `Decision`, or the `TerminalOp` /
`GroupOp` / `MetadataOp` taxonomy, and `policy::MetadataScope` is a shadow
of the union the wire actually encodes (`wire::frame::Scope`).

## Decision

**Prune the unreferenced vocabulary; keep and document the seam.**

In `phux-protocol::policy`, keep exactly what live code produces or
consumes: `PeerIdentity`, `TransportType`, `QUIC_ALPN`, `QUIC_RELAY_ALPN`,
and `Capability` (the seam's argument and return type). Remove
`ConsumerId`, `Decision`, `ChallengeType`, `AuditEvent`, `AuditAction`,
`TerminalOp`, `GroupOp`, `MetadataOp`, `MetadataScope`, `AuditTarget`,
`ConsumerClass`, `InputTag`, and `TaggedInput` — and with them the crate's
`serde_json` dependency. No wire bytes change and no `docs/spec/` type is
touched, so this is not a spec change.

In `phux-server::policy`, keep `PolicyEngine` (narrowed to
`authorize_hello`), `PermissivePolicy`, and `PolicyError` (narrowed to
`Unauthorized` and `Internal`). Remove `AuditSink`, `NoopAuditSink`,
`AuditFilter`, `AuditError`, `InputProvenance`, `UnknownProvenance`, the
four zero-caller `authorize_*` methods, and `PolicyBundle` — a three-field
bundle whose other two fields are gone collapses to
`Arc<dyn PolicyEngine>`. `ServerConfig::policy_bundle` becomes
`policy_engine`, still the injection point.

The seam's status is stated in the code, not just here: the module doc says
it is deliberately permissive, names phux-pjc5, and asks the next reader not
to re-file this bead. `tests/policy_deny.rs` pins both halves of the
contract — a denying engine gets `PermissionDenied` on the wire, and the
default still admits a local client.

## Why

- **The two halves have opposite costs.** The vocabulary is cheap now and
  expensive later; the seam is cheap to keep and expensive to rebuild. A
  uniform verdict has to be wrong about one of them.
- **The removed vocabulary is not the shape pjc5 wants.** Its acceptance
  criteria name an inventory/observe/create/bind/input/signal scope matrix.
  `TerminalOp::{Snapshot, InputKey, InputMouse, …}` is a third taxonomy,
  matching neither that matrix nor the wire's frame catalog. Keeping it
  would pre-commit pjc5 to a guess made before the requirement existed.
- **Audit and provenance already have live homes.** Structured tracing
  ([ADR-0028](./0028-runtime-log-control.md)) is the observability
  substrate; agent identity is an L3 record
  ([ADR-0040](./0040-agent-identity-metadata.md)). `AuditSink` and
  `InputProvenance` were a parallel mechanism nothing adopted.
- **A tested refusal path is what makes "keep" honest.** The audit's real
  complaint was an unreachable deny branch. Two tests and a doc comment
  answer it without inventing a policy source phux has no configuration
  language for.

## Tradeoffs

- **`Capability` survives with no non-placeholder producer.** The seam
  passes a one-element stand-in because HELLO carries no capability request
  on the wire, and the granted set is discarded because nothing enforces
  it. This is the piece of pre-shaping we accept, because it is the seam's
  own signature rather than vocabulary around it.
- **`ConsumerId` is removed although ADR-0031 names it.** That mention is a
  forward reference in a design not yet implemented, not a code contract.
  pjc5 will reintroduce whatever identity handle the workload-key model
  actually needs — plausibly not a `String` newtype.
- **Deleting the seam entirely would be simpler still.** We keep roughly
  100 LOC that no shipped configuration can reach. The wager is that pjc5
  lands; if it is abandoned, this ADR should be revisited rather than left
  as a permanent exception.

## Alternatives

**Activate everything.** Wire the four `authorize_*` methods into the
handlers and give `PolicyBundle` a config source. Rejected: it would design
phux's authorization model in a cleanup task, ahead of the Blackbird pairing
requirement that is supposed to drive it, and freeze an op taxonomy that
already fails to match pjc5's scope matrix.

**Prune everything, including the seam.** Delete `phux-server::policy`
outright and let pjc5 start clean. Rejected: the call site, the refusal
path, and the per-transport `PeerIdentity` plumbing are the expensive parts
and they already work. Removing them buys ~100 LOC and costs a rebuild plus
the risk that the refusal path comes back subtly different.

**Keep the vocabulary, defer the decision to 1.0.** Rejected on the
ADR-0071 window: after 1.0 the same prune is a semver break on a published
crate, and "we will clean it later" is how the surface got here.

## Related

- [ADR-0031](./0031-remote-consumer-auth-and-encryption.md) — the pairing
  and token model that motivated this vocabulary.
- ADR-0071 — why the prune window is open now and closes at 1.0.
- phux-3djs — the audit bead this closes; phux-pjc5 — the feature that
  implements the seam.
