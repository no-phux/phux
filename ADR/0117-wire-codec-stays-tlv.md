---
audience: contributors
stability: stable
last-reviewed: 2026-09-12
---

# 0115 — The wire codec stays hand-rolled TLV; protobuf considered and rejected

**TL;DR.** The field-tagged TLV codec in
`docs/spec/appendix-encoding.md` stays. Replacing it with protobuf (or
CBOR, Cap'n Proto, bincode) would be technically fine and mildly better
for hypothetical third-party consumers — but the codec is the floor every
message, snapshot test, and FFI surface stands on, so swapping it is a
fleet-wide break under ADR-0061 for a benefit no existing consumer can
collect. The hot path is opaque bytes, where codec choice is nearly
irrelevant. The cheap 80% is a machine-readable schema of the existing
TLV. Revisit when a second independent implementation exists and can
price the demand.

Status: Accepted
Date: 2026-09-12

## Context

The wire codec is bespoke: message bodies are field-tagged TLV
(`field_id` varint + `wire_type` u8 + length-delimited value) with
skip-by-length over unknown ids, while nested values inside a field are
positional with append-only-trailing-field evolution, and leaf
`str`/`bytes` use a u32 big-endian length prefix distinct from the
envelope varint (`appendix-encoding.md` §§1–2.1). That is three encoding
disciplines where protobuf has one, with no codegen, no standard
fuzzers, and every non-Rust consumer hand-rolling the codec from prose.
`decode.rs` is ~2,200 lines. The question — asked during the 2026-09
protocol review — is whether this is wheel-reinvention we should undo.

## Decision

**Keep the codec. Record the rejection. Build the schema.**

1. No migration to protobuf or any other serialization framework.
2. The rejection is on the record (this ADR) so it stops being
   re-litigated every review.
3. The follow-up that actually pays is a **machine-readable schema of
   the existing TLV**: every frame's field ids, types, and optionality
   in one parseable file, with a test that the codec agrees with it.
   That gives third-party implementers 80% of what protobuf codegen
   would (a table to code against, mechanically checked) at ~1% of the
   migration cost.

## Why

- **The codec is the floor.** Every frame definition, the canonical
  hex-dump snapshot test, the FFI consumer, and both spec appendices
  stand on these exact bytes. Under ADR-0061's equality gate a codec
  swap is not a migration — it is a synchronized upgrade of every
  deployment with zero functional gain. There is no dual-speaking path
  worth building for an encoding change.
- **The hot path doesn't care.** `RESOURCE_OUTPUT`, bootstrap chunks,
  and history pages are opaque bytes under every candidate codec; the
  TLV envelope costs a few bytes per control frame. Protobuf would not
  make anything measurably faster or smaller where it matters.
- **The costs critics cite are real but unpriced.** Three disciplines
  vs. one, hand-rolled decoders, 2,200 lines of `decode.rs` — all true,
  and all currently paid by exactly one implementation maintained in
  the same tree as its spec. The bill comes due when the *second*
  implementation arrives (the Go/WASM viewers ADR-0010 imagines). Until
  then a migration charges a certain break against a hypothetical
  saving.
- **The bespoke parts encode real requirements.** Skip-by-length
  additive evolution is the mechanism ADR-0061's discipline rests on;
  `#[non_exhaustive]`-shaped open enums ride it. Any replacement has to
  re-provide that exact evolution story (protobuf would; CBOR schemas
  need more care), so "just use X" is never the whole proposal.

## Tradeoffs

- Every future third-party consumer hand-rolls a codec from prose until
  the schema follow-up lands. The schema is therefore not optional
  garnish — it is the part of this decision that keeps the rejection
  honest.
- The triple-discipline sharp edge (§2.1's varint-vs-u32 trap) stays.
  Mitigation is documentation plus schema-level explicitness (the
  schema names the leaf encoding per position), not redesign.
- If the second implementation ever materializes and reports that the
  TLV is the dominant cost of interop, this ADR is the document that
  gets superseded — with a priced demand attached, which is exactly
  what is missing today.

## Alternatives

- **Migrate to protobuf now.** Rejected per above: certain fleet-wide
  break, no existing consumer benefits, negligible hot-path effect.
- **Migrate to CBOR / Cap'n Proto / bincode.** Same rejection, plus:
  CBOR needs a schema story we would still have to write; Cap'n
  Proto's zero-copy promises buy nothing over opaque byte fields;
  bincode has no evolution story at all (it is the anti-ADR-0061).
- **Do nothing (no schema either).** Rejected: it converts "revisit
  later" into "relitigate every review." The schema is the commitment
  device.
- **Unify the three disciplines (e.g. varints everywhere).**
  Tempting hygiene, identical blast radius to a full migration (every
  byte changes), none of the ecosystem benefit. Rejected — if we ever
  pay a break-sized price it buys protobuf, not tidiness.

## Related

- ADR-0061 — why a codec change is break-sized (the equality gate).
- ADR-0011 — the published-crate boundary a schema would serve
  (third-party consumers import `phux-protocol`, never core).
- `docs/spec/appendix-encoding.md` — the normative codec this keeps.
- `docs/spec/appendix-reserved.md` — tag discipline, unaffected.
