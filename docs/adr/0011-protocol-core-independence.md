---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0011 — `phux-protocol` and `phux-core` are independent; `IdBridge` is their only meeting point

**TL;DR.** `phux-core` and `phux-protocol` have no dependency edge in either direction. Both define same-named IDs and mirror info types deliberately: core's IDs are slotmap-generational keys for in-process safety, protocol's are u32 newtypes for wire stability. The two namespaces only meet in `phux-server`'s `IdBridge`, which allocates wire IDs monotonically and never recycles them.

Status: Accepted
Date: 2026-05-25

## Context

`phux-core` and `phux-protocol` live as sibling workspace crates with
**no dependency edge in either direction**. Both crates define types
with the same names — `SessionId`, `WindowId`, `PaneId`, plus mirror
structs `SessionInfo`/`WindowInfo`/`PaneInfo`/`LayoutNode`/`SplitDir` —
and the two namespaces only meet inside `phux-server`, where the
`IdBridge` (`crates/phux-server/src/id_bridge.rs`) maps between them.

This boundary is the hardest architectural invariant in the codebase
and the least self-evident from any one file. The two reasons it
matters are:

1. **Core's IDs are slotmap-generational keys; the protocol's IDs are
   u32 newtypes addressable across the wire.** `phux_core::ids::*` are
   `slotmap::new_key_type!` keys carrying a generation counter — the
   compiler uses that counter to catch use-after-free at test time
   (ADR-0001/0004 rest on this), and the bit layout is a `slotmap`
   implementation detail that is explicitly not stable across releases.
   `phux_protocol::ids::*` are plain `u32` newtypes server-allocated
   monotonically from `1`, with `0` reserved as a sentinel — they are
   the addressable identifiers the wire codec writes into frames.
   Forcing one type to serve both jobs costs us either wire stability
   (if we publish slotmap keys) or in-process safety (if we drop the
   generation tag). We want both.

2. **`phux-protocol` is a published crate; `phux-core` is not.** Per
   ADR-0008, the protocol crate's `server` feature depends on
   `libghostty-vt` so it can re-export libghostty's input and style
   atoms. The published-default surface (`default-features = []`) is
   the IDs, the protocol-version constant, and the codec — a
   git-dep-free shell that `crates.io` and `docs.rs` can build. This
   is the third-party-client story from ADR-0010: future CC-adapter
   crates, future tmux-CC compat shims, future Go/WASM viewers, and
   any iTerm2/Blink-style consumer that ever materializes attach by
   importing `phux-protocol` and nothing else. Letting `phux-protocol`
   depend on `phux-core` would drag the PTY plumbing, the slotmap
   registry, and the in-process domain types into every consumer that
   only wants to speak the wire. Letting `phux-core` depend on
   `phux-protocol` would either re-introduce the libghostty build
   chain into the domain crate (which currently `forbid(unsafe_code)`s
   and ships without an async runtime) or force the wire format to
   track in-process refactors.

The duplication between `phux_core::Session/Window/Pane/LayoutNode/
SplitDir` and `phux_protocol::wire::info::{SessionInfo, WindowInfo,
PaneInfo, LayoutNode, SplitDir}` looks like technical debt on first
read. It is not. The wire types carry presentation-time denormalizations
(`SessionInfo::window_count`, `SessionInfo::attached_client_count`),
cross-language-friendly representations (`i64` Unix seconds, not
`SystemTime`; `String`, not `PathBuf`), `#[non_exhaustive]` markers for
forward-compat wire evolution, and `pub` fields chosen for the codec.
The core types carry in-process semantics (a `Vec<WindowId>` ordered
list, `PathBuf` cwds, `SystemTime` timestamps) chosen for the
registry. Trying to unify them collapses one set of choices onto the
other.

New contributors have repeatedly tried to add `From`/`Into` impls
between core IDs and wire IDs and been blocked by clippy and review.
This ADR makes the constraint *first* not *learned*.

## Decision

Three invariants. Stated as MUST.

### 1. No dependency edges between `phux-core` and `phux-protocol`

Neither crate's `Cargo.toml` lists the other. `phux-core` does not
import `phux_protocol::*`; `phux-protocol` does not import
`phux_core::*`. PRs that add either edge are rejected.

### 2. Parallel ID types with identical names are intentional

