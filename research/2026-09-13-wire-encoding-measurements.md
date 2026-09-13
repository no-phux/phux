---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-13
---

# Tagged-field encoding allocation measurements

**TL;DR.** Reusing one scratch buffer per encoder reduces measured allocations
for ACK and 1 KiB output frames from four to one per frame. Wire bytes and
builder semantics remain unchanged. These are component allocation counts,
not CPU, latency, RSS, or WAN throughput results.

## Reproduction

```sh
cargo run --locked -p phux-protocol --example encode_allocations
```

The example invokes the production `FrameKind::encode` path 10,000 times per
case. Each case uses a preallocated 16 KiB output buffer, cleared between
frames. Frame construction and printing are outside the allocation interval.
The counting allocator delegates to `System`; its default reallocation path
also passes through the counted allocation method. No engine, transport, or
terminal rendering participates in this measurement.

Measured on macOS arm64 with Rust 1.98.1 and the locked dependencies:

| Frame | Baseline allocations | Reused scratch allocations | Encoded bytes |
|---|---:|---:|---:|
| Ping | 10,000 | 10,000 | 16 |
| FrameAck | 40,000 | 10,000 | 46 |
| ResourceOutput, 1 KiB | 40,000 | 10,000 | 1,074 |

The baseline is the encoder at `fbe220cb`, before scratch reuse; run the same
example against that revision to reproduce it. The original baseline probe
used the identical frame shapes and counting interval. Output payloads already
use borrowed field writes, so the 1 KiB payload does not add a scratch allocation.
Each frame creates its own encoder: scratch reuse is within one encoder, not
across frames. A larger field can grow scratch, and nested encoders retain
their own independent buffers.

## Compatibility and validation

The closure passed to `write_field_with` still starts with an empty, zero-based
`position()` and `buffer()` view. A direct write/backfill implementation would
change that public behavior. Keeping scratch also preserves the existing
property that a panicking builder cannot publish a partial field into the
parent output. `Encoder::new` remains a `const fn` using optional lazy scratch.

- Protocol suite: 77 unit tests, one golden-frame test, 13 allocation-abuse
  tests, 158 wire-contract tests, and one doctest passed.
- New regressions verify nested zero-based builders, exact expected bytes,
  and recovery after a builder panic.
- Integrated client-core with native-engine: 264 unit tests and two doctests
  passed with scratch reuse and bounded bootstrap staging.
- Independent GPT-5.6 Sol source review found no correctness issues in scratch
  ownership, nesting, builder isolation, or panic recovery.
- Strict all-target/all-feature protocol Clippy passed.
- Lizard: `write_field_with` 2 → 2; `Encoder::new` 1 → 1;
  measurement `main` 3. No touched function exceeds 10.

The broader acceptance matrix remains in the
[protocol optimization audit](2026-09-12-protocol-optimization-audit.md).
