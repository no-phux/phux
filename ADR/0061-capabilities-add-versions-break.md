---
audience: contributors
stability: stable
last-reviewed: 2026-07-27
---

# 0061 — Capabilities add, versions break

**TL;DR.** The server rejects any client whose protocol `major.minor`
differs, with no grace window, so a minor bump is a fleet-wide break rather
than a rolling upgrade. New wire surface therefore ships as a negotiated
capability — a feature bit, a capability byte, or an additive field id —
and a version bump is reserved for changes no additive shape can express.

Status: Accepted
Date: 2026-07-27

## Context

The HELLO handshake ([`../docs/spec/proto.md`](../docs/spec/proto.md) §6.1)
carries one concrete version, not a range. The reference server compares
`major.minor` for equality and, on a mismatch, sends
`ERROR { VERSION_INCOMPATIBLE }` and closes before processing any stateful
frame. There is no negotiation down, no dual-speaking peer, and no window in
which an older client keeps working with reduced function.

That gate is deliberate — a silently half-compatible peer is a worse failure
than a loud refusal — but it makes the cost of a `minor` bump categorical
rather than incremental. A `0.7.0` client cannot talk to a `0.6.0` server at
all, for every user, for any reason.

The wire already has the additive machinery to avoid paying that: the
`ServerFeature` bitset and `ClientCapabilities` negotiated at HELLO
(proto.md §6.2), and the field-tagged TLV rule that makes an unknown field id
skippable by length
([`../docs/spec/appendix-encoding.md`](../docs/spec/appendix-encoding.md)).
`ACKNOWLEDGED_INPUT` and `FILE_UPLOAD` both shipped this way.

Nothing wrote the resulting rule down. It existed in a commit message and in
the reasoning of whoever last designed against it — which is how
[ADR-0060](./0060-self-contained-session-recording.md) came to be shaped by a
constraint a reader of that ADR could not look up.

## Decision

1. **A `minor` bump is a fleet-wide break** and is treated as one: it means
   every server and every consumer in a deployment upgrade together.
2. **New wire surface ships as a negotiated capability** whenever the wire
   admits one — a `ServerFeature` bit, a `ClientCapabilities` byte, or an
   additive field id decoders skip by length. A change that could have been
   capability-gated MUST NOT be introduced by bumping `minor` instead.
3. **`minor` is reserved for changes no additive shape can express**:
   renumbering a tag, changing the meaning of bytes already on the wire,
   reallocating a freed tag to unrelated behavior, or removing a message
   peers depend on. A PR proposing one says so explicitly, because it is
   proposing a synchronized upgrade of every deployment.
4. **A capability bit is itself a permanent contract.** Once advertised it is
   not withdrawn or re-pointed at different behavior.

The normative statement lives in proto.md §6.3; the wire-change checklist in
[`../CONTRIBUTING.md`](../CONTRIBUTING.md) points at it.

## Why

The alternative most contributors reach for is "bump the minor, it is only a
minor." Under the equality gate that phrase is false: minor and major have
identical blast radius, and the only real dimension is additive versus not.
Naming that removes a whole class of proposal from the design space before it
is drafted.

Recording is the worked example, and ADR-0060 §"Why" carries the full cost
analysis: a server-side recorder — the more capable feature, since it
survives the recorder process dying — would have cost a new command tag, a
new feature bit, and a `0.6.0` to `0.7.0` bump, breaking every deployment to
gain durability nobody had asked for. It was rejected in favor of a
consumer-side projection over the already-normative `ATTACH_TERMINAL`
observer contract, which added zero wire surface. That trade is only legible
if the version rule is written down.

Making the rule normative also gives the capability path a reason to exist.
`ServerFeature` bits are otherwise easy to read as optional politeness rather
than as the mechanism that keeps a heterogeneous fleet talking.

## Tradeoffs

- The wire accretes capability bits and optional field ids instead of being
  periodically cleaned up by a version break. Bits are 32-wide and cheap
  today, but the encoding is now the thing that gets complicated rather than
  the upgrade story.
- Every capability multiplies the negotiated state space, and the reference
  implementation must keep working with the bit unset. That is real test
  surface per bit.
- Designs that genuinely want new wire shape get pushed toward being
  re-derived over existing frames, which sometimes yields a less capable
  feature than a clean-sheet frame would have (ADR-0060's recorder is bounded
  by the life of the process that asked for it).
- The rule holds only as long as the equality gate does. A future range or
  down-negotiation would change the calculus and supersede this ADR.

## Alternatives

- **Version ranges in HELLO** (peers advertise `min..max` and negotiate
  down). This is the standard fix and would make minor bumps cheap, but it
  obliges the server to keep every supported encoding alive and to test the
  cross-product. Deferred, not refused: it is the natural successor if the
  capability bitset ever runs out of room or a genuinely breaking change
  becomes unavoidable.
- **Compare `major` only**, letting minors diverge. Rejected: it reintroduces
  the silently half-compatible peer the equality gate exists to prevent,
  since nothing else on the wire says which minor's semantics are in force.
- **Leave it as tribal knowledge.** Rejected — that is the failure this ADR
  documents. ADR-0060's central decision is unreadable without it.

## Related

- ADR-0060 — self-contained session recording; the worked example and the
  cost analysis this ADR references rather than restates.
- ADR-0032 — graceful server upgrade; the mechanism by which a fleet-wide
  break is still survivable for live sessions.
- ADR-0007 — mosh-class transport and satellites; federation multiplies the
  number of peers a break has to reach.
