---
audience: contributors
stability: stable
last-reviewed: 2026-06-06
---

# 0029 — One cursor authority and a repaint scheduler

**TL;DR.** ADR-0020 invariant 4 ("exactly one renderer positions the
cursor per frame") has drifted: end-of-frame CUP + DECTCEM + flush is
copy-pasted across ~6 sites, each re-deriving the None-fallback policy,
and five repaint triggers paint inline with no scheduler so two in one
`select!` iteration double-paint. We add one `end_of_frame_cursor`
emitter (the sole CUP+DECTCEM+flush authority) and one monotone
`RepaintLevel` accumulator drained once per loop iteration.

Status: Accepted
Date: 2026-06-06

## Context

ADR-0020 split the client into ratatui chrome over libghostty pane
interiors and committed five invariants. Invariant 4 says exactly one
renderer positions the cursor per frame; there is no shared cursor
state. The pane primitives themselves are intact —
`paint_full_frame` (the full-viewport painter) and `paint_focused_pane`
(the incremental single-pane helper) are the two ADR-0020 primitives,
not a sprawl of divergent paths.

What drifted is the *tail* of a frame. The end-of-frame cursor
placement — CUP, `?25h`/`?25l` (DECTCEM), and the LineWriter flush — is
duplicated across ~6 sites, each re-deriving the same three-way
None-fallback policy: `paint_full_frame` (`paint.rs:159-192`),
`paint_bar_after_pane` (`paint.rs:258-287`), the snapshot and output
non-focused no-bar arms (`server_frame.rs:370-380`, `573-583`), and the
`status_tick` arm in `driver.rs`. The focused-pane authority inside
`render_at` (`render.rs:309-330`) is legitimate and stays; the problem
is the *composite* tail run after `render_at` returns, which now has ~6
authorities instead of one. Bead scars phux-gxy / 9xn / b9n / d69 / 549
are all this one concern. phux-gxy in particular was a buffered CUP that
unit tests on the in-memory sink passed but the live LineWriter never
flushed.

Separately, five repaint triggers call `paint_full_frame` inline with
no coordination: `needs_resync` (`driver.rs:796`), `layout_replaced`
(~989), stdin `layout_changed` (~1075), the bare-ESC flush
`layout_changed` (~1137), and SIGWINCH (~1208). When two fire in one
`select!` iteration (e.g. `layout_replaced` plus a coalesced output
burst) the viewport repaints twice.

This is a client-internal consolidation. The wire is untouched
([ADR-0013](./0013-libghostty-bytes-on-wire.md) stands), structured
cell diffs are not reintroduced, and the subscription model is out of
scope. This ADR EXTENDS ADR-0020 invariant 4; it does not supersede it.

## Decision

Two free functions and one enum, threaded through the existing call
sites. No new traits, no type-state, no wire change.

1. **One end-of-frame cursor emitter.** Add to `paint.rs`:

   ```rust
   pub(super) fn end_of_frame_cursor<W: Write>(
       out: &mut W,
       cursor: Option<(u16, u16)>,
       fallback_origin: Option<(u16, u16)>,
   ) -> io::Result<()>
   ```

   It is the SOLE site that emits the composite cursor placement (CUP +
   DECTCEM) and the SOLE site that flushes for the pane/chrome
   composite. The None-fallback policy is resolved once via a private
   `CursorResolve` enum — `Show{row,col}` for `Some(cursor)`,
   `HideAt{row,col}` for `None` + `Some(origin)` (we hide because a
   `None` last_cursor means libghostty reported the cursor hidden or had
   no viewport position, so showing it at a guess would lie), and a
   `HideAt(0,0)` safety net for `None`+`None`. CUP formatting reuses the
   existing private `render::write_cup` (1-based `saturating_add(1)`); it
   is not re-open-coded. The function flushes itself, killing the
   buffered-CUP-with-no-newline hazard (phux-gxy).

   Raw `"\x1b[..H"`, `"\x1b[?25h"`, and `"\x1b[?25l"` writes elsewhere
   under `attach/` are banned. Exactly three sites keep them, each
   annotated `// CURSOR-AUTHORITY:` and allow-listed by a
   `scripts/check-cursor-authority.sh` grep gate (sibling to the retired
   `check-ratatui-boundary.sh` lineage): `paint.rs::end_of_frame_cursor`
   (composite authority), `render.rs::write_cup` (the pane-interior CUP
   formatter `render_at` uses), and `render.rs::write_reset` (RawModeGuard
   teardown, not a frame). `render_at`'s own cursor emit is the
   pane-interior authority ADR-0020 inv.4 names; `end_of_frame_cursor`
   is the *composite* realization of that same invariant, reading
   `render_at`'s `last_cursor()` back as its input — they never both own
   the final cursor, because a bar/other-pane paint always follows when
   both could run.

2. **One repaint scheduler.** Add a small accumulator (in a new
   `attach/repaint.rs`):

   ```rust
   #[derive(Default, PartialEq, Eq, PartialOrd, Ord)]
   enum RepaintLevel { #[default] None, Incremental, Full }
   #[derive(Default)]
   struct RepaintAccumulator { level: RepaintLevel, viewport_was_cleared: bool }
   ```

   `RepaintLevel` derives `Ord` in declaration order so `raise` is a
   monotone `self.level = self.level.max(new)` — idempotent and
   order-independent across triggers in one iteration. API:
   `raise_full()` (sets `Full` + `viewport_was_cleared = true`),
   `raise_incremental()`, and `drain() -> (RepaintLevel, bool)`
   returning the level + cleared flag and resetting to `Default`. The
   five inline triggers RAISE instead of painting; `needs_resync` at
   loop top also raises rather than painting inline.

   The accumulator is drained EXACTLY ONCE at the bottom of each
   `select!` iteration, in one place:

   ```rust
   let (level, cleared) = accum.drain();
   if overlays.is_active() {
       // ED2 + overlays.paint — overlay supersedes pane repaints
   } else { match level {
       None        => {}
       Incremental => paint_bar_after_pane(...),          // bar/tick only
       Full        => paint_full_frame(..., /*force_full=*/cleared),
   } }
   ```

   Because the level is a max and the drain runs once, two triggers in
   one iteration collapse to a single paint at the highest level —
   double-paint becomes structurally impossible for the loop-level
   triggers.

3. **Route paint tails through the shared emitter.** The two ADR-0020
   primitives stay; only their cursor/bar/flush tails converge.
   `paint_full_frame`'s `if final_cursor {…} else {…}` + flush tail
   (`paint.rs:159-192`) becomes one `end_of_frame_cursor` call.
   `paint_bar_after_pane`'s three-way restore tail (`258-287`) becomes
   one `end_of_frame_cursor` call after the bar emits. The two
   non-focused no-bar arms in `server_frame.rs` drop their raw
   `write!`+flush and call `end_of_frame_cursor`. After this, every site
   differs only in which panes it iterates and Full-vs-Incremental —
   never in cursor, bar, or flush logic.

The steady-state inline paints inside `handle_server_frame`
(`TERMINAL_OUTPUT`/`TERMINAL_SNAPSHOT`) stay inline and keep their
byte-level tests; they are the legitimate per-frame incremental locus.
The accumulator governs only the loop-level Full/overlay/tick repaints.
A loop-level `Full` supersedes any inline incremental paint that ran
earlier in the same iteration (the layout mutated under it).

## Why

A free-function emitter taking `(cursor, fallback)` plus a private
resolve enum is the smallest change that makes inv.4 true again: the
None-fallback decision exists in one constructor, the flush exists in
one function, and the existing phux-gxy/9xn/b9n tests retarget onto it
unchanged. A monotone max-accumulator drained once is the minimal
structure that makes the double-paint un-representable rather than
merely fixed. We keep paint inside `handle_server_frame` specifically to
preserve its byte-assertion regression suite (phux-2x9/paer/9xn) —
moving it (proposals 2 and 3) would force a test migration that is
exactly where a regression would hide. The grep gate plus
`// CURSOR-AUTHORITY:` markers make a new raw-CUP site a conscious,
reviewed act.

## Tradeoffs

- Enforcement is a grep gate, not the compiler. A crate-split or
  type-state guard (proposal 3) would be stronger but reintroduces the
  type-state and wrapper layer this consolidation is explicitly avoiding.
  We keep the allow-list tiny (3 entries) so it stays legible.
- `end_of_frame_cursor` consolidates the *emit*, not the fallback
  *computation*: callers still compute `fallback_origin` locally via
  rect lookups. A follow-up may thread the focused rect through; out of
  scope here.
- "At most one paint per iteration" holds for the loop-level triggers.
  The inline incremental path inside `handle_server_frame` is a
  deliberate, documented second locus, superseded by a same-iteration
  `Full`.

## Alternatives

- **Scheduler-owned Frame controller (proposal 2).** Handlers return
  repaint intent; all paint drains in one place, making double-paint
  structurally impossible and overlay a fourth drain target. Rejected as
  the spine: it forces `handle_server_frame`'s byte-assertion tests onto
  a level-assertion harness — the biggest blast radius and the place a
  regression hides. Its single-drain framing and explicit overlay branch
  are grafted in.
- **Type-state `FrameGuard` (proposal 3).** Paint helpers receive
  `&mut Frame`, never `&mut W`, so they cannot flush except via
  `Frame::finish(FrameCursor)`. Strongest inv.4 enforcement short of a
  linear type, but the guarantee is `#[must_use]` + a Drop backstop
  (runtime, not compile-time), it adds a wrapper and a borrow dance, and
  it touches ~12 sites plus a test migration. Its `FrameCursor` enum
  (one match for the fallback policy) and `// CURSOR-AUTHORITY:` markers
  are grafted in.
