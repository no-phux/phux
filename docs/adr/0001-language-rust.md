---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0001 — Use Rust

**TL;DR.** phux is implemented in Rust. The wire protocol is a tagged union and the server is a graph of long-lived state machines, both of which fit Rust's algebraic types and exhaustive matching. A maintained safe Rust crate over `libghostty-vt` eliminates the FFI-tax argument that would have favored Zig.

Status: Accepted
Date: 2026-05-24

## Context

phux is a long-running daemon owning many PTYs and terminal grids, plus
one or more clients with their own rendering needs. We must choose an
implementation language. The realistic candidates are C (tmux's
choice), Zig (libghostty's choice), and Rust.

## Decision

We use Rust.

## Rationale

- **Type system fits the problem.** The wire protocol is a tagged union
  of message types; the server is a graph of long-lived state machines.
  Rust's algebraic data types and exhaustive matching are the natural
  shape for both. The protocol code we write will look almost exactly
  like the specification document.
- **High-quality Rust bindings to libghostty already exist.**
  [libghostty-rs][libghostty-rs] provides idiomatic safe wrappers over
  `libghostty-vt` (`Terminal`, `RenderState`, `KeyEncoder`, etc.),
  tracks upstream Ghostty, and is endorsed by upstream. The "FFI tax"
  argument for choosing Zig does not apply.
- **Memory safety for a long-running daemon.** Use-after-free in a
  server that owns user sessions for weeks is a serious operational
  hazard. The borrow checker eliminates the class.
- **Ecosystem.** Mature crates for everything we need: `tracing`,
  `clap`, `thiserror`, `proptest`, `insta`, `nextest`, `deny`,
  `slotmap`, async runtimes.

[libghostty-rs]: https://github.com/Uzaaft/libghostty-rs

## Tradeoffs

- **Compile times are slower than C or Zig.** We accept this for the
  type-system payoff.
- **The data model is a graph.** Rust's affine ownership fights cyclic
  references. We address this with `SlotMap<Id, T>` indirection (see
  `ARCHITECTURE.md`), which has the side benefit of producing the
  stable IDs the wire protocol needs anyway.

## Alternatives considered

- **C.** tmux's choice. Smallest runtime, maximum portability, but
  starting a new system-software project in C in 2026 is hard to
  justify: no memory safety, weaker abstractions, worse tooling, and
  the protocol layer would require hand-written codecs that are
  routine in Rust.
- **Zig.** libghostty's native language; no FFI seam. The seam
  argument is undermined by the existence of a maintained safe Rust
  crate. Zig also remains pre-1.0; the stdlib churn rate is a real
  long-term tax, and the async story is unresolved.
