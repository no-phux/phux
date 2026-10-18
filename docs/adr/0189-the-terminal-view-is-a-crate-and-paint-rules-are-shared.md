---
audience: contributors
stability: stable
last-reviewed: 2026-10-18
---

# 0189 — The terminal view is a crate and paint rules are shared

**TL;DR.** The desktop's GPUI terminal (painter, input, attachments,
presentation receipts, fonts) is the `phux-gpui` crate, a second member of
the desktop's Cargo workspace that builds on the published gpui-fast. The
rules for how a `GridFrame` paints live once, in `phux_client_runtime::draw`,
and reach C as `phux_client_terminal_draw_list`.

Status: Accepted
Date: 2026-10-18

## Context

ADR-0165 made the desktop one Rust executable over the in-process runtime.
Its terminal view was a set of modules inside `phux-desktop`, so an app that
wanted a phux terminal had to take the whole desktop or copy the view. It
also called three gpui-fast methods that only phux's toolchain patches add,
so it could not build on the published gpui-fast.

The paint rules had one home: the GPUI painter's private functions. Default
colours under theme and reverse video, palette provenance, inverse and
selection, faces, hidden and blinking cells, the cursor's head, shape and
colour, and block elements as cell coverage. Every other renderer (the C
ABI's embedders, phux-mobile, the browser) would have had to work them out
again from `PhuxTerminalCell` and the metadata side table.

## Decision

1. **`phux-gpui` is the GPUI terminal view.** It lives at
   `clients/desktop/native/phux-gpui`, a member of the desktop's Cargo
   workspace, sharing its lock and its patched GPUI. It holds the modules
   that depended only on each other, GPUI and the runtime: `terminal`,
   `input`, `attach`, `fonts`, `perf` and the presentation receipts.
   `phux-desktop` depends on it and re-exports them under their old paths.
2. **Patched GPUI is a feature.** `src/platform.rs` is the only file that
   calls the methods patches 0001, 0005 and 0006 add. With `patched-gpui`
   (the desktop's build) behaviour is unchanged. Without it the crate builds
   on gpui-fast 0.1.9 from crates.io: glyphs paint from shaped lines, a
   delivery fence clears at the next frame instead of a presented-drawable
   receipt, and macOS keeps press-and-hold.
3. **Paint rules are the runtime's.** `phux_client_runtime::draw` holds
   them, generic over the renderer's colour type. `phux-gpui` calls them
   with `Hsla`. `DrawList::build` applies them to a whole frame and returns
   background runs, text runs, block cells, decorations and the cursor in
   cell coordinates.
4. **C gets the draw list.** `phux_client_terminal_draw_list` lends
   `DrawList` as C records, additive to ABI version 3. The cell grid and
   metadata queries stay for renderers that want every cell.
5. **Nothing is published yet.** The crates stay `publish = false` and
   embedders depend on them by path or git. Publishing waits for a second
   consumer to have used the API, and for `libghostty-vt` to be on
   crates.io.

## Why

The view's dependency closure was already clean: six modules that named only
each other, GPUI and the runtime. Making it a crate only moved files, and the
desktop's 435 tests, pixel fixtures and headless perf budget run unchanged.
Keeping it in the desktop's workspace keeps one lock and one GPUI. A
separate workspace would duplicate the `[patch.crates-io]` table and the
lock, and drift.

The patch feature lets an embedder start on stock gpui-fast without phux's
toolchain, and keeps the desktop on the faster, receipt-backed path. One file
of `cfg`s is cheaper than a second painter.

The paint rules are about the terminal, not the toolkit. Their home is the
crate that owns `GridFrame`, so a rule fixed once is fixed for every
renderer. Generic colours let the GPUI painter call them with no conversion
and no change to a pixel.

## Tradeoffs

Without `patched-gpui`, presentation recovery is weaker. A scene that was
submitted but never shown can clear an unknown-delivery fence. The desktop
never builds that way, and the README says so.

`phux-gpui`'s path is under `clients/desktop/native`, not a top-level crate,
so CI routes it with the desktop lane (correct, since the desktop is its
consumer) and an embedder's path dependency is longer.

The draw list is one more C surface to keep stable. It is additive and its
layouts are pinned from both sides (`tests/draw_list_abi.c` and a Rust
test).

## Alternatives

**Leave the view in the desktop and document how to copy it.** Copies drift
from every painter fix the desktop makes, and an embedder inherits the
patched-GPUI requirement anyway.

**Move the view into the root workspace under `crates/`.** Every root
consumer would then resolve the GPUI graph, which ADR-0165 and ADR-0186 keep
out of it.

**Ship cells only and let each renderer decide.** This is what the C ABI did.
Every renderer repeated provenance, inverse, selection and cursor rules, and
none had the desktop's tests.
