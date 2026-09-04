---
audience: contributors
stability: stable
last-reviewed: 2026-09-03
---

# 0098 — Workload proof and closed-scope authority

**TL;DR.** Workload clients authenticate with mutually bound Ed25519 proofs,
a persistent server-authority fingerprint, a fresh server incarnation, and a
transport channel binding. The server intersects requested authority with an
owner-managed registry, enforces a closed verb/selector matrix before routing,
and terminates live connections when that authority expires or is revoked.

Status: Accepted (forward-compat)
Date: 2026-09-03

## Context

Every transport reaches one `PolicyEngine::authorize_hello` seam, but production
policy is permissive and discards its returned capability
([ADR-0072](./0072-prune-policy-vocabulary-keep-the-seam.md)). Socket possession
therefore implies input, signals, metadata mutation, and satellite forwarding.

That is acceptable only for an owner-only local socket, not an agent, Cockpit,
or remote workload. Cockpit correctly treats stable authority key material,
server incarnation, and endpoint as three different identities.

[ADR-0031](./0031-remote-consumer-auth-and-encryption.md) put a bearer token in
the WebSocket upgrade and banned in-band auth. The token remains useful
transport admission, but cannot prove possession or express least authority.
This decision amends that doctrine. It shares authentication machinery with the
logically separate coordinator from
[ADR-0092](./0092-durable-work-coordinator-authority.md), never its L1 namespace.

## Decision

### Mutual, channel-bound proof

Adopt the endpoint-neutral `phux-workload/v1` profile in
[`docs/spec/workload-auth.md`](../docs/spec/workload-auth.md). After version
equality, the server signs the service, version, fresh client/server nonces,
current 128-bit incarnation, transport binding, and persistent authority key.
The client verifies that proof and its pinned SHA-256 authority fingerprint,
then signs the same evidence plus key id, endpoint-owned scope bytes, and expiry.
Key import and proof verification use strict RFC 8032 point/scalar acceptance,
not a library's permissive default.

TLS uses an exporter. Paired UDS uses kernel-observed uid/gid/pid that the client
independently checks; platforms lacking them cannot offer paired UDS. The
authority's owner-only key persists across restarts and endpoints, while the
incarnation remains volatile replay identity. Key replacement changes the
fingerprint and requires explicit re-pairing, never silent trust on first use.

### Closed grants and one enforcement seam

Terminal grants pair a known verb bitset with one `Terminal`, `Group`, `Host`,
or `Global` selector. The six verbs are `Inventory`, `Observe`, `Create`,
`Bind`, `Input`, and `Signal`. Unknown bits/tags, duplicate or non-canonical
grants, and unclassified client frames fail closed.

Effective authority is the logical intersection of signed request and live
registry ceiling, expiring at the earlier time. Conjunctive selector provenance
is retained and re-evaluated against authoritative topology on every dispatch;
a Group ceiling can never become unconditional Terminal authority.

The total matrix runs before lookup, relay, queue, mutation, or handler; nested
commands are checked before the satellite branch and multi-subject operations
are all-or-nothing. It classifies server-interpreted metadata before generic
L3 writes, resolves owner-addressed spawn against the actual destination Group,
and retains UDS as an additional SHUTDOWN predicate. A scope miss denies only
that operation; handshake failure, expiry, and revocation close the connection.

### Registry, policy modes, and secret boundary

`<state-dir>/workload-keys` is an owner-only, no-follow, atomically replaced
public-key registry with scope ceilings, expiries, and revocation state. It is
live authority, not a cache. `phux workload add-key|list|revoke|authority`
handles only public material and fingerprints; no Blackbird query or second
coordinator participates ([ADR-0095](./0095-the-blackbird-boundary.md)).

Private keys come from an inherited descriptor, owner-only file, or OS
credential store such as Cockpit's Keychain backend. Secret bytes never enter
argv, environment values, the registry, output, diagnostics, logs, or `Debug`.

`local` admits only the kernel-authenticated owner UDS with the full operator
grant. `paired` requires workload proof on every stateful transport, including
UDS, in addition to transport security. Unset mode is valid only for owner UDS
with no registry; a registry/non-UDS listener without explicit mode, or paired
mode with unsafe/missing key material, refuses startup. There is no bearer-only
or SSH-auth-suffices mode; SSH-stdio is unavailable until a profile defines an
independently verifiable channel binding.

Registry removal, revocation, expiry, ceiling reduction, or malformed reload
blocks dispatch, flushes a denial/detach when possible, releases connection
leases/subscriptions, and closes. This supersedes ADR-0031's
survive-until-drop rule.

## Why

Server proof defeats an impostor endpoint; client proof defeats bearer replay.
Client/server nonces, incarnation, and channel binding prevent challenge,
restart, and cross-connection replay. Canonical bytes remove parser equivalence.

Pairing each verb set with one selector avoids cross-product privilege growth.
One pre-routing classifier covers new handlers and satellite forwarding.
Additive HELLO fields, two lifecycle frames, a HELLO_OK grant, and one permanent
feature bit follow [ADR-0061](./0061-capabilities-add-versions-break.md) while
paired peers still refuse downgrade.

## Implementation sequence

```text
Stage 1: ADR-0098 + normative spec
  ├─ Stage 2: wire types, strict codecs, total classifier
  └─ Stage 3: authority/key registry + public-only CLI
             └─ Stage 4: handshake integration + retained grant
                    └─ Stage 5: pre-routing frame/command enforcement
                           └─ Stage 6: live revocation, policy cutover,
                                      adversarial/secret-leak conformance
```

Wire and registry lanes run in parallel; handshake joins them, then enforcement,
then revocation/default cutover after the classifier has no bypass.

## Tradeoffs

- Two strict Ed25519 signatures add one round trip and a persistent server
  secret; the reference `ed25519-dalek::verify_strict` path may add a dependency
  if the existing crypto stack cannot prove the same RFC 8032 acceptance set.
- Exact canonical encoding makes profile evolution deliberate: a changed proof
  transcript requires `phux-workload/v2`, not permissive unknown-field skipping.
- Paired UDS is unavailable where peer pid cannot be authenticated. This is a
  refusal, not an empty-binding fallback.
- Live revocation can terminate useful work immediately. Preserving a connection
  after its authority changed would make the registry advisory instead of true.
- `local` remains convenient, but only because its transport set is closed to the
  owner UDS. Remote operation requires explicit paired policy.

## Alternatives

**Transport-only bearer authentication.** Retained as an outer WebSocket gate;
rejected as workload authority because it is replayable and unscoped.

**Client proof without server proof.** Rejected: Cockpit could not bind durable
provider identity to verified authority key material.

**Timestamp freshness.** Rejected for challenges: single-use nonces,
incarnation, and channel binding need no synchronized clocks. Expiry is policy.

**Per-handler checks.** Rejected because new handlers and forwarding drift.
Enforcement belongs at the common dispatch and relay seams.