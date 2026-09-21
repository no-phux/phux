---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-21
---

# 0135 — One binding crate: one projection, two encoders

**TL;DR.** A binding crate is one crate: a `projection/` layer that derives
the product vocabulary from `phux-client-runtime` exactly once, and one
encoder per foreign language behind a Cargo feature. `phux-mobile-ffi` is
merged into `phux-client-ffi` as its `uniffi` encoder; the C ABI is the
default `c-abi` encoder. Two hand-written projections of one runtime were
the drift ADR-0133 exists to prevent.

Status: Accepted
Date: 2026-09-21

## Context

ADR-0133 put one connected-client runtime below every binding, and
ADR-0134 settled that UniFFI stays for mobile with its shim in this
repository. Both left the shims themselves alone, and there were two:

- `phux-client-ffi` (20k lines with tests) read runtime `Event`s into
  `PHUX_CLIENT_EFFECT_STATUS` pairs, and read a `GridFrame` into a
  borrowed `PhuxTerminalGridView`.
- `phux-mobile-ffi` (2.5k lines) read the same `Event`s into `WireEvent`s
  through `project_lifecycle` / `project_terminal_signal` /
  `project_delivery_outcome`, the same `Topology` through
  `project_topology`, and the same `GridFrame` into a copied
  `GridProjection`.

Nothing made the two agree. A runtime event added in one and read in the
other is not a compile error; a folding rule changed on one side is not a
test failure. The `phux.agent/v1` attention derivation, the
`Idle`/`Connecting`/`Negotiated` fold, and the three-way delivery outcome
each existed once per shim, by hand, with no shared test.

## Decision

1. **A binding crate is one crate.** `phux-client-ffi` is the binding
   crate. `crates/phux-mobile-ffi` is deleted.
2. **One projection layer.** `src/projection/` derives, from runtime
   values and nothing else, the terminal-signal and lifecycle families,
   the topology, the connection status, the delivery/upload/transcribe/
   listing outcomes, the agent badge, and the reading of a `GridFrame`.
   It is binding-neutral Rust: no `#[repr(C)]`, no `uniffi` derive. Its
   unit tests pin every mapping decision.
3. **One encoder per language, behind a feature.** `c-abi` (default) is
   `src/c/`: today's `extern "C"` surface, `include/phux/client.h`
   byte-identical, mechanical over `projection/`. `uniffi` (off by
   default) is `src/uniffi/`: the `RemoteClient` object, the playground
   `TerminalEngine`, the keymap and the predictor, mechanical over the
   same layer. An encoder may ignore a projected fact it has no
   vocabulary for; it may not decide what the fact means.
4. **The mobile artifacts are the same crate under a feature.**
   `ffi-xcframework.yml` and `ffi-android.yml` build
   `-p phux-client-ffi --features uniffi`. Artifact names
   (`PhuxMobileFFI-*`), the `PhuxFFI` Swift module name and the
   provenance format keys are unchanged, so phux-mobile re-pins with a
   `PHUX_REV` bump.
5. **Cockpit never links UniFFI.** `just cockpit-ffi` and
   `cargo build -p phux-client-ffi --profile ffi-release` build the
   default features; the generator stack is compiled only for the mobile
   artifact jobs.

## Why

ADR-0133's rule — one runtime, one interpretation of it — was enforced for
transport and dropped for meaning. One crate with one projection layer
enforces it where it was leaking: a new runtime event is now one decision
in one place, with one set of tests, and both languages get it or neither
does. The feature split is what keeps that from costing Cockpit anything.

## Tradeoffs

- A C ABI crate now carries feature-gated proc macros. They are optional
  and off by default, so the default graph and Cockpit's archive are
  unchanged, but `--all-features` builds compile the generator stack.
- The artifact library file name changed: `libphux_mobile_ffi.a` and
  `libphux_mobile_ffi.so` became `libphux_client_ffi.*`, and the Kotlin
  file became `phux_client_ffi.kt`, because UniFFI names both after the
  crate. The xcframework, the zip and the Swift module are unchanged, so
  only the build scripts saw it.
- The UniFFI lane lost its engine-free `wire`-only build: the merged
  crate always compiles the native engine, so its checks need the pinned
  Zig toolchain. The shipped artifact was always built with the engine.
- Foreign record names are mirrored rather than derived: `SessionTopology`
  is a `From` of `projection::topology::SessionGraph`. See below.

## Alternatives

**Keep two crates and share a third.** Rejected: a third crate is the same
layering with one more manifest, and the thing that actually drifted — the
two encoders' reading of one value — stays two crates apart.

**One generator for C and Swift/Kotlin.** Rejected in phux-mobile ADR-0031
and unchanged here: BoltFFI's C output is partial, and a generated C ABI
would not be `include/phux/client.h`.

**Put the projections in `phux-client-runtime`.** Rejected: string terminal
ids, "no declared agent" badges and copied cell arenas are binding
vocabulary. The runtime would carry them for consumers that never ask.

**Derive the foreign records straight from the projection types.**
Rejected: UniFFI 0.28's `remote` derives only reach types in other crates,
so this would mean `uniffi` derives inside the binding-neutral layer,
which decision 2 forbids — and it would rename the published Swift
surface for no product reason.
