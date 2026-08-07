---
audience: contributors
stability: stable
last-reviewed: 2026-08-07
---

# 0071 — What phux 1.0 commits to

**TL;DR.** 1.0 is a promise about the *consumer surface* — CLI grammar, exit
codes, `--json` documents, config schema, action/hook/widget vocabulary, MCP
tools, file locations — under semver with the existing deprecation cycle. It
is explicitly **not** a promise about the wire, which keeps its own `0.x` line
under ADR-0061. The compatibility unit is the release, not the frame.

Status: Proposed
Date: 2026-08-07

## Context

phux ships at `0.12.1` with mature release machinery — release-please owns the
tag and changelog, `just release-preflight` proves local coherence, an interop
gate and a Homebrew tap follow — and no written statement of what a major
version would mean. The README's Status section names surfaces as "still
pre-1.0" without saying what 1.0 would change about them.

Two version axes already exist and are routinely conflated. The workspace
version (`0.12.1`) tracks the binary. `PROTOCOL_VERSION` (`0.7.0`, ADR-0067)
tracks the wire, and [ADR-0061](./0061-capabilities-add-versions-break.md)
already declares that a wire `minor` bump is a fleet-wide break with no grace
window: a `0.7.0` client cannot talk to a `0.6.0` server at all, for anyone,
for any reason. That is deliberate — a silently half-compatible peer is worse
than a loud refusal — but it means the wire cannot honestly carry a 1.0
compatibility promise while the ADR-0067 client kernel, history lanes, and
frontend adapters are mid-migration.

Meanwhile the surfaces consumers actually script against are stable in
practice and have been for several releases: the CLI grammar is pinned by
ADR-0065 and a generated reference, `--json` documents carry their own
`schema_version`, the validation vocabulary has one source of truth, and
deprecated spellings already warn on a published removal schedule. Those
surfaces have earned a promise and are not getting one.

Some cleanups are free before a 1.0 and expensive after it: the deprecated CLI
spellings whose published removal window (`v0.12.0`) has passed, the
ADR-0031 policy types that are `pub` in the publishable `phux-protocol` crate
but that no wire path constructs, and wire shapes the spec allocates but
production never builds (the ADR-0030 demotion cascade).

## Decision

1. **1.0 freezes the consumer surface**, enumerated: the CLI grammar and its
   exit codes, every `--json` document (additive-only within a major, each
   carrying its `schema_version`), the config file schema, the
   action/hook/widget vocabulary, the MCP tool names and arguments, and the
   documented file and socket locations. Breaking any of them requires a major
   bump plus the deprecation cycle already implemented in
   `crates/phux/src/deprecations.rs`.

2. **1.0 does not freeze the wire.** `PROTOCOL_VERSION` keeps its own `0.x`
   line under ADR-0061's semantics, and `phux-protocol` stays `0.x` on
   crates.io. A 1.0 binary may ship a `0.8` wire.

3. **The compatibility unit is the release.** One deployment — server, local
   clients, satellites, relays — runs one release. Two consequences are in
   1.0 scope rather than optional: a one-command update path so a lockstep
   fleet is upgradable in practice, and an unbypassable HELLO version gate so
   a mismatched peer fails loudly at the handshake instead of part-way through
   a session.

4. **Anything that becomes a breaking change after 1.0 is resolved before
   it.** That is exactly two things, both on frozen surfaces: the overdue
   deprecated-spelling removals, and dead `pub` surface in the published
   `phux-protocol` crate. Wire shapes the spec allocates but production never
   constructs — the ADR-0030 demotion cascade — stay tracked rather than
   blocking, precisely because point 2 leaves the wire unfrozen; what 1.0 owes
   there is that `docs/CONCEPTS.md` and the wire agree, not that the cascade
   has run.

5. **What 1.0 explicitly does not include** is named, so its absence is not
   read as a regression: the ADR-0067 native engine-state program beyond the
   protocol layer that already shipped, the native GUI, Blackbird workload
   authentication, per-consumer federation sub-identities, and the residual
   Mosh-SSP loss bound.

## Why

The surfaces a 1.0 should protect are the ones other people's scripts and
agents depend on, and for phux those are the CLI and its JSON — not the
frames. An agent driving `phux run --json` cares that the document shape
survives an upgrade; it never sees a discriminant. Freezing the consumer
surface buys the whole benefit of a 1.0 for the people who have one.

Freezing the wire at the same moment would buy nothing and cost honesty.
ADR-0067 retired `TERMINAL_SNAPSHOT` outright and the client kernel that
consumes the new bootstrap lifecycle is still being migrated onto its
frontends; a 1.0 stamp on that would be a promise made during a move.

Keeping both axes at `0.x` until the wire settles is the option that looks
safest and is not: it withholds a promise phux can already keep, on surfaces
that have not broken in months, from exactly the consumers most likely to
automate against them.

## Tradeoffs

A 1.0 product on a `0.x` wire will surprise anyone who reads a version as one
number, and "your 1.0 client cannot talk to my 1.0 server" is a genuinely
confusing sentence. The mitigation is that the mismatch is never silent —
the HELLO gate refuses and names both versions — and that the release, not
the frame, is what a user is told to match.

Choosing the CLI and JSON as the frozen surface makes CLI mistakes expensive:
a badly named flag survives to the next major. That cost is real and is the
point; it is what the generated reference and the deprecation table exist to
make visible before the freeze rather than after.

Naming out-of-scope work in an ADR risks reading as abandonment. It is the
opposite: the native program is the largest thing phux is building, and 1.0 is
deliberately not blocked on it.

## Alternatives

**Freeze the wire at 1.0 too.** One version, one promise, no explaining. It
would require either stopping the ADR-0067 migration at its current line or
committing to compatibility shims that ADR-0061 exists to refuse. Rejected as
a promise phux cannot currently keep.

**Stay `0.x` until the wire settles.** Defensible and self-consistent, but it
withholds a commitment on surfaces that are already stable and already being
scripted against, and it leaves the pre-1.0 cleanups (dead `pub` types,
overdue deprecations) with no forcing function.

**Split release trains** — version the CLI and the protocol crate
independently with separate tags and changelogs. This is the fully honest
shape and is what the two axes already imply, but it doubles the release
machinery for one repository whose crates are published as a single tag.
Deferred: the two version numbers are documented as independent, which
captures most of the benefit without the second train.
