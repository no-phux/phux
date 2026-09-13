---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-05-30
---

# 0024 — The wire protocol owns its input atoms

**TL;DR.** [ADR-0008] reused libghostty-vt's `Key`/`Action`/`Mods` types
directly as the wire's input atoms ("no parallel universe of types"). That
couples `phux-protocol`'s wire to libghostty-vt, which is FFI-backed (a zig
static lib) and does not build for `wasm32` — so the codec can't compile for a
browser consumer (`phux-web`, ADR-0017's wire-consumer thesis). This ADR amends
ADR-0008, scoped to the wire: **`phux-protocol` defines its own
wire-representation input atoms** (`PhysicalKey` as a `u32` newtype over the
libghostty key discriminant; `KeyAction`/`ModSet`/focus/mouse as small native
types). The libghostty engine boundary is where atoms convert — `From`/`TryFrom`
impls compiled only under the `server` feature. The wire stays byte-identical;
no protocol/version change.

Status: Accepted
Date: 2026-05-30

## Context

`phux-protocol` is the one crate meant for external consumption and is the wire
contract every consumer speaks (ADR-0017: the TUI is not protocol-privileged; an
agent SDK and a browser client are peers). Per ADR-0008, its `input` module did
`pub use libghostty_vt::key::{Action, Key, Mods}` — the wire atoms *were*
libghostty's types. Both `pub mod input` and `pub mod wire` were gated behind
the `server` feature, which depends on `libghostty-vt`.

That gate means the default (libghostty-free) build of `phux-protocol` has **no
codec at all** — only `caps`/`ids`. A wasm consumer needs the `FrameKind` codec
(to speak `HELLO`/`ATTACH` and decode `TERMINAL_OUTPUT`/`TERMINAL_SNAPSHOT`), but
turning on `server` pulls `libghostty-vt`, whose `build.rs` builds/links a zig
static lib and `panic!`s on non-native targets. The wire is therefore unbuildable
on `wasm32`, blocking the browser client.

The atoms enter the wire only through five small re-exports
(`input/key.rs`, `mouse.rs`, `focus.rs`). On the wire they are already plain
integers (`key` is a `u32` discriminant, `mods` a `u16` bitset). The server is
the only place that needs the *libghostty* form — its per-terminal encoders feed
events to libghostty's key/mouse encoders to produce PTY bytes.

## Decision

1. `phux-protocol` defines its own wire input atoms, libghostty-free:
   `PhysicalKey` is a phux-owned copy of libghostty's `key::Key` enum (same
   discriminants, derives `int_enum::IntEnum` for `TryFrom<u32>`); `KeyAction`
   (Press/Release/Repeat), `ModSet` (`bitflags`, `u16`), and the focus/mouse
   atoms are likewise native. The wire encode/decode is unchanged (same bytes).
   A copied enum (not a `u32` newtype) because consumers — notably
   `phux-config`'s keybind parser — reference named variants (`PhysicalKey::Tab`).
2. `pub mod input` and `pub mod wire` are no longer gated behind `server`; the
   codec builds with default (libghostty-free) features and so compiles for
   `wasm32`.
3. `libghostty-vt` becomes an optional dependency, enabled by `server`. Under
   `server`, `phux-protocol` provides `From`/`TryFrom` conversions between its
   atoms and libghostty's, so the server's encoders convert at their boundary.

## Rationale

A wire protocol should own its wire-format types; depending on a specific
engine's Rust type layout for the *protocol* is a layering inversion. The
conversion belongs exactly where bytes meet the engine — the server's encoders —
not in the shared contract. This unblocks every non-native consumer (browser,
and future no-FFI SDKs) without a protocol change, and keeps libghostty as the
canonical *engine* (ADR-0008's real intent) while it is no longer the canonical
*wire vocabulary*.

## Tradeoffs

- The `PhysicalKey` enum is a copy of libghostty's `Key` (176 W3C key codes), so
  the two can drift. We accept the copy to decouple the wire from the engine and
  to keep `phux-config`'s named-variant ergonomics; a `server`-gated round-trip
  test (`atoms_round_trip_libghostty`) catches any divergence at CI time.
- `KeyAction`/`ModSet`/focus/mouse atoms are likewise duplicated, but they are
  tiny and stable.

## Alternatives considered

- **Keep ADR-0008, make libghostty-vt build on wasm.** Would preserve a single
  type universe, but requires either compiling the zig FFI into the wasm binary
  (the rust+zig shared-linear-memory problem) or a types-only build mode of
  libghostty-vt — upstream work with real risk, for a worse layering.
- **A `u32` newtype for `PhysicalKey` instead of a copied enum.** Leaner (no
  176-variant copy), but `phux-config`'s keybind parser maps names to
  `PhysicalKey::Tab`/`Enter`/… variants, which a newtype can't provide without a
  large named-const list — so the enum copy is the smaller change in practice.

[ADR-0008]: 0008-use-libghostty-types-directly.md
[ADR-0017]: 0017-tui-not-protocol-privileged.md
