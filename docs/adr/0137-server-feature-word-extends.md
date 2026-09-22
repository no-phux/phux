---
audience: contributors
stability: stable
last-reviewed: 2026-09-21
---

# 0137 — The ServerFeature word grows by a trailing u32

**TL;DR.** The `ServerFeature` u32 is closed. `0x80000000` is not a
feature bit, and the never-used low gaps stay unused. The next
capability is a second trailing `u32`, `features_ext`, inside
`HELLO_OK` server caps. An old client reads only the first word and
treats every new bit as unset. Bits, spec changelog rows, and
command and event tags are first-come on main.

Status: Accepted (forward-compat)
Date: 2026-09-21

## Context

`ServerFeature` is one `u32` in `HELLO_OK` field 4, after the `layers`
byte ([proto.md](../spec/proto.md) §6.2).
[ADR-0061](./0061-capabilities-add-versions-break.md) makes that bitset
the way new wire surface ships without a `minor` bump. Twenty-six bits
are taken, from `ACKNOWLEDGED_INPUT` (`0x00000010`) through `APPROVALS`
(`0x40000000`). The reference list is `ServerFeature::ALL` in
`crates/phux-protocol/src/caps.rs`.

Six values in the word are not features:

- `0x00000001`, `0x00000002`, `0x00000004`, and `0x00000008` were never
  assigned. The bitset started at `0x00000010` (spec changelog
  `0.5.0-draft.25`). Those same numbers are `EngineFeature` bits in a
  different set.
- `0x00001000` is retired-unshipped. It was specified as `WORKLOAD_AUTH`
  and never implemented
  ([ADR-0116](./0116-workload-auth-is-mtls.md)). It must not be reused
  without a version bump.
- `0x80000000` is the last unallocated high bit. `from_wire` already
  ignores it.

On 2026-09-15, #718 took `CLOSE_TAB_RESOURCES = 0x10000000`, changelog
row `0.9.0-draft.23`, and command tag `0x1d` while PHA-406 L20 was in
flight planning `KEYED_SIGNAL` for that bit. `KEYED_SIGNAL` landed at
`0x20000000` and `APPROVALS` at `0x40000000`. PHA-333/334 multiplexing
will need bits; this record gives them a place and does not design
that program.

## Decision

1. **Word 0 is closed.** Do not assign `0x80000000`. Do not assign the
   four never-used low bits. Leave `0x00001000` retired. No new
   `ServerFeature` variant uses this `u32`.
2. **The next feature is bit `0x00000001` of a trailing `features_ext`
   `u32`** in the same `server_caps` record, after the existing
   `features` word. This ADR does not put that word on the wire. The
   PR that takes the first word-1 bit adds the encoder, the decoder
   read, the proto.md sentence, and the changelog row together. Until
   then, `HELLO_OK` bytes stay as they are.
3. **Old clients keep working.** The reference decoder reads `layers`
   and, when at least four bytes remain, one `u32`. Bytes after that
   word are left unread and are not an error. A client from before the
   extension therefore sees only word 0 and treats
   every word-1 bit as unset, so it must not send a word-1-gated
   command. A server that sets any word-1 bit still emits the word-0
   `u32` first, even when word 0 is zero. Emitting word 1 alone would
   make an old decoder read those bits as word 0. Omitting
   `features_ext` when it is zero keeps today's encoding. Absence
   means every word-1 bit is unset. No `minor` bump.
4. **Identifiers are first-come on main.** A `ServerFeature` bit, a
   `docs/spec/CHANGELOG.md` row, and a command or event tag are claimed
   by the commit that lands, not by a branch plan. Re-read `caps.rs`,
   proto.md §6.2, the changelog, and
   [appendix-reserved.md](../spec/appendix-reserved.md) at every rebase
   and take the next free value actually on main. #718 is the example:
   a planned bit, row, and tag moved under a sibling.

## Why

A second `u32` is the extension the current encoding already allows.
`server_caps` grew from one `layers` byte to that byte plus a `u32`
because old decoders stop and new bytes trail. A length-prefixed
feature list would also trail, but it is a new grammar, and the demand
is a handful of multiplexing bits, not an open catalog. Using
`0x80000000` as a sentinel does not help: old clients ignore unknown
bits, and presence is "four bytes remain." Spending it on a feature is
the race this record exists to close. Four low gaps do not cover the
next program, and a second place to allocate is how #718 happened.

## Tradeoffs

- Word 0 leaves five numeric holes (`0x1` through `0x8`, and
  `0x80000000`) plus the retired `0x1000`. That waste is the cost of
  one allocation space.
- The first word-1 feature pays for the trailing word, the decoder's
  second read, and tests that a remainder still decodes as word 0.
  Later word-1 bits are then ordinary allocations.
- A client that does not know `features_ext` cannot be told that a
  word-1 feature exists. That is the ADR-0061 contract.
- Another 32 bits can fill. Widening again is a later decision. This
  record does not choose a list or a third word in advance.

## Alternatives

- **Take `0x80000000` for the next feature.** Rejected. It spends the
  last slack before the extension exists.
- **Reuse `0x1`, `0x2`, `0x4`, and `0x8`.** History is clean, unlike
  `0x1000`, but reuse keeps word 0 open and postpones the extension
  until the next collision.
- **A length-prefixed feature list now.** Rejected as the immediate
  shape. It stays available once `features_ext` is full, appended
  after that word so old decoders still stop.
- **Bump `minor` and widen the field in place.** Rejected under
  ADR-0061. A minor bump refuses every older client instead of hiding
  the new bits from them.

## Related

- ADR-0061 — capabilities add and versions break. This record is the
  successor it names for a full bitset.
- ADR-0116 — retires the unshipped `WORKLOAD_AUTH` bit at `0x00001000`.