```text
phux_core::ids::SessionId      // slotmap::new_key_type! — generational, opaque, in-process
phux_protocol::ids::SessionId  // pub struct SessionId(pub u32) — wire-stable, server-allocated
```

Same shape for `WindowId`, `PaneId`. `ClientId` exists only on the
protocol side (clients are an attached-state concept, not a domain
concept). The two `SessionId`s are deliberately distinct types — they
**MUST NOT** acquire `From`/`Into`/`AsRef` conversions, nor a
super-trait abstracting over them. Code paths that need both spell
both out and route the conversion through `IdBridge`.

### 3. Bridging happens in `phux-server::id_bridge::IdBridge` only

`IdBridge` (`crates/phux-server/src/id_bridge.rs`, lines 33-137) is
the **only** place in the workspace that imports both
`phux_core::ids::*` and `phux_protocol::ids::*`. Its contract:

- **`intern(core) -> wire`** is idempotent: calling it twice with the
  same core key returns the same wire id.
- **Wire id allocation is monotonic from `1`.** Zero is reserved as a
  sentinel for any future `Option<SessionId>` encoding to claim
  without collision.
- **`forget(core)` does not recycle the wire id.** Once a wire id has
  been handed out it stays retired for the server's lifetime —
  destroyed sessions reverse-lookup to `None` rather than aliasing
  some new core key. This is what makes "wire ids are stable for the
  server's lifetime" a contract `phux-client` can rely on for cache
  invalidation, predictive-echo state, and SPEC §13 attach replay.

The mirror snapshot types — `phux_protocol::wire::info::{SessionInfo,
WindowInfo, PaneInfo, LayoutNode, SplitDir}` — follow the same
pattern: they duplicate core's `Session/Window/Pane/LayoutNode/SplitDir`
deliberately and meet core's types in `phux-server` (a parallel
`info-bridge` module, by convention, when it lands). The bridge code
is the price of the boundary; the boundary is what we're buying.

## Consequences

### Positive

- **`phux-protocol` publishes independently.** Third-party-client
  implementers (per ADR-0010, including any future tmux-CC compat
  shim) depend on `phux-protocol` alone and never pull in slotmap,
  PTY plumbing, or the in-process registry. `crates.io` and `docs.rs`
  see a git-dep-free default surface; the `server` feature gate
  activates the libghostty-backed surface for consumers that need it.
- **The wire format evolves on its own version cadence** (SPEC §6).
  Bumping `PROTOCOL_VERSION` is a `phux-protocol` change with zero
  semver impact on `phux-core`. Conversely, refactoring the slotmap
  layout in `phux-core` cannot cause a wire-format break — there is
  no path from a `phux-core` change to a `phux-protocol` recompile.
- **`phux-core` stays `forbid(unsafe_code)` and async-runtime-free.**
  It is reachable from tests without spinning up the wire codec, the
  libghostty allocator, or any I/O.
- **Generational-key safety is preserved.** Slotmap's generation
  counter catches use-after-free in core unit tests; the wire never
  sees it, so a drop-and-recreate of a session changes its `core`
  generation but its `wire` id is either still valid (if not yet
  `forget`-ed) or retired (if it was) — clients always see a
  monotone, never-aliasing id space.

### Negative

- **Type duplication is real.** `SessionInfo` vs `Session`,
  `LayoutNode` (twice), `SplitDir` (twice) — five paired types today,
  potentially more as the snapshot graph grows. Every new piece of
  domain state that needs to ship in `ATTACHED` adds a mirror.
- **Bridge code in `phux-server` is mechanical but load-bearing.**
  `IdBridge` is ~100 lines today; an analogous `info-bridge` will add
  more. Mechanical does not mean free — every conversion site is a
  place a `None`-on-unknown-wire-id can leak into an `unwrap()` if
  the contract is misread.
- **New contributors must learn the constraint.** It is invisible
  from looking at either crate in isolation; it shows up as a
  reviewer comment the first time someone reaches for a `From` impl.
  This ADR is the answer to "why was that rejected?"

## Alternatives considered

### Shared `phux-types` crate that both core and protocol depend on

