---
audience: contributors
stability: stable
last-reviewed: 2026-08-17
---

# 0086 — The pooled libghostty render trio lives in `phux-protocol`

**TL;DR.** Both ends of the wire pool a libghostty `RenderState` +
`RowIterator` + `CellIterator`, and a pooled state serves stale rows when the
grid it cached is no longer the grid it walks. That trio plus its rebuild —
on a geometry change, and on a caller-named identity change — lives in one
type, `phux_protocol::render_pool::RenderPool`, behind the existing `server`
feature, with a single entry point (`begin(terminal, generation)`). The
walkers that reach `RenderPool` are `phux-server`'s `SnapshotSynthesizer` and
`phux-client`'s `TerminalRenderer`; `phux-record`, `phux-client-ffi`, and
`phux-server-testkit` still hold private copies (see Tradeoffs).
Dirty-bit policy stays at the call sites, which legitimately differ.

Status: Accepted
Date: 2026-08-15

## Context

Under [ADR-0013](./0013-libghostty-bytes-on-wire.md) a libghostty `Terminal`
runs on the server *and* on the client, so both ends walk a grid through the
same three libghostty objects. Allocating them is not free, so every walker
pools them for the life of the pane it serves. Five private copies of that
trio existed: `phux-server`'s `SnapshotSynthesizer`, `phux-client`'s
`TerminalRenderer`, `phux-record`'s `Replayer`, `phux-client-ffi`'s
`RenderCache`, and `phux-server-testkit`'s `Screen`.

Pooling carries one non-obvious hazard, learned the expensive way as
`phux-5pyx`: libghostty's per-row dirty bits live on the `Terminal` and are
drained by whichever `RenderState` reads a row first, so after a resize a
pooled state can report the *new* dimensions while still serving *pre-resize*
row bodies. Exactly one of the five copies — the server synthesizer — carried
the fix. The other four each stayed correct (or latent) by a different,
unstated mechanism, and nothing connected them.

## Decision

`RenderPool` owns the trio and the `(cols, rows)` it last walked, rebuilding
the trio when they change. `RenderPool::begin` returns the snapshot and the
two iterators as disjoint borrows, so a call site drives its walk exactly as
it did with three private fields.

Geometry is not the only staleness axis. A walker whose `Terminal` can be
REPLACED between walks — the client publishes a new replica generation per
bootstrap — must say so, so `begin` takes an opaque caller-chosen identity
token for the terminal it is walking alongside the terminal itself. A token
change rebuilds the trio even at identical geometry (`phux-994s`). The pool
never interprets the token; the client packs its replica key's `(stream_id,
bootstrap_id)` into one, via `ReplicaKey::generation_token`.

The token is a **required** argument of the one entry point rather than a
second `begin_generation` method. A walker whose terminal is fixed for the
pool's life — the server synthesizer walks one PTY-backed `Terminal` per pane
for the pane's whole life — passes a constant, which is exactly "a generation
that never changes", so behaviour is identical either way. Two entry points
would mean one of them was always the wrong one to reach for, and the cost of
reaching for it was silent grid corruption that only manifests when a
replacement terminal's pages recycle the freed allocation. On the client, the
terminal and its token travel together as one `attach::render::ReplicaWalk`
produced solely by `attach::pane_state::published_replica`, so a paint path
cannot pair a terminal with a token that disagrees.

It lives in `phux-protocol` behind the `server` feature. It carries no wire
types and does not participate in protocol versioning.

The pool owns **allocation, geometry, and caller-named terminal identity**.
Dirty-bit policy stays at the call sites.

## Why

`phux-core` is the only other crate both ends already import, and it
deliberately carries no `libghostty-vt` dependency — moving a libghostty type
there would force the domain crate onto the emulator, a trade this repo has
already declined once (see `phux-record/src/replay.rs`'s "third copy,
knowingly" note). `phux-protocol` behind `server` is where the existing
libghostty-backed render helpers that both ends need already live:
`crate::sgr` and `crate::kitty_replay`. `RenderPool` is the same shape of
thing, so this follows a settled precedent rather than opening a new one.

Dirty policy is excluded because the call sites genuinely disagree, and each
disagreement is deliberate: `SnapshotSynthesizer::mark_synced` clears both the
row bits and the snapshot bit; `synthesize_incremental` clears neither,
because an unacked diff must stay re-emittable
([ADR-0018](./0018-lazy-state-synchronization.md)); `prepare_tick` bypasses
the dirty bits entirely in favour of a per-consumer reference diff; and
`TerminalRenderer::render_at_inner` clears only the rows it drew. A type that
unified those four would erase four decisions.

## Tradeoffs

`phux-protocol` grows a module that is not about the wire. The `server`
feature gate and the module docs say so explicitly, but a reader who assumes
everything in the protocol crate is normative will be briefly wrong.

Adopting the pool gives `TerminalRenderer` the geometry rebuild it did not
have. That is the intended fix, but it is a behaviour change on the client's
hottest path, justified by a hazard that has never been reproduced
deterministically (the `phux-5pyx` bead records "No repro today").

`phux-record`, `phux-client-ffi`, and `phux-server-testkit` are **not**
adopted here. They depend on `phux-protocol` without the `server` feature, and
turning it on pulls `png` and the full libghostty type surface into crates
whose feature hygiene deliberately excludes them. Three copies of the trio
therefore remain; each is tracked separately.

**Not yet migrated, concretely.** `phux-client-ffi`'s `RenderCache`
(`crates/phux-client-ffi/src/client.rs`) still holds a raw private
`RenderState` + `RowIterator` + `CellIterator` and has never adopted
`RenderPool` — so "the trio lives in one type" is true of the walkers named
above and not yet of the whole workspace. Migrating it requires the
feature-graph decision described in the paragraph above; that is tracked in
bead `phux-u8zm`, whose id is also carried as a comment on the type itself so
the deferral is findable from the code rather than only from this ADR.

## Alternatives

**Upstream into the `libghostty-rs` fork.** The bead's first choice, and the
right long-term home: the hazard is a property of libghostty's dirty model,
not of phux. Declined for now because it commits the fork to a new public API
on phux's schedule, and the rebuild policy (which dimensions, how often) is a
consumer decision that upstream should not be asked to guess.

**A type that owns the `Terminal` too.** The bead framed this as a
"Terminal+RenderState pairing". It is not one: only `Replayer` owns its
terminal. On the server one `Terminal` is walked by several pools, one per
consumer; on the client the pool outlives individual replica generations.
Owning the terminal would be wrong at both ends. The identity token added by
`phux-994s` is the narrow part of that pairing worth keeping: the pool learns
*which* terminal it is walking without owning it.

**Detecting replacement from the terminal itself** (comparing a pointer, or
trusting libghostty's own viewport-pin comparison to notice). Declined: a
`&Terminal` is not a stable identity — a replacement whose pages recycle the
freed allocation compares equal — so the check would silently pass in exactly
the allocator-dependent case that makes the bug rare and hard to reproduce.
The caller knows the generation for certain; the pool cannot infer it.

**A closure-driven `walk(terminal, |row, cells| ...)` API.** Fully
encapsulates the iterators, but every call site's per-row body differs
(bounded writes with a byte budget, per-tick shared buffers, clip rectangles,
copy-mode inversion). The closure would need enough parameters and escape
hatches to be worse than the disjoint-borrow return.