Tempting and superficially clean. Rejected for v0.1 for three reasons.
First, it adds a third crate to maintain, version, and document
without removing the duplication: the wire-vs-internal differences
(`PathBuf` vs `String`, `SystemTime` vs `i64`, `Vec<WindowId>` vs the
denormalized snapshot triple) are real, not cosmetic — `phux-types`
would either pick one representation (forcing the other to convert
anyway) or hold both (achieving nothing). Second, the bridge code is
already small — `IdBridge` is ~100 lines including tests, and the
info-bridge will be similar — so the per-unit cost of conversion is
not the constraint. Third, the published-crate stance (consequence
#1 above) requires that the shared crate be git-dep-free too — which
either means `phux-types` cannot re-export libghostty atoms (gutting
its value, since the wire types compose them) or it must replicate
the `default`/`server` feature-flag dance that already lives in
`phux-protocol`. Net: more surface area, same duplication, no concrete
win.

**Worth filing as a follow-up to revisit** once the protocol
stabilizes (post-v0.1, after `ATTACHED` and `PANE_SNAPSHOT` ship and
we know the actual shape of the duplication). If `phux-types` would
absorb four or more paired types AND the in-process / on-wire shapes
have converged into one canonical form, the bookkeeping math flips.
Not before.

### Single shared `SessionId` (and friends)

Considered and rejected on first principles. Slotmap generational
keys are not wire-stable — `slotmap::KeyData` packs an index and a
generation into a `u64` (or `u32`-pair, depending on the key type)
whose bit layout is documented as an implementation detail. Drop-
and-recreate of a session changes its generation. Shipping that on
the wire either commits us to slotmap's bit layout forever (worse
than the current wire format, which we own) or strips the generation
tag (losing the in-process use-after-free check that makes
`forbid(unsafe_code)` in `phux-core` work). Conversely, replacing
slotmap keys with bare `u32`s inside `phux-core` re-introduces the
ABA problem the slotmap was bought to solve. Two types is the right
answer.

### `phux-protocol` depends on `phux-core` (downward edge)

Briefly considered when the snapshot-info types were designed —
mirror types could trivially be `impl From<phux_core::Session> for
SessionInfo` if the edge existed. Rejected because every third-party
consumer of `phux-protocol` would transitively link `phux-core` — PTY
plumbing, slotmap, the registry. ADR-0010's third-party-client story
is the constraint that breaks the tie: a tmux-CC compat shim, a
WASM viewer, or a Go phux-client has no business depending on the
in-process domain crate of the server.

### `phux-core` depends on `phux-protocol` (upward edge)

Considered and rejected. Would force `phux-core` to either link
`libghostty-vt` (when the `server` feature is on) or peer through
`default-features = false`, neither of which buys anything — the
domain crate has no use for wire IDs and no use for libghostty atoms.
Adds a recompile chain (`phux-protocol` change → `phux-core` rebuild)
in the wrong direction.

## References

- `crates/phux-server/src/id_bridge.rs:33-137` — `IdBridge` definition,
  allocation contract, and tests (`intern_is_idempotent`,
  `intern_allocates_monotonically_from_one`,
  `forget_does_not_recycle_wire_ids`).
- `crates/phux-server/src/id_bridge.rs:1-32` — the doc comment that
  was previously the only written statement of the boundary; this
  ADR is its load-bearing twin.
- `crates/phux-protocol/src/ids.rs` — wire ID newtypes.
- `crates/phux-core/src/ids.rs` — slotmap-keyed core IDs.
- `crates/phux-protocol/src/wire/info.rs:1-15` — module doc spelling
  out "WITHOUT crossing the core/protocol independence boundary."
- `crates/phux-protocol/Cargo.toml` — note the absence of
  `publish = false` and the `default`/`server` feature split (the
  published-crate stance this ADR underwrites).
- `crates/phux-core/Cargo.toml` — note `publish = false` (the
  in-workspace-only stance this ADR underwrites for the symmetric
  direction).
- ADR-0008 — libghostty types re-exported by `phux-protocol`; the
  published-crate boundary this ADR extends to the domain types.
- ADR-0010 — frontend-agnostic; the third-party-client story this
  ADR's no-cross-deps invariant makes mechanically achievable.
- ADR-0001/0004 — Rust and `libghostty-vt` as the grid; the
  `forbid(unsafe_code)`-in-core stance the boundary preserves.
- SPEC §6 — protocol version cadence (the version this boundary lets
  evolve independently of core).
- SPEC §13 — `ATTACHED` and `SessionSnapshot` (the consumer of the
  mirror types this ADR justifies).
