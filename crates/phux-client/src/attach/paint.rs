//! Paint composition for the attach driver.
//!
//! Two paint paths:
//! * `paint_full_frame` — clear viewport, render every pane, dividers,
//!   status bar. Use after layout mutations, viewport resize, or attach.
//! * `paint_focused_pane` + `paint_bar_after_pane` — incremental path
//!   for `TERMINAL_OUTPUT` arrivals where only the focused pane changed.
//!
//! `content_rect` reserves one outer-terminal row for the status bar —
//! the bottom row by default, the top row under `[status] position =
//! "top"` (phux-foz.8) — so pane Rects never spill into it.

use std::collections::HashMap;
use std::io::Write;
use std::time::SystemTime;

use libghostty_vt::Terminal as GhosttyTerminal;
use phux_protocol::ids::TerminalId;

use super::pane_state::{AttachKernel, PaneSlot, published_replica};
use crate::layout::LayoutState;
use crate::render::chrome::status_bar::{
    BarInset, ComposePolicy, Position, StatusBarPainter, make_context,
};

// phux-l96p.2: the DEC 2026 constants and the nestable guard that owns them
// moved to `super::render` so the per-pane paint can open its own block
// without truncating the frame-level one opened here. `SyncOutput::begin`
// emits the mode bytes only for the outermost block.
use super::render::SyncOutput;

/// Hide the cursor for the duration of a composited frame. Emitted just
/// inside the synchronized-output block so a terminal that ignores mode 2026
/// still never shows the cursor skating across a half-painted grid; the
/// frame's own [`end_of_frame_cursor`] shows it again at the authoritative
/// position.
const CURSOR_HIDE: &[u8] = b"\x1b[?25l";

/// One composited frame: a DEC 2026 synchronized-output block that swallows
/// the flushes of everything painted inside it and ships exactly once at the
/// end.
///
/// Two problems solved together, both of which are per-frame costs on the
/// hottest path in the client:
///
/// * **One block, not none.** Before this, only the destructive full-frame
///   paint was wrapped in mode 2026. The incremental path — the one that runs
///   on every `TERMINAL_OUTPUT` — emitted pane cells, then the status bar,
///   then the cursor, each visible to the outer terminal as it landed. A
///   conforming terminal presented up to three intermediate states per frame.
/// * **One flush, not two.** [`end_of_frame_cursor`] flushes, and before
///   phux-l96p.2 the pane renderer's epilogue flushed too. Against the
///   off-loop [`super::stdout_writer::StdoutSink`] each flush is a queue push
///   plus a condvar wake of the writer thread.
///
/// The wrapper is deliberately a `Write` rather than a change to the painters:
/// each painter keeps whatever `flush()` calls it needs to be correct
/// standalone (in tests, or the headless renderer), and this type makes them
/// no-ops for the duration of a composed frame.
///
/// # Why the body is buffered
///
/// The block's DEPTH is taken in [`Self::begin`], before any painter runs,
/// and released in [`Self::end`]. That ordering is load-bearing: the pane
/// renderer opens its own [`SyncOutput`] around a dirty repaint, and it must
/// see a non-zero depth so it nests instead of closing the frame's block half
/// way through — leaving the status bar and the cursor outside the
/// transaction, which is exactly the tearing this type exists to remove.
///
/// Taking the depth eagerly means [`SyncOutput::begin`] emits `?2026h`
/// eagerly too. So it emits into a scratch buffer rather than to the sink,
/// and [`Self::end`] ships the buffer only if a painter actually wrote
/// something. A frame that emits nothing — every pane clean, the bar cached,
/// the cursor already where it belongs — therefore still costs zero bytes,
/// zero writes, and no writer-thread wake, which is what makes an idle attach
/// silent between status ticks.
///
/// The buffer is borrowed from a thread-local pool and returned on `end`, so
/// the steady state allocates nothing.
pub(super) struct FrameBlock<'a, W: Write> {
    inner: &'a mut W,
    /// The frame's bytes, including the block prologue and epilogue.
    /// `mem::take`n from the thread-local pool at `begin` and returned at
    /// `end`.
    body: Vec<u8>,
    /// The nestable DEC 2026 guard. Held for the frame's whole life so the
    /// per-pane renderer's own guard nests inside it. `None` only if the
    /// scratch buffer somehow refused a write, which a `Vec` cannot.
    sync: Option<SyncOutput>,
    /// Whether any painter has emitted through the `Write` impl. The
    /// prologue and epilogue are written directly to `body`, so this counts
    /// painter output only.
    painted: bool,
}

thread_local! {
    /// Scratch buffer reused across composited frames.
    ///
    /// Per-thread because the client's paint path is one tokio current-thread
    /// runtime (ADR-0003) and libghostty's `Terminal` is `!Send`. A nested or
    /// concurrent frame simply takes a fresh buffer, which is correct — it
    /// just does not get the reuse.
    static FRAME_BODY: std::cell::RefCell<Vec<u8>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl<'a, W: Write> FrameBlock<'a, W> {
    /// Begin a composited frame on `inner`.
    ///
    /// Opens the synchronized-output block immediately (see the type docs for
    /// why the depth cannot wait for the first write) but writes nothing to
    /// `inner` until [`Self::end`].
    pub(super) fn begin(inner: &'a mut W) -> Self {
        let mut body = FRAME_BODY.with(|pool| {
            pool.try_borrow_mut()
                .map(|mut pool| std::mem::take(&mut *pool))
                .unwrap_or_default()
        });
        body.clear();
        // Infallible: the sink is a `Vec`. `ok()` keeps the guard optional so
        // a future fallible sink degrades to "no block" rather than panicking.
        let sync = SyncOutput::begin(&mut body).ok();
        let _ = body.write_all(CURSOR_HIDE);
        Self {
            inner,
            body,
            sync,
            painted: false,
        }
    }

    /// Whether any painter inside this frame has emitted a byte.
    ///
    /// The end-of-frame cursor tail keys on this: a frame in which every pane
    /// was clean and the bar was cached has moved nothing on screen, so the
    /// host cursor is already where the last frame left it and re-placing it
    /// would be pure cost.
    pub(super) const fn opened(&self) -> bool {
        self.painted
    }

    /// Close the block and ship it: `?2026l`, then the frame's one write and
    /// its one flush.
    ///
    /// A frame in which no painter wrote closes to a no-op — the buffered
    /// prologue is discarded, nothing reaches the sink, and the writer thread
    /// is never woken.
    pub(super) fn end(mut self) -> std::io::Result<()> {
        if let Some(sync) = self.sync.take() {
            let _ = sync.end(&mut self.body);
        }
        let shipped = if self.painted {
            super::render_prof::note_paints(1);
            self.inner
                .write_all(&self.body)
                .and_then(|()| self.inner.flush())
        } else {
            Ok(())
        };
        // Return the buffer to the pool whatever the sink did.
        let body = std::mem::take(&mut self.body);
        FRAME_BODY.with(|pool| {
            if let Ok(mut pool) = pool.try_borrow_mut() {
                *pool = body;
            }
        });
        shipped
    }
}

impl<W: Write> Drop for FrameBlock<'_, W> {
    /// Release the synchronized-output depth if `end` was never reached (an
    /// early return, or a panic between the two). `SyncOutput`'s own `Drop`
    /// does the release; this exists so the guard is dropped rather than
    /// leaked through the buffer.
    fn drop(&mut self) {
        self.sync = None;
    }
}

impl<W: Write> Write for FrameBlock<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.painted = true;
        self.body.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.painted = true;
        self.body.extend_from_slice(buf);
        Ok(())
    }

    /// Swallowed. The whole point: painters inside a composited frame must
    /// not each ship their partial work to the terminal.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Memoized pane tiling for the attach loop's hot path.
///
/// [`crate::multi_pane::compute_layout_in`] is not free: it walks the split
/// tree, allocates a `HashMap` of rects, rasterizes every divider cell into a
/// `Vec`, and builds the drag hit-map. The `TERMINAL_OUTPUT` path used to run
/// it up to three times per frame (mirror sizing, the focused pane's rect,
/// the cursor-fallback origin) for a layout that changes only when the user
/// splits, resizes, zooms, or switches windows — i.e. essentially never,
/// relative to output frames.
///
/// The key is the whole tiling input: the rendered [`LayoutState`] (which
/// carries the split tree AND the focus that divider weights key on), the
/// content rect the panes tile into, and the outer viewport (so a SIGWINCH
/// misses). The content rect is the load-bearing one: it is whatever
/// [`content_rect`] currently reserves, so a sidebar toggle, a bar moving to
/// the top, and the phux-l96p.8 pane-grid RAIL row all land in the key
/// without this cache having to know they exist. Validation
/// is a structural equality check against the cached state rather than a
/// hash, so a hit is exact — there is no collision class that could paint a
/// pane at a stale rect.
///
/// A miss stores a clone of the layout; hits are compare-only and allocate
/// nothing.
#[derive(Debug, Default)]
struct LayoutCache {
    /// The inputs the cached tiling was computed from.
    key: Option<(LayoutState, crate::layout::Rect, (u16, u16))>,
    /// The tiling itself, dropped whenever the key moves.
    tiling: Option<crate::multi_pane::PaneLayout>,
    /// How many times the tiling was actually computed. Incremented on the
    /// cold path only; the memoization tests assert against it, because
    /// "returns the right rect" is true of an implementation that memoizes
    /// nothing.
    misses: u64,
}

impl LayoutCache {
    /// The tiling for `layout` inside `content`, computed only on a miss.
    fn get(
        &mut self,
        layout: &LayoutState,
        content: crate::layout::Rect,
        viewport_dims: (u16, u16),
    ) -> &crate::multi_pane::PaneLayout {
        let hit = self.key.as_ref().is_some_and(|(cached, rect, viewport)| {
            *rect == content && *viewport == viewport_dims && cached == layout
        });
        if !hit {
            self.key = Some((layout.clone(), content, viewport_dims));
            self.tiling = None;
        }
        // The `None` arm runs exactly on a miss; a hit never re-tiles.
        let misses = &mut self.misses;
        self.tiling.get_or_insert_with(|| {
            *misses = misses.saturating_add(1);
            super::render_prof::note_layouts(1);
            crate::multi_pane::compute_layout_in(layout, content, viewport_dims)
        })
    }
}

thread_local! {
    /// One cache per attach thread.
    ///
    /// Thread-local rather than a [`super::driver`] field because the key is
    /// the COMPLETE tiling input — a hit is a structural match on the layout,
    /// the content rect, and the viewport, so a cache shared between two
    /// session loops (or an attach and its re-attach) can only ever miss, never
    /// answer wrongly. That buys the memoization without threading a
    /// twenty-third parameter through `handle_server_frame`'s driver boundary
    /// and its thirteen call sites. ADR-0003 pins the client to one
    /// current-thread runtime and libghostty's `Terminal` is `!Send`, so
    /// "per thread" is "per attach loop" in practice.
    static LAYOUT_CACHE: std::cell::RefCell<LayoutCache> =
        std::cell::RefCell::new(LayoutCache::default());
}

/// Run `read` against the memoized tiling of `layout` inside `content`.
///
/// The hot path's single entry point for pane geometry. `read` sees the same
/// [`crate::multi_pane::PaneLayout`] a direct
/// [`crate::multi_pane::compute_layout_in`] would produce and should copy out
/// what it needs (rects are `Copy`) rather than borrow past the call.
///
/// Falls back to computing in place if the cache is already borrowed — a
/// re-entrant read cannot happen today, and correctness must not depend on
/// that staying true.
pub(super) fn with_tiling<R>(
    layout: &LayoutState,
    content: crate::layout::Rect,
    viewport_dims: (u16, u16),
    read: impl FnOnce(&crate::multi_pane::PaneLayout) -> R,
) -> R {
    LAYOUT_CACHE.with(|cache| {
        if let Ok(mut cache) = cache.try_borrow_mut() {
            return read(cache.get(layout, content, viewport_dims));
        }
        super::render_prof::note_layouts(1);
        read(&crate::multi_pane::compute_layout_in(
            layout,
            content,
            viewport_dims,
        ))
    })
}

/// One pane's rect in the memoized tiling.
pub(super) fn tiled_rect(
    layout: &LayoutState,
    content: crate::layout::Rect,
    viewport_dims: (u16, u16),
    terminal_id: &TerminalId,
) -> Option<crate::layout::Rect> {
    with_tiling(layout, content, viewport_dims, |tiling| {
        tiling.rects.get(terminal_id).copied()
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum StatusBarPaint {
    #[default]
    NotPublished,
    Published {
        cols: u16,
    },
}

impl StatusBarPaint {
    pub(super) fn delivered(self, painter: Option<&StatusBarPainter>, expected: &str) -> bool {
        matches!(self, Self::Published { cols } if usize::from(cols) >= expected.chars().count())
            && painter.is_some_and(|painter| painter.notice_is(expected))
    }
}

/// The server-authoritative mirror grid `(cols, rows)` used to letterbox a
/// pane within its render rect (phux-7ubw).
///
/// Reads the libghostty mirror's own grid size. On the (unexpected) error
/// path it falls back to the rect dims, which makes [`render_at_letterboxed`]
/// degrade to the prior rect-clamp paint (zero pad, no margins) rather than
/// mis-centring on a bogus size.
///
/// `pub(super)` since phux-foz.11: the `handle_server_frame` snapshot and
/// non-focused-output paints must letterbox with the SAME mirror dims as
/// `paint_full_frame` / `paint_focused_pane`, or an undersized mirror gets
/// painted at two different origins (rect origin vs centred) and the screen
/// shows doubled text until a full repaint.
///
/// [`render_at_letterboxed`]: super::render::TerminalRenderer::render_at_letterboxed
pub(super) fn mirror_dims(
    terminal: &GhosttyTerminal<'_, '_>,
    rect: crate::layout::Rect,
) -> (u16, u16) {
    let cols = terminal.cols().unwrap_or(rect.w);
    let rows = terminal.rows().unwrap_or(rect.h);
    (cols, rows)
}

/// Render one pane into `rect`, its outer-viewport sub-Rect.
///
/// The rect is the CALLER's, not re-derived here: both call sites already
/// hold the frame's tiling (`paint_full_frame` computed it, the
/// `TERMINAL_OUTPUT` path reads it from the [`LayoutCache`]), and re-tiling
/// inside this function made the incremental path compute the same layout
/// three times per frame. Callers pass the content rect itself when the
/// layout has no entry for the pane (single-pane bootstrap).
///
/// Resizes nothing: the mirror's grid is server-authoritative, and `rect`
/// only clips and positions the paint.
///
/// Returns the renderer's cached `last_cursor` (outer-viewport coords),
/// or `None` if the pane has no slot or its libghostty cursor is hidden.
/// Callers use this to restore the cursor after a status-bar paint.
pub(super) fn paint_focused_pane<W: Write>(
    out: &mut W,
    rect: crate::layout::Rect,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    kernel: &AttachKernel,
    focused: &TerminalId,
    force_full: bool,
) -> Option<(u16, u16)> {
    let slot = panes.get_mut(focused)?;
    let walk = published_replica(kernel, focused)?;
    // The mirror grid size is server-authoritative (set only at the
    // snapshot / resize-ack handler); the layout rect clips and positions
    // the paint but never resizes the pane's libghostty Terminal.
    let mirror = mirror_dims(walk.terminal, rect);
    let _ = slot.renderer.render_at_letterboxed(
        walk,
        out,
        (rect.x, rect.y),
        (rect.w, rect.h),
        mirror,
        force_full,
    );
    slot.renderer.last_cursor()
}

/// The single composite end-of-frame cursor authority (ADR-0029, phux-gxy/
/// 9xn/b9n/d69/549). Every frame ends here: this is the SOLE place that emits
/// the composite cursor placement (CUP + DECTCEM) and the SOLE place that
/// flushes the pane/chrome composite. Routing all paint paths through it keeps
/// ADR-0020 invariant 4 ("exactly one renderer positions the cursor per
/// frame") true and collapses the three-way None-fallback policy — previously
/// copy-pasted across several paint sites — into one body.
///
/// `cursor` is the focused pane's authoritative last cursor as `(row, col)`
/// (0-based). When `None`, `fallback_origin` (`(x, y)` = the focused pane's
/// `Rect` origin) parks the cursor inside the pane area and HIDES it (`?25l`),
/// so a `None` cursor never strands the host cursor at the status bar's tail
/// (bottom-right) — the visible phux-gxy/9xn symptom. `None` + `None` parks at
/// the viewport origin, hidden, as a safety net.
///
/// The trailing flush is load-bearing: stdout is a `LineWriter` and the CUP we
/// write has no newline, so without the flush it sits buffered until the next
/// pane output — which never comes for an idle pane (a shell prompt). That was
/// the real phux-gxy: prior fixes computed the right CUP but never flushed it,
/// so in-memory unit tests passed while the live terminal never saw it.
// CURSOR-AUTHORITY: composite
pub(super) fn end_of_frame_cursor<W: Write>(
    out: &mut W,
    cursor: Option<(u16, u16)>,
    fallback_origin: Option<(u16, u16)>,
) -> std::io::Result<()> {
    if let Some((row, col)) = cursor {
        tracing::trace!(row, col, "end_of_frame_cursor: restore focused cursor");
        super::render::write_cup(out, row, col)?;
        out.write_all(b"\x1b[?25h")?;
    } else {
        // No authoritative cursor: park at the focused pane's origin (or the
        // viewport origin) and hide. `fallback_origin` is `(x, y)`.
        let (x, y) = fallback_origin.unwrap_or((0, 0));
        tracing::trace!(x, y, "end_of_frame_cursor: no cursor, parking hidden");
        super::render::write_cup(out, y, x)?;
        out.write_all(b"\x1b[?25l")?;
    }
    out.flush()
}

/// Clear the viewport and paint every pane + dividers + bar from
/// scratch. Use after layout mutations, viewport resize, or initial
/// attach — anything where the previous frame may not be a coherent
/// base for an incremental repaint. For "focused pane got output"
/// situations call [`paint_focused_pane`] + [`paint_bar_after_pane`]
/// instead.
#[allow(
    clippy::too_many_arguments,
    reason = "phux-4h5a adds the sidebar reservation + painter to the existing paint context; same arg-list refactor follow-up as handle_server_frame"
)]
pub(super) fn paint_full_frame<W: super::RenderSink>(
    out: &mut W,
    layout_state: &LayoutState,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    kernel: &AttachKernel,
    focused_pane: Option<&TerminalId>,
    viewport_dims: (u16, u16),
    mut status_bar: Option<&mut StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    sidebar_painter: Option<&mut crate::render::chrome::sidebar::SidebarPainter>,
    session_name: &str,
    theme: &crate::render::theme::Theme,
) -> StatusBarPaint {
    // The full screen paint (ratatui chrome + per-pane libghostty render).
    // Its close-duration is the client-side render-lag signal the flywheel
    // reads; debug-level so it is free at the default filter, and kept here
    // (not at the 4 call sites) so every repaint is timed.
    let _paint = tracing::debug_span!(
        "paint_full_frame",
        cols = viewport_dims.0,
        rows = viewport_dims.1,
        panes = panes.len()
    )
    .entered();
    let _timed = crate::perf::PAINT_FULL.timer();
    let bar = status_bar.as_ref().map(|p| p.position());
    let ContentLayout {
        rect: content,
        rail,
    } = content_layout(viewport_dims, bar, sidebar);
    let multi = super::multi_pane::compute_layout_in(layout_state, content, viewport_dims);
    // Component painters flush independently. Keep their intermediate states
    // hidden from the outer terminal while the destructive frame is rebuilt.
    let sync = SyncOutput::begin(out).ok();
    // ED2 (clear screen) + cursor home. Cheap and unambiguous.
    let _ = out.write_all(b"\x1b[2J\x1b[H");
    // Non-focused panes first; chrome (dividers + status bar) next; the
    // focused pane's render_at is intentionally the LAST cursor-touching
    // emit in the frame so it owns final cursor position + DECTCEM. This
    // matters on fresh attach where libghostty's snapshot may not yet
    // expose a `cursor_viewport`, so a "restore cursor after the bar"
    // strategy strands the cursor invisible.
    for (id, rect) in &multi.rects {
        if Some(id) == focused_pane {
            continue;
        }
        if let (Some(slot), Some(walk)) = (panes.get_mut(id), published_replica(kernel, id)) {
            // Force a full redraw: the ED2 above cleared the screen, so
            // unchanged pane content must still be emitted.
            let mirror = mirror_dims(walk.terminal, *rect);
            let _ = slot.renderer.render_at_letterboxed(
                walk,
                out,
                (rect.x, rect.y),
                (rect.w, rect.h),
                mirror,
                true,
            );
        }
    }
    let panes_ref = &*panes;
    let _ = crate::render::chrome::dividers::render_dividers(
        out,
        &multi,
        content,
        rail,
        focused_pane,
        theme,
        |id| super::pane_state::pane_label(panes_ref, id),
    );
    // Paint the sidebar strip into its reserved columns. The ED2 above cleared
    // it, so invalidate the painter's cache to force a re-emit even if the
    // window list is byte-identical to the previous frame. The strip occupies
    // the columns `content_rect` carved out, so it never overlaps pane content.
    if let (Some(res), Some(painter)) = (sidebar, sidebar_painter) {
        painter.invalidate();
        let _ = painter.paint(out, sidebar_rect(viewport_dims, res));
    }
    // The ED2 above cleared the bar row, so force a re-emit even if the
    // bar's content is byte-identical to the previous frame.
    let status_bar_painted = paint_bar_after_pane(
        status_bar.as_deref_mut(),
        out,
        viewport_dims,
        sidebar,
        session_name,
        None,
        None,
        true,
    );
    // Paint the focused pane LAST so its render_at owns final cursor
    // placement. But render_at may be a no-op (slot missing, or the
    // libghostty Terminal grid has no diffs to emit), in which case
    // the cursor is still wherever the bar's final write parked it —
    // bottom-right of the host terminal. Capture `paint_focused_pane`'s
    // last_cursor and always emit an explicit cursor placement so the
    // frame ends with a deterministic cursor position regardless of
    // whether render_at touched the cursor. See phux-gxy.
    let final_cursor = focused_pane.and_then(|fid| {
        let rect = multi.rects.get(fid).copied().unwrap_or(content);
        paint_focused_pane(out, rect, panes, kernel, fid, true)
    });
    // The focused pane's Rect origin is the fallback cursor parking spot when
    // `final_cursor` is None (phux-gxy/9xn). All cursor placement + the flush
    // is owned by the one composite authority.
    let fallback_origin = focused_pane
        .and_then(|fid| multi.rects.get(fid).copied())
        .map(|r| (r.x, r.y));
    let cursor_published = end_of_frame_cursor(out, final_cursor, fallback_origin).is_ok();
    let sync_ended = sync.is_some_and(|sync| sync.end(out).is_ok());
    let frame_flushed = out.flush().is_ok();
    if cursor_published && sync_ended && frame_flushed {
        status_bar_painted
    } else {
        if !matches!(status_bar_painted, StatusBarPaint::NotPublished)
            && let Some(painter) = status_bar
        {
            painter.invalidate();
        }
        StatusBarPaint::NotPublished
    }
}

/// Repaint ONLY the chrome — the sidebar strip and the status bar — in place.
///
/// The cheap counterpart to [`paint_full_frame`], and the reason a live
/// agent-state detector is not a regression. Every agent-state change (and
/// every other `chrome_dirty` event) used to route to `paint_full_frame`,
/// which leads with `ESC[2J` and force-redraws every visible pane. That was
/// survivable only because the `phux.agent/v1` state never actually changed;
/// the moment a server-side detector starts publishing transitions, the same
/// path becomes a full-screen strobe. This function is what the
/// `RepaintLevel::Chrome` drain calls instead.
///
/// The contract, mirroring the one [`paint_bar_after_pane`] already proves:
///
/// * NO `ED2` — the viewport is never cleared, so pane interiors keep whatever
///   the last content paint left on screen.
/// * NO pane render — not even the focused pane. `panes` is taken by shared
///   reference precisely so this is unrepresentable; we only READ the focused
///   renderer's cached `last_cursor` so the frame can end where it began.
/// * NO cache invalidation. [`paint_full_frame`] calls
///   `SidebarPainter::invalidate` only because its own `ED2` physically wiped
///   the strip's cells. Invalidating here would re-emit the entire strip on
///   every tick and throw away the zero-byte no-op the painter's content cache
///   exists to provide — an unchanged strip must cost nothing.
///
/// Order is load-bearing: the sidebar paint moves the host cursor into the
/// strip, so the bar row is emitted next and this function ALWAYS ends in its
/// own [`end_of_frame_cursor`], which puts the cursor back at the focused
/// pane's authoritative position (ADR-0020 invariant 4 / ADR-0029).
///
/// The cursor tail is emitted here, NOT delegated to [`paint_bar_after_pane`]:
/// that function early-returns when there is no [`StatusBarPainter`], and a
/// status bar is optional (an empty widget list yields `None` — a legitimate
/// config for someone who runs the sidebar instead of a bar). Delegating would
/// strand the host cursor wherever the sidebar strip's last cell left it, on
/// every agent-state transition, for a bar-less config.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors paint_full_frame's chrome context minus the pane map's mutability; same arg-list refactor follow-up"
)]
pub(super) fn paint_chrome_in_place<W: super::RenderSink>(
    out: &mut W,
    layout_state: &LayoutState,
    panes: &HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
    viewport_dims: (u16, u16),
    mut status_bar: Option<&mut StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    sidebar_painter: Option<&mut crate::render::chrome::sidebar::SidebarPainter>,
    session_name: &str,
    theme: &crate::render::theme::Theme,
) -> StatusBarPaint {
    let _paint = tracing::debug_span!(
        "paint_chrome_in_place",
        cols = viewport_dims.0,
        rows = viewport_dims.1,
    )
    .entered();
    let _timed = crate::perf::PAINT_CHROME.timer();
    let bar = status_bar.as_ref().map(|p| p.position());
    let ContentLayout {
        rect: content,
        rail,
    } = content_layout(viewport_dims, bar, sidebar);
    let multi = super::multi_pane::compute_layout_in(layout_state, content, viewport_dims);
    // The focused pane's LAST authoritative cursor — read, never re-derived by
    // a render. `None` (hidden / not yet rendered) falls back to the pane's
    // rect origin, hidden, exactly as every other paint tail does.
    let restore = focused_pane
        .and_then(|fid| panes.get(fid))
        .and_then(|slot| slot.renderer.last_cursor());
    let fallback = focused_pane
        .and_then(|fid| multi.rects.get(fid))
        .map(|r| (r.x, r.y));
    // Sidebar/status painters flush independently. Publish their updates and
    // final cursor restoration as one outer-terminal transaction.
    let sync = SyncOutput::begin(out).ok();
    // phux-l96p.8: the pane grid's rules and TITLES are chrome, and a
    // pane title changing is exactly a `RepaintLevel::Chrome` event
    // (`handler`'s `chrome_dirty`). Repainting the grid here is what
    // makes a title land without waiting for the next full frame; it
    // costs only the chrome cells, and the skip-cell carve-out means it
    // still cannot touch a pane interior.
    let _ = crate::render::chrome::dividers::render_dividers(
        out,
        &multi,
        content,
        rail,
        focused_pane,
        theme,
        |id| super::pane_state::pane_label(panes, id),
    );
    if let (Some(res), Some(painter)) = (sidebar, sidebar_painter) {
        let _ = painter.paint(out, sidebar_rect(viewport_dims, res));
    }
    // `bar_row_clobbered = false`: nothing cleared the bar row, so the
    // painter's cache decides. Skipped entirely when the config has no bar.
    let status_bar_painted =
        status_bar
            .as_deref_mut()
            .map_or(StatusBarPaint::NotPublished, |painter| {
                paint_bar_row(
                    painter,
                    out,
                    viewport_dims,
                    sidebar,
                    session_name,
                    false,
                    ComposePolicy::Always,
                )
            });
    // The sole CUP + DECTCEM + flush authority for this paint, reached on EVERY
    // path — bar or no bar. The sidebar's own emit parks the host cursor at the
    // end of the last strip row, so an early return here leaves the user's
    // cursor sitting in the strip until the next pane render (never, for an
    // idle pane).
    let cursor_flushed = end_of_frame_cursor(out, restore, fallback).is_ok();
    let sync_ended = sync.is_some_and(|sync| sync.end(out).is_ok());
    let frame_flushed = out.flush().is_ok();
    if cursor_flushed && sync_ended && frame_flushed {
        status_bar_painted
    } else {
        if !matches!(status_bar_painted, StatusBarPaint::NotPublished)
            && let Some(painter) = status_bar
        {
            painter.invalidate();
        }
        StatusBarPaint::NotPublished
    }
}

/// phux-nz4.5: shared helper invoked after every pane render so the
/// status row is restored on top of whatever VT the pane renderer just
/// wrote. No-op when there is no painter or no live viewport.
///
/// `restore_cursor` is the renderer's last authoritative cursor
/// position (outer-viewport coords); when present we CUP+show there.
///
/// `fallback_origin` is the focused pane's `Rect` origin to use when
/// `restore_cursor` is `None` (phux-9xn). Without this, the bar's
/// final write strands the host terminal's cursor at the end of the
/// bar row — i.e. bottom-right of the screen. The fallback emits a
/// CUP into the pane area + `?25l` so the cursor sits in a sane
/// location and is hidden until the next authoritative render
/// places it. We hide rather than show because `last_cursor == None`
/// means libghostty's snapshot either reported the cursor hidden or
/// had no viewport position — in both cases showing the cursor at an
/// arbitrary fallback position would lie to the user.
///
/// Pass `fallback_origin = None` at call sites where a subsequent
/// pane render is guaranteed to own final cursor placement (e.g.
/// `paint_full_frame`, which paints the focused pane LAST).
///
/// `bar_row_clobbered` controls whether the painter's content cache is
/// bypassed. Pane rendering is confined to the rows ABOVE the reserved
/// bar row (see [`pane_viewport`]), so on the steady-state hot path
/// (`TERMINAL_OUTPUT`) the focused pane render never overwrites the bar
/// row — the painter's own cache then makes an unchanged bar a zero-byte
/// no-op (the win in phux's incremental-paint pass). Pass `true` only
/// from callers that physically cleared the bar row (the `paint_full_frame`
/// `ED2`), where the on-screen row must be re-emitted even if its content
/// is identical to last frame.
#[allow(
    clippy::too_many_arguments,
    reason = "phux-qtw8 adds the sidebar reservation so the bar can inset out of the strip's columns; same arg-list refactor follow-up as paint_full_frame / paint_focused_pane"
)]
pub(super) fn paint_bar_after_pane<W: Write>(
    status_bar: Option<&mut StatusBarPainter>,
    out: &mut W,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &str,
    restore_cursor: Option<(u16, u16)>,
    fallback_origin: Option<(u16, u16)>,
    bar_row_clobbered: bool,
) -> StatusBarPaint {
    let Some(painter) = status_bar else {
        // phux-l96p.2: the pane renderer no longer flushes on its own (one
        // flush per composite frame). With no bar there is no
        // `end_of_frame_cursor` below to own it, so this early return is the
        // frame's end and must publish what the pane painted.
        let _ = out.flush();
        return StatusBarPaint::NotPublished;
    };
    let status_bar_painted = paint_bar_row(
        painter,
        out,
        viewport_dims,
        sidebar,
        session_name,
        bar_row_clobbered,
        ComposePolicy::Always,
    );
    // After the bar repaints, the cursor sits on the bar row. Put it
    // back at the focused pane's known position when we have one;
    // otherwise fall back to the focused pane's Rect origin (hidden)
    // so the cursor doesn't remain stranded at the bar's tail —
    // bottom-right of the host terminal. See phux-9xn.
    // All cursor placement (restore / fallback / safety-net) and the
    // load-bearing flush are owned by the one composite authority (ADR-0029).
    let cursor_flushed = end_of_frame_cursor(out, restore_cursor, fallback_origin).is_ok();
    if cursor_flushed {
        status_bar_painted
    } else {
        if !matches!(status_bar_painted, StatusBarPaint::NotPublished) {
            painter.invalidate();
        }
        StatusBarPaint::NotPublished
    }
}

/// Emit the status-bar row and NOTHING else — no cursor placement, no flush.
///
/// The shared body of [`paint_bar_after_pane`] and [`paint_chrome_in_place`].
/// It exists so the cursor tail is a decision of the CALLER: the bar is
/// optional, and a caller whose earlier emits moved the host cursor (the
/// sidebar strip) must own its `end_of_frame_cursor` whether or not a bar
/// exists. See [`paint_bar_after_pane`] for `bar_row_clobbered`.
pub(super) fn paint_bar_row<W: Write>(
    painter: &mut StatusBarPainter,
    out: &mut W,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &str,
    bar_row_clobbered: bool,
    compose: ComposePolicy,
) -> StatusBarPaint {
    let inset = bar_inset(viewport_dims, sidebar);
    if viewport_dims.1 == 0 || inset.span(viewport_dims.0).1 == 0 {
        return StatusBarPaint::NotPublished;
    }
    // Force a re-emit only when the bar row was physically overwritten
    // (e.g. the full-frame `ED2`). On the incremental path the pane
    // render stays above the bar row, so the painter's content/dims
    // cache decides: an unchanged bar emits zero bytes.
    if bar_row_clobbered {
        painter.invalidate();
    }
    match painter.paint_outcome(
        out,
        // phux-qtw8: yield the sidebar's columns so the window tabs start
        // beside the strip, not underneath it.
        inset,
        viewport_dims.0,
        viewport_dims.1,
        // The window list is owned by the painter and injected inside
        // `paint`; this context carries none.
        &make_context(session_name, SystemTime::now()),
        compose,
    ) {
        Ok(true) => StatusBarPaint::Published {
            cols: inset.span(viewport_dims.0).1,
        },
        Ok(false) | Err(_) => StatusBarPaint::NotPublished,
    }
}

/// Close a composited frame with its chrome tail: the status-bar row, then the
/// ONE cursor placement, then the block epilogue and its single flush.
///
/// The counterpart to [`FrameBlock::begin`], and the reason the bar is emitted
/// through [`paint_bar_row`] rather than [`paint_bar_after_pane`]: the latter
/// owns a cursor tail of its own, which would place the cursor before the
/// frame is finished and, on a frame where nothing changed, force bytes onto
/// the wire to say so.
///
/// A frame in which nothing was emitted closes to nothing at all — no cursor
/// tail, no `?2026l`, no flush, no writer-thread wake. That is the idle path:
/// the host cursor is already where the previous frame left it.
///
/// On a failed close the painter's cache is invalidated and the frame reports
/// `NotPublished`, so the bar re-emits next time rather than trusting a cache
/// that describes bytes the terminal may never have received.
#[allow(
    clippy::too_many_arguments,
    reason = "the frame tail's context: block, painter, geometry, sidebar, session, cursor, compose policy; same arg-list refactor follow-up as paint_full_frame"
)]
pub(super) fn close_frame_with_chrome<W: Write>(
    mut block: FrameBlock<'_, W>,
    mut status_bar: Option<&mut StatusBarPainter>,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &str,
    cursor: Option<(u16, u16)>,
    fallback_origin: Option<(u16, u16)>,
    compose: ComposePolicy,
) -> StatusBarPaint {
    let painted = status_bar
        .as_deref_mut()
        .map_or(StatusBarPaint::NotPublished, |painter| {
            // `bar_row_clobbered = false`: pane rendering is confined to the
            // rows above the reserved bar row, so the painter's own content
            // cache decides whether anything is owed.
            paint_bar_row(
                painter,
                &mut block,
                viewport_dims,
                sidebar,
                session_name,
                false,
                compose,
            )
        });
    let cursor_placed = if block.opened() {
        end_of_frame_cursor(&mut block, cursor, fallback_origin).is_ok()
    } else {
        true
    };
    if cursor_placed && block.end().is_ok() {
        return painted;
    }
    if !matches!(painted, StatusBarPaint::NotPublished)
        && let Some(painter) = status_bar
    {
        painter.invalidate();
    }
    StatusBarPaint::NotPublished
}

/// Effective viewport available to pane rendering: outer dims with the
/// status-bar row reserved when a bar is present.
///
/// Equivalent to `content_rect(outer, has_status_bar, None)`'s `(w, h)` —
/// the no-sidebar content rect is anchored at `(0, 0)` with these dims, which
/// is what keeps the disabled path byte-identical to the pre-sidebar tiling.
/// phux-4h5a converted every production call site to `content_rect`, so this
/// now survives only as the reference half of the disabled-path invariant test
/// [`tests::content_rect_disabled_equals_pane_viewport_rect`].
#[cfg_attr(not(test), allow(dead_code, reason = "test-only invariant reference"))]
pub(super) const fn pane_viewport(outer: (u16, u16), has_status_bar: bool) -> (u16, u16) {
    if has_status_bar {
        (outer.0, outer.1.saturating_sub(1))
    } else {
        outer
    }
}

/// Which edge a reserved sidebar strip docks to. Mirrors
/// [`phux_config::SidebarPosition`]; kept local so `paint`'s geometry doesn't
/// depend on the config crate's enum directly (the driver maps one to the
/// other).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SidebarEdge {
    /// Dock on the left; panes tile to its right.
    Left,
    /// Dock on the right; panes tile to its left.
    Right,
}

/// A chrome-region reservation for the window sidebar (phux-4h5a): `width`
/// columns reserved on `edge`. The driver builds this from `[sidebar]` config
/// each frame (`None` when the sidebar is disabled) and threads the SAME value
/// to every layout site so panes, dividers, reflow, mouse, and predict agree
/// on the inset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SidebarReservation {
    /// The edge the strip docks to.
    pub edge: SidebarEdge,
    /// Strip width in columns.
    pub width: u16,
}

/// Fold the sidebar's on/off state and geometry into the per-frame
/// reservation, yielding the strip when the terminal cannot afford it.
///
/// The strip costs its `width` columns off every pane, permanently. On a
/// 60-column terminal a 20-column strip is a third of the screen spent on
/// a list of window names, and the panes it is meant to help you navigate
/// between are the ones paying for it. Below `width + min_pane_cols` the
/// reservation therefore folds to `None` and the panes get the whole
/// viewport back.
///
/// `min_pane_cols` is [`crate::render::ChromeBreakpoints::min_pane_cols`] —
/// the shipped 40 unless `[chrome]` moved it (phux-huhi).
///
/// This is the single place the decision is made, so every layout site —
/// panes, dividers, reflow, mouse hit-testing, the strip painter itself —
/// receives the same answer and cannot disagree about which columns
/// belong to whom.
pub(super) fn sidebar_reservation(
    outer_cols: u16,
    enabled: bool,
    width: u16,
    edge: SidebarEdge,
    min_pane_cols: u16,
) -> Option<SidebarReservation> {
    (enabled && outer_cols >= width.saturating_add(min_pane_cols))
        .then_some(SidebarReservation { edge, width })
}

/// The residual content `Rect` panes tile into after the status bar and the
/// (optional) sidebar are folded off the outer viewport.
///
/// Height drops one row for the status bar (mirroring [`pane_viewport`]);
/// `bar` carries the bar's row so a top-docked bar (phux-foz.8) shifts the
/// content origin to `y: 1` instead of trimming the bottom. Width and
/// x-origin inset for the sidebar: a left strip pushes the origin right by
/// `width`; a right strip just narrows the width. `width` is clamped to the
/// viewport so an over-wide sidebar yields a zero-width content rect rather
/// than underflowing.
///
/// CRITICAL: with `sidebar = None` and a bottom (or absent) bar this is
/// exactly `Rect { x: 0, y: 0, w, h }` where
/// `(w, h) == pane_viewport(outer, bar.is_some())`, so
/// `compute_layout_in(ls, content_rect(outer, bar, None), outer)` is
/// byte-identical to the pre-sidebar `compute_layout(ls, pane_viewport(..))`.
pub(super) fn content_rect(
    outer: (u16, u16),
    bar: Option<Position>,
    sidebar: Option<SidebarReservation>,
) -> crate::layout::Rect {
    content_layout(outer, bar, sidebar).rect
}

/// The pane area AND the pane-grid rail row above it.
///
/// The rail row is REPORTED rather than left to be inferred from
/// `rect.y`, because `rect.y` is 1 for two unrelated reasons: a rail was
/// reserved, or a top-docked status bar pushed the content down. On a
/// viewport too short to spare a rail row those two coincide, and a
/// consumer inferring the rail would paint a rule straight across the
/// status bar's own row — over a bar that, being unchanged, never
/// repaints to correct it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ContentLayout {
    /// The rectangle panes tile into.
    pub rect: crate::layout::Rect,
    /// The row reserved above `rect` for the pane-grid rail, or `None`
    /// when the viewport was too short to spare one.
    pub rail: Option<u16>,
}

/// Split `outer` into the status-bar row, the pane-grid rail, the
/// sidebar strip, and the pane area that survives them.
pub(super) fn content_layout(
    outer: (u16, u16),
    bar: Option<Position>,
    sidebar: Option<SidebarReservation>,
) -> ContentLayout {
    let (cols, rows) = outer;
    let h = if bar.is_some() {
        rows.saturating_sub(1)
    } else {
        rows
    };
    // phux-foz.8: a top-docked bar pushes the content down one row; the
    // bottom (default) reservation keeps the pre-knob `y: 0` origin.
    let y = match bar {
        Some(Position::Top) => 1,
        Some(Position::Bottom) | None => 0,
    };
    // phux-l96p.8: one more row above the pane area for the pane-grid
    // RAIL — the rule that closes the divider grid at the top and holds
    // each top-row pane's title. It is unconditional so that every pane
    // has a rule above it to be labelled in; a rail that appeared only
    // on split windows would move the panes under the user on every
    // split. Yielded whole on a viewport too short to spare it, so a
    // two-row terminal still shows a pane instead of only chrome.
    let (y, h, rail) = if h >= 2 {
        (y + 1, h - 1, Some(y))
    } else {
        (y, h, None)
    };
    let rect = sidebar.map_or(
        crate::layout::Rect {
            x: 0,
            y,
            w: cols,
            h,
        },
        |res| {
            let width = res.width.min(cols);
            let w = cols - width;
            let x = match res.edge {
                SidebarEdge::Left => width,
                SidebarEdge::Right => 0,
            };
            crate::layout::Rect { x, y, w, h }
        },
    );
    ContentLayout { rect, rail }
}

/// The sidebar strip's own `Rect` — the columns [`content_rect`] reserved for
/// it, over the FULL viewport height. The strip docks flush to the left or
/// right outer edge per `res.edge`.
///
/// The strip owns its columns for every row, the bar row included: it is
/// [`bar_rect`] that yields, insetting the bar out of the strip's columns so
/// the window tabs never paint underneath it. The two are complementary —
/// `sidebar_rect ∪ bar_rect ∪ content_rect` tiles the viewport with no overlap
/// — and mouse routing depends on it, since `input_dispatch` hit-tests the
/// strip BEFORE the bar row and so hands the strip the corner cell the bar
/// gave up.
pub(super) const fn sidebar_rect(
    outer: (u16, u16),
    res: SidebarReservation,
) -> crate::layout::Rect {
    let (cols, rows) = outer;
    // `Ord::min` is not const for u16.
    let width = if res.width < cols { res.width } else { cols };
    let x = match res.edge {
        SidebarEdge::Left => 0,
        SidebarEdge::Right => cols - width,
    };
    crate::layout::Rect {
        x,
        y: 0,
        w: width,
        h: rows,
    }
}

/// Columns the status bar yields at each edge so it does not paint under a
/// docked sidebar (phux-qtw8): the strip is full-height, so the bar shrinks to
/// [`content_rect`]'s horizontal extent rather than spanning the full width.
///
/// `BarInset::NONE` with the sidebar disabled, which keeps the bar row
/// byte-identical to the pre-sidebar paint.
pub(super) fn bar_inset(outer: (u16, u16), sidebar: Option<SidebarReservation>) -> BarInset {
    sidebar.map_or(BarInset::NONE, |res| {
        let width = res.width.min(outer.0);
        match res.edge {
            SidebarEdge::Left => BarInset {
                left: width,
                right: 0,
            },
            SidebarEdge::Right => BarInset {
                left: 0,
                right: width,
            },
        }
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::attach::render::SYNC_OUTPUT_END;
    use crate::render::ChromeBreakpoints;

    fn leaf_layout(id: &TerminalId) -> LayoutState {
        LayoutState {
            tree: Some(crate::layout::LayoutNode::Leaf(id.clone())),
            focus: Some(id.clone()),
        }
    }

    /// The whole point of the cache: an unchanged layout tiles ONCE, however
    /// many times the frame asks for a rect. The `TERMINAL_OUTPUT` path used
    /// to run `compute_layout_in` three times per frame — mirror sizing, the
    /// focused pane's rect, the cursor-fallback origin — for a layout that
    /// only moves when the user splits, resizes, zooms, or switches windows.
    #[test]
    fn an_unchanged_layout_tiles_once_however_often_it_is_read() {
        let id = TerminalId::Local { id: 1 };
        let layout = leaf_layout(&id);
        let content = crate::layout::Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 23,
        };
        let mut cache = LayoutCache::default();
        for _ in 0..16 {
            let rect = cache
                .get(&layout, content, (80, 24))
                .rects
                .get(&id)
                .copied();
            assert_eq!(rect, Some(content), "the single leaf fills the content");
        }
        assert_eq!(cache.misses, 1, "sixteen reads, one tiling");
    }

    /// Every part of the key must invalidate: the layout tree (a split), the
    /// content rect (a sidebar toggle or a bar moving), and the viewport (a
    /// SIGWINCH). Validation is structural equality, so a hit is exact —
    /// there is no collision that could paint a pane at a stale rect.
    #[test]
    fn every_component_of_the_key_forces_a_retile() {
        let id = TerminalId::Local { id: 1 };
        let other = TerminalId::Local { id: 2 };
        let layout = leaf_layout(&id);
        let content = crate::layout::Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 23,
        };
        let mut cache = LayoutCache::default();
        let _ = cache.get(&layout, content, (80, 24));
        assert_eq!(cache.misses, 1);

        // A different tree.
        let _ = cache.get(&leaf_layout(&other), content, (80, 24));
        assert_eq!(cache.misses, 2, "a changed layout retiles");

        // A different content rect. Every reservation `content_rect` makes
        // shows up here — a sidebar appearing, a top-docked bar, the
        // pane-grid rail — so none of them can leave a stale tiling behind.
        let inset = crate::layout::Rect {
            x: 20,
            y: 0,
            w: 60,
            h: 23,
        };
        let _ = cache.get(&leaf_layout(&other), inset, (80, 24));
        assert_eq!(cache.misses, 3, "a changed content rect retiles");

        // A different viewport (SIGWINCH).
        let _ = cache.get(&leaf_layout(&other), inset, (100, 30));
        assert_eq!(cache.misses, 4, "a changed viewport retiles");

        // Back to an already-seen key: still a miss, because the cache holds
        // exactly one entry — but the rect is correct, which is what matters.
        let rect = cache
            .get(&leaf_layout(&other), inset, (100, 30))
            .rects
            .get(&other)
            .copied();
        assert_eq!(cache.misses, 4, "re-reading the current key is free");
        assert_eq!(rect, Some(inset));
    }

    fn published_kernel(
        terminals: &[TerminalId],
        cols: u16,
        rows: u16,
        replay: &[u8],
    ) -> AttachKernel {
        use phux_client_core::session::{
            EffectBuffer as KernelEffectBuffer, KernelInput, SessionKernel,
        };
        use phux_protocol::{
            BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, StreamId,
        };

        let mut kernel = SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(BootstrapLimits::default()),
            BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = KernelEffectBuffer::new();
        kernel
            .update(
                KernelInput::AttachStarted {
                    attach_id: 1,
                    terminals,
                },
                &mut effects,
            )
            .expect("attach");
        for (index, terminal_id) in terminals.iter().enumerate() {
            let stream_id = StreamId::new(1).expect("stream");
            let bootstrap_id = BootstrapId::new(index as u64 + 1).expect("bootstrap");
            kernel
                .update(
                    KernelInput::BootstrapBegin {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        profile: BootstrapStreamProfile::SynthesizedVtRaw,
                        geometry: phux_client_core::engine::CanonicalGeometry::new(cols, rows)
                            .expect("geometry"),
                        base_seq: 0,
                    },
                    &mut effects,
                )
                .expect("begin");
            kernel
                .update(
                    KernelInput::BootstrapChunk {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        chunk_seq: 0,
                        payload: replay,
                    },
                    &mut effects,
                )
                .expect("chunk");
            kernel
                .update(
                    KernelInput::BootstrapReady {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        history_cursor: None,
                    },
                    &mut effects,
                )
                .expect("ready");
        }
        kernel
    }

    /// The strip yields rather than squeezing the panes it exists to help
    /// you navigate between: on a 50-column terminal a 20-column sidebar
    /// would leave 30 columns of actual work.
    #[test]
    fn the_sidebar_yields_when_it_would_starve_the_panes() {
        let min_pane_cols = ChromeBreakpoints::DEFAULT.min_pane_cols;
        let res = |cols| sidebar_reservation(cols, true, 20, SidebarEdge::Left, min_pane_cols);

        // 20 + min-pane-cols(40) = 60 is the threshold.
        assert_eq!(res(59), None, "50-col terminal cannot afford the strip");
        assert_eq!(
            res(60),
            Some(SidebarReservation {
                edge: SidebarEdge::Left,
                width: 20
            })
        );
        assert!(res(200).is_some());

        // A narrower strip is affordable sooner — the rule is about what
        // is left for the panes, not about a fixed terminal size.
        assert!(sidebar_reservation(50, true, 10, SidebarEdge::Left, min_pane_cols).is_some());

        // Disabled stays disabled at every width.
        for cols in [0u16, 60, 200] {
            assert_eq!(
                sidebar_reservation(cols, false, 20, SidebarEdge::Left, min_pane_cols),
                None
            );
        }
    }

    /// phux-huhi: `[chrome] min-pane-cols` moves the yield threshold. The
    /// motivating case is a 55-column terminal whose owner would rather
    /// keep the strip than the columns it costs.
    #[test]
    fn a_lowered_min_pane_cols_keeps_the_sidebar_on_a_narrow_terminal() {
        // Shipped: 20 + 40 = 60, so 55 columns yields the strip.
        assert_eq!(
            sidebar_reservation(55, true, 20, SidebarEdge::Left, 40),
            None
        );
        // Configured down to 30: 20 + 30 = 50, so 55 keeps it.
        assert_eq!(
            sidebar_reservation(55, true, 20, SidebarEdge::Left, 30),
            Some(SidebarReservation {
                edge: SidebarEdge::Left,
                width: 20
            })
        );
        // And a raised floor takes it away from a terminal that used to
        // afford it.
        assert_eq!(
            sidebar_reservation(70, true, 20, SidebarEdge::Left, 60),
            None
        );
    }

    /// However narrow the terminal, the panes are never handed a
    /// zero-width or negative content rect by a sidebar that overran it.
    #[test]
    fn a_reserved_sidebar_always_leaves_a_usable_content_rect() {
        let min_pane_cols = ChromeBreakpoints::DEFAULT.min_pane_cols;
        for cols in 0u16..=120 {
            let sidebar = sidebar_reservation(cols, true, 20, SidebarEdge::Left, min_pane_cols);
            let rect = content_rect((cols, 24), Some(Position::Bottom), sidebar);
            if sidebar.is_some() {
                assert!(
                    rect.w >= min_pane_cols,
                    "cols={cols} left only {} pane columns",
                    rect.w
                );
            } else {
                assert_eq!(rect.w, cols, "cols={cols}: panes get the whole width");
            }
        }
    }

    /// phux-4h5a / phux-l96p.8: the disabled-path invariant. With no
    /// sidebar the content rect is the full width, anchored one row down
    /// from where `pane_viewport` starts — that row is the pane-grid
    /// RAIL, which holds every top-row pane's title. Height is
    /// `pane_viewport`'s less the rail.
    #[test]
    fn content_rect_disabled_is_pane_viewport_less_the_rail() {
        for outer in [(80u16, 24u16), (200, 50)] {
            for bar in [None, Some(Position::Bottom)] {
                let (vw, vh) = pane_viewport(outer, bar.is_some());
                assert_eq!(
                    content_rect(outer, bar, None),
                    crate::layout::Rect {
                        x: 0,
                        y: 1,
                        w: vw,
                        h: vh - 1,
                    },
                    "outer={outer:?} bar={bar:?}"
                );
            }
        }
    }

    /// phux-l96p.8 fix pass: the rail row is REPORTED, never inferred
    /// from `rect.y`. With a top-docked bar on a two-row viewport the
    /// bar takes row 0 and `rect.y` is 1 — but no rail was reserved, and
    /// a consumer inferring `rect.y - 1` would paint a rule across the
    /// bar's own row.
    #[test]
    fn a_two_row_top_bar_reports_no_rail() {
        let two = content_layout((40, 2), Some(Position::Top), None);
        assert_eq!(two.rect.y, 1, "the bar still takes row 0");
        assert_eq!(two.rect.h, 1, "the single remaining row goes to the pane");
        assert_eq!(
            two.rail, None,
            "no rail was reserved, so `rect.y - 1` is the BAR's row"
        );
        // With room, the rail is reported and sits under the bar.
        let roomy = content_layout((40, 24), Some(Position::Top), None);
        assert_eq!(roomy.rail, Some(1));
        assert_eq!(roomy.rect.y, 2);
        // Bottom bar (the default) puts the rail at row 0.
        let bottom = content_layout((40, 24), Some(Position::Bottom), None);
        assert_eq!(bottom.rail, Some(0));
        assert_eq!(bottom.rect.y, 1);
        // And a one-row viewport reserves nothing at all.
        let tiny = content_layout((40, 1), Some(Position::Bottom), None);
        assert_eq!(tiny.rail, None);
    }

    /// `content_rect` stays the projection of `content_layout`, so the
    /// twenty-odd call sites that only want the rectangle cannot drift
    /// from the one that also needs the rail.
    #[test]
    fn content_rect_is_content_layouts_rect() {
        for outer in [(80u16, 24u16), (40, 2), (10, 1), (200, 50)] {
            for bar in [None, Some(Position::Top), Some(Position::Bottom)] {
                assert_eq!(
                    content_rect(outer, bar, None),
                    content_layout(outer, bar, None).rect,
                    "outer={outer:?} bar={bar:?}"
                );
            }
        }
    }

    /// phux-l96p.8: the rail costs a row only when there is a row to
    /// spare. A viewport with one usable row keeps it for the pane —
    /// chrome must never be the only thing on screen.
    #[test]
    fn a_viewport_too_short_for_a_rail_keeps_its_only_row() {
        for outer in [(10u16, 1u16), (10, 2)] {
            for bar in [None, Some(Position::Bottom)] {
                let (_, vh) = pane_viewport(outer, bar.is_some());
                let rect = content_rect(outer, bar, None);
                if vh >= 2 {
                    assert_eq!(rect.h, vh - 1, "outer={outer:?} bar={bar:?}");
                } else {
                    assert_eq!(rect.h, vh, "outer={outer:?} bar={bar:?}");
                    assert_eq!(rect.y, 0, "outer={outer:?} bar={bar:?}");
                }
            }
        }
    }

    /// phux-foz.8: a top-docked bar keeps the one-row height reservation but
    /// shifts the content origin down to row 1, so panes never underlap the
    /// bar row. The sidebar inset composes with the shift unchanged.
    #[test]
    fn content_rect_top_bar_shifts_origin_down_one_row() {
        let outer = (80, 24);
        // One row for the bar, one for the rail below it.
        assert_eq!(
            content_rect(outer, Some(Position::Top), None),
            crate::layout::Rect {
                x: 0,
                y: 2,
                w: 80,
                h: 22,
            }
        );
        // Composes with a left sidebar: x inset and y shift together.
        assert_eq!(
            content_rect(
                outer,
                Some(Position::Top),
                Some(SidebarReservation {
                    edge: SidebarEdge::Left,
                    width: 20,
                }),
            ),
            crate::layout::Rect {
                x: 20,
                y: 2,
                w: 60,
                h: 22,
            }
        );
        // Degenerate 1-row viewport: the reservation empties the content
        // rect without underflowing, and no rail is taken from nothing.
        let tiny = content_rect((10, 1), Some(Position::Top), None);
        assert_eq!(tiny.h, 0);
        assert_eq!(tiny.y, 1);
    }

    /// A left sidebar pushes the content origin right by `width` and narrows
    /// the width; a right sidebar leaves the origin at 0 and just narrows.
    /// Height tracks the status-bar reservation in both cases.
    #[test]
    fn content_rect_insets_for_left_and_right_sidebar() {
        let outer = (80, 24);
        // No bar, left dock, width 20: x = 20, w = 60, h = 23 (rail).
        let left = content_rect(
            outer,
            None,
            Some(SidebarReservation {
                edge: SidebarEdge::Left,
                width: 20,
            }),
        );
        assert_eq!(
            left,
            crate::layout::Rect {
                x: 20,
                y: 1,
                w: 60,
                h: 23,
            }
        );
        // With bar, right dock, width 20: x = 0, w = 60, h = 22 (bar + rail).
        let right = content_rect(
            outer,
            Some(Position::Bottom),
            Some(SidebarReservation {
                edge: SidebarEdge::Right,
                width: 20,
            }),
        );
        assert_eq!(
            right,
            crate::layout::Rect {
                x: 0,
                y: 1,
                w: 60,
                h: 22,
            }
        );
        // An over-wide sidebar clamps to the viewport: zero content width, no
        // underflow.
        let huge = content_rect(
            outer,
            None,
            Some(SidebarReservation {
                edge: SidebarEdge::Left,
                width: 999,
            }),
        );
        assert_eq!(huge.w, 0);
        assert_eq!(huge.x, 80);
    }

    /// phux-qtw8: the strip docks flush to the outer edge, spans `width`
    /// columns, and runs the FULL viewport height — the bar row included. It is
    /// the bar that yields (see [`bar_inset`]), so the strip's height no longer
    /// depends on whether a bar is docked, or where.
    #[test]
    fn sidebar_rect_is_full_height_regardless_of_the_bar() {
        let outer = (80, 24);
        let left = sidebar_rect(
            outer,
            SidebarReservation {
                edge: SidebarEdge::Left,
                width: 20,
            },
        );
        assert_eq!(
            left,
            crate::layout::Rect {
                x: 0,
                y: 0,
                w: 20,
                h: 24,
            }
        );
        let right = sidebar_rect(
            outer,
            SidebarReservation {
                edge: SidebarEdge::Right,
                width: 20,
            },
        );
        assert_eq!(
            right,
            crate::layout::Rect {
                x: 60,
                y: 0,
                w: 20,
                h: 24,
            }
        );
    }

    /// phux-qtw8: the bar yields exactly the strip's columns, so the window tabs
    /// start beside the sidebar instead of painting underneath it. Its span is
    /// the content rect's horizontal extent — the two agree by construction.
    #[test]
    fn bar_inset_yields_the_sidebar_columns() {
        let outer = (80, 24);
        assert_eq!(bar_inset(outer, None), BarInset::NONE);

        let left = SidebarReservation {
            edge: SidebarEdge::Left,
            width: 20,
        };
        assert_eq!(
            bar_inset(outer, Some(left)),
            BarInset { left: 20, right: 0 }
        );
        let right = SidebarReservation {
            edge: SidebarEdge::Right,
            width: 20,
        };
        assert_eq!(
            bar_inset(outer, Some(right)),
            BarInset { left: 0, right: 20 }
        );

        // The bar and the panes occupy the same columns: whatever the edge,
        // `bar_inset`'s span IS `content_rect`'s (x, w).
        for res in [left, right] {
            let content = content_rect(outer, Some(Position::Bottom), Some(res));
            let span = bar_inset(outer, Some(res)).span(outer.0);
            assert_eq!(span, (content.x, content.w), "edge {:?}", res.edge);
        }

        // Over-wide strip: the bar has nowhere to paint rather than underflowing.
        let huge = SidebarReservation {
            edge: SidebarEdge::Left,
            width: 999,
        };
        assert_eq!(bar_inset(outer, Some(huge)).span(outer.0).1, 0);
    }

    /// ADR-0029: the one composite cursor emitter resolves the three-way
    /// None-fallback policy and always ends with a flush. Pins the byte output
    /// for each case (the cursor-matrix the phux-gxy/9xn/b9n scars chased).
    #[test]
    fn end_of_frame_cursor_resolves_all_three_cases() {
        // Some(cursor) -> CUP(row,col) + show. (2,4) 0-based -> CUP 3;5.
        let mut out = Vec::new();
        end_of_frame_cursor(&mut out, Some((2, 4)), None).expect("write");
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[3;5H\x1b[?25h");

        // None + fallback origin (x=3, y=5) -> CUP(y,x)=6;4 + hide.
        let mut out = Vec::new();
        end_of_frame_cursor(&mut out, None, Some((3, 5))).expect("write");
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[6;4H\x1b[?25l");

        // None + None -> safety net: viewport origin, hidden.
        let mut out = Vec::new();
        end_of_frame_cursor(&mut out, None, None).expect("write");
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[1;1H\x1b[?25l");
    }

    use crate::attach::pane_state::PaneSlot;
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};
    use phux_protocol::wire::info::{LayoutNode, SplitDir};

    fn build_painter() -> StatusBarPainter {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let reg = WidgetRegistry::with_builtins();
        let bar = phux_config::widget::StatusBar::build(&cfg, &reg).expect("bar build");
        StatusBarPainter::new(bar, Position::Bottom)
    }

    /// A realistic large-viewport truecolor repaint is BIGGER THAN THE
    /// STDOUT SINK'S BACKLOG CAP, in one chunk.
    ///
    /// This is the size premise behind
    /// `stdout_writer::an_oversized_frame_on_an_empty_queue_is_written_not_dropped`,
    /// asserted here so the two cannot drift apart. Once the renderer stopped
    /// flushing per pane (phux-l96p.2) and the composite frame stopped
    /// flushing per component (phux-l96p.3), a full repaint became ONE chunk
    /// handed to `StdoutSink` — and the sink's cap used to reject any chunk
    /// that did not fit alongside the queue, which at this size meant
    /// rejecting it even when the queue was empty. That froze the screen after
    /// every full repaint at a large viewport.
    ///
    /// Measured on a grid small enough to bootstrap in one chunk
    /// (`DEFAULT_BOOTSTRAP_CHUNK_BYTES` is 256 KiB and this content costs
    /// about as many bytes going in as coming out), then extrapolated to the
    /// 250x70 a full-screen terminal actually is. The content is one 24-bit
    /// foreground and background per cell, which is what a half-block image
    /// renderer (`chafa`, `timg`) or a gradient dashboard (`btop`) emits.
    #[test]
    fn a_large_truecolor_full_frame_exceeds_the_sink_backlog_cap() {
        const COLS: u16 = 100;
        const ROWS: u16 = 40;
        /// The viewport the freeze was reported at.
        const REAL_COLS: usize = 250;
        const REAL_ROWS: usize = 70;

        let pane = TerminalId::local(1);
        let layout = LayoutState {
            tree: Some(LayoutNode::Leaf(pane.clone())),
            focus: Some(pane.clone()),
        };
        // Every cell a different fg+bg, so the renderer's SGR delta cannot
        // coalesce runs — the pathological-but-real case.
        let mut vt = Vec::new();
        for row in 0..ROWS {
            vt.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
            for col in 0..COLS {
                let r = row.wrapping_mul(3).wrapping_add(col) % 256;
                let g = col.wrapping_mul(7).wrapping_add(row) % 256;
                let b = row.wrapping_mul(col) % 256;
                vt.extend_from_slice(
                    format!("\x1b[38;2;{r};{g};{b}m\x1b[48;2;{b};{r};{g}m\u{2580}").as_bytes(),
                );
            }
        }
        let kernel = published_kernel(std::slice::from_ref(&pane), COLS, ROWS, &vt);
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(
            pane.clone(),
            PaneSlot::new_with_size(COLS, ROWS).expect("slot"),
        );

        let mut out: Vec<u8> = Vec::new();
        paint_full_frame(
            &mut out,
            &layout,
            &mut panes,
            &kernel,
            Some(&pane),
            (COLS, ROWS),
            None,
            None,
            None,
            "demo",
            &crate::render::theme::Theme::default(),
        );

        let cells = usize::from(COLS) * usize::from(ROWS);
        let per_cell = out.len() / cells;
        assert!(
            per_cell >= 20,
            "a truecolor cell costs about 40 bytes to emit; got {per_cell} \
             from {} bytes over {cells} cells — if this collapsed, the \
             renderer changed and the sink's sizing assumptions want revisiting",
            out.len()
        );
        let full_screen = per_cell * REAL_COLS * REAL_ROWS;
        assert!(
            full_screen > crate::attach::stdout_writer::CAP_BYTES,
            "a {REAL_COLS}x{REAL_ROWS} truecolor repaint is ~{full_screen} \
             bytes in ONE chunk, which must exceed the sink's \
             {} byte backlog cap — that is the case the cap used to reject \
             outright, freezing the screen",
            crate::attach::stdout_writer::CAP_BYTES
        );
    }

    /// `paint_full_frame` against an injected `Vec<u8>` sink composites
    /// the whole frame for a two-pane layout: it wraps ED2 + home and all
    /// component writes in synchronized output,
    /// emits both panes' rect-anchored content, draws the divider, and
    /// ends with an explicit cursor placement. Locks the full-frame
    /// composition contract on the now-injectable sink (phux-549).
    #[test]
    fn paint_full_frame_composites_two_panes_into_sink() {
        let left = TerminalId::local(1);
        let right = TerminalId::local(2);
        let layout = LayoutState {
            tree: Some(LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(left.clone())),
                right: Box::new(LayoutNode::Leaf(right.clone())),
            }),
            focus: Some(left.clone()),
        };
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(left.clone(), PaneSlot::new().expect("left slot"));
        panes.insert(right, PaneSlot::new().expect("right slot"));
        let kernel = published_kernel(&[left.clone(), TerminalId::local(2)], 80, 24, b"");

        let mut out: Vec<u8> = Vec::new();
        paint_full_frame(
            &mut out,
            &layout,
            &mut panes,
            &kernel,
            Some(&left),
            (80, 24),
            None,
            None,
            None,
            "demo",
            &crate::render::theme::Theme::default(),
        );

        let s = String::from_utf8_lossy(&out);
        // The destructive clear and every component write are one outer
        // terminal transaction, so nested flushes cannot expose a blank or
        // partially rebuilt frame.
        assert!(
            s.starts_with("\x1b[?2026h\x1b[2J\x1b[H"),
            "frame must open a synchronized ED2 transaction; out = {s:?}"
        );
        assert!(
            s.ends_with("\x1b[?2026l"),
            "frame must close synchronized output; out = {s:?}"
        );
        // The divider for a 0.5 side-by-side split sits at column 40
        // (1-based 41). render_dividers emits CUPs into that column.
        assert!(
            s.contains(";41H") || s.contains(";40H"),
            "expected a divider CUP near the split column; out = {s:?}"
        );
        // The frame ends with an explicit cursor placement (CUP + DECTCEM)
        // — never stranded at the bar tail (phux-gxy).
        assert!(
            s.contains("\x1b[?25h") || s.contains("\x1b[?25l"),
            "frame must end with an explicit cursor visibility; out = {s:?}"
        );
    }

    struct TailFailSink {
        fail_sync_end: bool,
        fail_final_flush: bool,
        sync_end_seen: bool,
    }

    impl Write for TailFailSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf == SYNC_OUTPUT_END {
                self.sync_end_seen = true;
                if self.fail_sync_end {
                    return Err(std::io::Error::other("sync end failed"));
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_final_flush && self.sync_end_seen {
                return Err(std::io::Error::other("final flush failed"));
            }
            Ok(())
        }
    }

    fn paint_full_frame_with_tail_failure(
        fail_sync_end: bool,
        fail_final_flush: bool,
    ) -> StatusBarPaint {
        let id = TerminalId::local(1);
        let layout = LayoutState {
            tree: Some(LayoutNode::Leaf(id.clone())),
            focus: Some(id.clone()),
        };
        let mut panes = HashMap::from([(id.clone(), PaneSlot::new().expect("pane"))]);
        let kernel = published_kernel(std::slice::from_ref(&id), 80, 24, b"");
        let mut painter = build_painter();
        let mut out = TailFailSink {
            fail_sync_end,
            fail_final_flush,
            sync_end_seen: false,
        };

        paint_full_frame(
            &mut out,
            &layout,
            &mut panes,
            &kernel,
            Some(&id),
            (80, 24),
            Some(&mut painter),
            None,
            None,
            "demo",
            &crate::render::theme::Theme::default(),
        )
    }

    #[test]
    fn paint_full_frame_does_not_publish_bar_when_sync_end_fails() {
        assert_eq!(
            paint_full_frame_with_tail_failure(true, false),
            StatusBarPaint::NotPublished
        );
    }

    #[test]
    fn paint_full_frame_does_not_publish_bar_when_final_flush_fails() {
        assert_eq!(
            paint_full_frame_with_tail_failure(false, true),
            StatusBarPaint::NotPublished
        );
    }

    /// phux-9xn regression: when `restore_cursor` is None (e.g. fresh
    /// attach before any PTY output, or hidden cursor) and a
    /// `fallback_origin` is provided, the helper must emit a CUP into
    /// the focused pane's rect origin plus `?25l` so the host
    /// terminal's cursor doesn't strand at the end of the bar row.
    #[test]
    fn paint_bar_after_pane_falls_back_to_pane_origin_when_cursor_none() {
        let mut painter = build_painter();
        let mut out = Vec::new();
        paint_bar_after_pane(
            Some(&mut painter),
            &mut out,
            (80, 24),
            None,
            "demo",
            None,
            Some((3, 5)),
            true,
        );
        let s = String::from_utf8_lossy(&out);
        // Pane origin (3, 5) ⇒ 1-based CUP `\x1b[6;4H`.
        assert!(s.contains("\x1b[6;4H"), "fallback CUP missing; out = {s:?}");
        // Fallback hides the cursor — we don't know if it should be
        // visible at this position.
        assert!(
            s.contains("\x1b[?25l"),
            "fallback ?25l missing; out = {s:?}"
        );
        // And we must NOT have emitted ?25h via the restore branch.
        let last_cup_idx = s.rfind("\x1b[6;4H").expect("cup present");
        let after = &s[last_cup_idx..];
        assert!(
            !after.contains("\x1b[?25h"),
            "fallback path must hide, not show cursor; trailing = {after:?}"
        );
    }

    /// Cursor-known path must continue to emit `?25h` at the
    /// authoritative position (phux-b9n regression guard).
    #[test]
    fn paint_bar_after_pane_restores_cursor_visible_when_known() {
        let mut painter = build_painter();
        let mut out = Vec::new();
        paint_bar_after_pane(
            Some(&mut painter),
            &mut out,
            (80, 24),
            None,
            "demo",
            Some((4, 7)),
            Some((0, 0)),
            true,
        );
        let s = String::from_utf8_lossy(&out);
        // (row, col) = (4, 7) ⇒ 1-based CUP `\x1b[5;8H`.
        assert!(s.contains("\x1b[5;8H"), "restore CUP missing; out = {s:?}");
        assert!(s.contains("\x1b[?25h"), "restore ?25h missing; out = {s:?}");
        // Fallback CUP for origin (0, 0) must NOT appear.
        assert!(
            !s.contains("\x1b[1;1H"),
            "fallback CUP leaked into restore path; out = {s:?}"
        );
    }

    /// When `restore_cursor` is None AND `fallback_origin` is None,
    /// the helper now parks the cursor at (0,0) hidden as a safety
    /// net. The old behavior (no CUP) stranded the cursor at the
    /// bar's last cell — bottom-right of the host terminal — when no
    /// follow-up paint owned final placement (phux-gxy).
    #[test]
    fn paint_bar_after_pane_parks_at_top_left_hidden_when_both_none() {
        let mut painter = build_painter();
        let mut out = Vec::new();
        paint_bar_after_pane(
            Some(&mut painter),
            &mut out,
            (80, 24),
            None,
            "demo",
            None,
            None,
            true,
        );
        let s = String::from_utf8_lossy(&out);
        // Bar CUP to row 24 must be present (the bar still paints).
        assert!(s.contains("\x1b[24;1H"), "bar CUP missing; out = {s:?}");
        // Safety-net CUP to (0,0) followed by hide.
        assert!(
            s.contains("\x1b[1;1H\x1b[?25l"),
            "safety-net CUP+?25l missing; out = {s:?}"
        );
        // Must NOT show cursor.
        assert!(
            !s.contains("\x1b[?25h"),
            "unexpected ?25h in both-none path; out = {s:?}"
        );
    }

    /// Incremental-paint win: on the hot path (`bar_row_clobbered = false`)
    /// a repaint whose bar content + dims are unchanged emits NO status-bar
    /// row bytes. Only the (cheap) cursor-restore CUP is written. This is
    /// the steady-state cost reduction: the prior unconditional
    /// `painter.invalidate()` re-emitted the entire bar row on every
    /// `TERMINAL_OUTPUT` frame.
    #[test]
    fn paint_bar_after_pane_skips_unchanged_bar_when_not_clobbered() {
        let mut painter = build_painter();
        // First paint primes the painter's cache (emits the bar row once).
        let mut first = Vec::new();
        paint_bar_after_pane(
            Some(&mut painter),
            &mut first,
            (80, 24),
            None,
            "demo",
            Some((4, 7)),
            None,
            false,
        );
        let first_s = String::from_utf8_lossy(&first);
        assert!(
            first_s.contains("\x1b[24;1H"),
            "first paint must emit the bar row CUP; out = {first_s:?}"
        );

        // Second paint, same dims + same widget inputs, NOT clobbered:
        // the bar row must NOT be re-emitted.
        let mut second = Vec::new();
        paint_bar_after_pane(
            Some(&mut painter),
            &mut second,
            (80, 24),
            None,
            "demo",
            Some((4, 7)),
            None,
            false,
        );
        let second_s = String::from_utf8_lossy(&second);
        assert!(
            !second_s.contains("\x1b[24;1H"),
            "unchanged bar must not re-emit its row CUP; out = {second_s:?}"
        );
        // The only bytes are the cursor restore to (4,7) ⇒ \x1b[5;8H.
        assert!(
            second_s.contains("\x1b[5;8H"),
            "cursor restore CUP still expected; out = {second_s:?}"
        );
    }

    /// Correctness guard: when the bar row WAS clobbered
    /// (`bar_row_clobbered = true`, the `paint_full_frame` ED2 path), the
    /// bar re-emits even if its content is byte-identical to the previous
    /// frame — otherwise the cleared row would stay blank.
    #[test]
    fn paint_bar_after_pane_re_emits_when_clobbered_even_if_unchanged() {
        let mut painter = build_painter();
        let mut first = Vec::new();
        paint_bar_after_pane(
            Some(&mut painter),
            &mut first,
            (80, 24),
            None,
            "demo",
            Some((4, 7)),
            None,
            true,
        );
        assert!(String::from_utf8_lossy(&first).contains("\x1b[24;1H"));

        // Same inputs, but clobbered: must force a re-emit of the bar row.
        let mut second = Vec::new();
        paint_bar_after_pane(
            Some(&mut painter),
            &mut second,
            (80, 24),
            None,
            "demo",
            Some((4, 7)),
            None,
            true,
        );
        let second_s = String::from_utf8_lossy(&second);
        assert!(
            second_s.contains("\x1b[24;1H"),
            "clobbered bar must re-emit its row even when unchanged; out = {second_s:?}"
        );
    }

    /// Build a left-docked sidebar painter primed with one window, plus its
    /// reservation, for the in-place chrome tests.
    fn build_sidebar() -> (
        crate::render::chrome::sidebar::SidebarPainter,
        SidebarReservation,
    ) {
        let mut painter =
            crate::render::chrome::sidebar::SidebarPainter::new(crate::render::Theme::default());
        painter.set_windows(vec![phux_config::widget::WindowInfo {
            name: "editor".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        }]);
        (
            painter,
            SidebarReservation {
                edge: SidebarEdge::Left,
                width: 20,
            },
        )
    }

    /// THE anti-regression contract for the agent-state detector: the in-place
    /// chrome paint must never emit `ED2` and never re-render a pane interior.
    /// Routing the (now live) `agent_meta_changed` arm at `paint_full_frame`
    /// would clear the screen on every state transition — a full-screen strobe.
    #[test]
    fn paint_chrome_in_place_never_clears_the_viewport_or_repaints_a_pane() {
        let id = TerminalId::local(1);
        let layout = LayoutState {
            tree: None,
            focus: Some(id.clone()),
        };
        let mut slot = PaneSlot::new_with_size(60, 23).expect("slot");
        // Pane content that a full-frame repaint WOULD re-emit.
        slot.terminal.vt_write(b"PANEBODY");
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), slot);

        let mut bar = build_painter();
        let (mut sidebar_painter, res) = build_sidebar();
        let mut out: Vec<u8> = Vec::new();
        paint_chrome_in_place(
            &mut out,
            &layout,
            &panes,
            Some(&id),
            (80, 24),
            Some(&mut bar),
            Some(res),
            Some(&mut sidebar_painter),
            "demo",
            &crate::render::theme::Theme::default(),
        );
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.starts_with("\x1b[?2026h") && s.ends_with("\x1b[?2026l"),
            "chrome components must publish as one transaction; out = {s:?}"
        );
        assert!(
            !s.contains("\x1b[2J"),
            "in-place chrome must never clear the viewport; out = {s:?}"
        );
        assert!(
            !s.contains("PANEBODY"),
            "in-place chrome must not re-render a pane interior; out = {s:?}"
        );
        // It still ends with the one composite cursor authority (ADR-0029).
        assert!(
            s.contains("\x1b[?25h") || s.contains("\x1b[?25l"),
            "frame must end with an explicit cursor visibility; out = {s:?}"
        );
        // And the strip itself painted: its second row CUPs to column 1.
        assert!(
            s.contains("\x1b[2;1H"),
            "sidebar strip rows must be emitted; out = {s:?}"
        );
    }

    /// The painter's content cache must survive the in-place path: an
    /// unchanged strip is a ZERO-byte no-op. (`paint_full_frame` invalidates
    /// only because its own ED2 wiped the cells.) With a detector ticking at
    /// up to 10 Hz per pane, re-emitting the whole strip on every unchanged
    /// chrome raise is exactly the cost this path exists to avoid.
    #[test]
    fn paint_chrome_in_place_keeps_the_sidebar_cache() {
        let id = TerminalId::local(1);
        let layout = LayoutState {
            tree: None,
            focus: Some(id.clone()),
        };
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), PaneSlot::new_with_size(60, 23).expect("slot"));
        let mut bar = build_painter();
        let (mut sidebar_painter, res) = build_sidebar();

        let mut first: Vec<u8> = Vec::new();
        paint_chrome_in_place(
            &mut first,
            &layout,
            &panes,
            Some(&id),
            (80, 24),
            Some(&mut bar),
            Some(res),
            Some(&mut sidebar_painter),
            "demo",
            &crate::render::theme::Theme::default(),
        );
        assert!(
            String::from_utf8_lossy(&first).contains("\x1b[2;1H"),
            "first paint primes the strip"
        );

        let mut second: Vec<u8> = Vec::new();
        paint_chrome_in_place(
            &mut second,
            &layout,
            &panes,
            Some(&id),
            (80, 24),
            Some(&mut bar),
            Some(res),
            Some(&mut sidebar_painter),
            "demo",
            &crate::render::theme::Theme::default(),
        );
        let s = String::from_utf8_lossy(&second);
        assert!(
            !s.contains("\x1b[2;1H"),
            "unchanged strip must not re-emit its rows; out = {s:?}"
        );
    }

    /// A config with a sidebar and NO status bar (an empty widget list makes
    /// `build_status_bar_painter` return `None`) must still end the frame with
    /// a cursor placement. The sidebar's own emit parks the host cursor at the
    /// end of the last strip row; with the cursor tail delegated to
    /// `paint_bar_after_pane` — which early-returns without a painter — the
    /// user's cursor was stranded in the strip's columns on every agent-state
    /// transition, until the next pane render (never, for an idle pane).
    #[test]
    fn paint_chrome_in_place_restores_the_cursor_without_a_status_bar() {
        let id = TerminalId::local(1);
        // A real leaf so the focused pane HAS a rect: with a 20-column left
        // strip and the pane-grid rail its origin is (x = 20, y = 1).
        let layout = LayoutState {
            tree: Some(LayoutNode::Leaf(id.clone())),
            focus: Some(id.clone()),
        };
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), PaneSlot::new_with_size(60, 24).expect("slot"));
        let (mut sidebar_painter, res) = build_sidebar();

        let mut out: Vec<u8> = Vec::new();
        paint_chrome_in_place(
            &mut out,
            &layout,
            &panes,
            Some(&id),
            (80, 24),
            // No status bar: the config runs the sidebar instead.
            None,
            Some(res),
            Some(&mut sidebar_painter),
            "demo",
            &crate::render::theme::Theme::default(),
        );
        let s = String::from_utf8_lossy(&out);
        // The strip painted (so the cursor really is inside it) ...
        assert!(
            s.contains("\x1b[2;1H"),
            "sidebar strip rows must be emitted; out = {s:?}"
        );
        // ... and the frame still ends in the one composite cursor authority.
        assert!(
            s.contains("\x1b[?25h") || s.contains("\x1b[?25l"),
            "bar-less chrome paint must still end with an explicit cursor \
             visibility; out = {s:?}"
        );
        // The cursor placement is the final operation inside the synchronized
        // transaction; only the publication barrier follows it.
        let tail = s
            .rfind("\x1b[?25")
            .expect("cursor visibility present in the tail");
        assert_eq!(
            &s[tail..],
            "\x1b[?25l\x1b[?2026l",
            "only the sync-output close may follow the cursor tail; out = {s:?}"
        );
        // The pane never rendered, so the fallback parks (hidden) at the
        // focused pane's rect origin — column 21, right of the 20-col
        // strip, and row 2, under the pane-grid rail.
        assert!(
            s.contains("\x1b[2;21H\x1b[?25l"),
            "cursor must park at the focused pane's origin, not in the strip; \
             out = {s:?}"
        );
    }

    /// phux-wurs: `paint_focused_pane` must NOT resize the pane's libghostty
    /// mirror to the client layout rect. The mirror grid size is
    /// server-authoritative (set only at the snapshot / resize-ack handler).
    /// Resizing the alt-screen mirror to a transient client-rect width during
    /// a resize handshake strands previous-screen content in the dropped
    /// columns (the right-side ghost), because the alternate screen does not
    /// reflow. Single-pane (`tree: None`) takes the full-viewport rect
    /// fallback, so the rect width (M) differs from the mirror width (N).
    #[test]
    fn paint_focused_pane_does_not_resize_server_authoritative_mirror() {
        use libghostty_vt::TerminalOptions;

        let id = TerminalId::local(1);
        // Single-pane: no layout tree ⇒ compute_layout yields no rect, so
        // paint_focused_pane falls back to the full pane viewport.
        let layout = LayoutState {
            tree: None,
            focus: Some(id.clone()),
        };

        // Mirror is server-authoritative at 20x4 on the ALT screen, filled
        // with full-width content (the "top-of-file" the ghost is made of).
        let mirror_cols = 20u16;
        let mirror_rows = 4u16;
        let mut slot = PaneSlot::new_with_size(mirror_cols, mirror_rows).expect("slot");
        slot.terminal.vt_write(b"\x1b[?1049h"); // enter alt screen (no reflow)
        slot.terminal
            .vt_write(b"ABCDEFGHIJKLMNOPQRST\r\nABCDEFGHIJKLMNOPQRST");
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), slot);
        let kernel = published_kernel(
            std::slice::from_ref(&id),
            mirror_cols,
            mirror_rows,
            b"\x1b[?1049hABCDEFGHIJKLMNOPQRST\r\nABCDEFGHIJKLMNOPQRST",
        );

        // Client viewport is far wider/taller than the mirror, so the rect
        // (M) disagrees with the mirror (N). With a bar, pane_dims = (80, 23).
        let viewport = (80u16, 24u16);
        let mut out: Vec<u8> = Vec::new();
        // The caller now owns the tiling: with no tree there is no rect, so
        // the content rect is what the pane paints into — exactly the
        // fallback `paint_focused_pane` used to compute for itself.
        let content = content_rect(viewport, Some(Position::Bottom), None);
        let rect = tiled_rect(&layout, content, viewport, &id).unwrap_or(content);
        let _ = paint_focused_pane(&mut out, rect, &mut panes, &kernel, &id, false);

        // The mirror grid size is unchanged — the layout rect did not resize it.
        let slot = panes.get(&id).expect("slot");
        assert_eq!(
            slot.terminal.cols().expect("cols"),
            mirror_cols,
            "focused paint must not widen the server-authoritative mirror"
        );
        assert_eq!(
            slot.terminal.rows().expect("rows"),
            mirror_rows,
            "focused paint must not grow the server-authoritative mirror"
        );

        // And the paint is clipped to the mirror's real width: no spill past
        // column 20 (the rect is 80 wide, but the mirror is only 20).
        // Reference: re-read the mirror grid and confirm the painted glyphs
        // match, with nothing beyond. A spill would emit extra glyphs from a
        // stale wider grid; here the grid is 20 wide so the clip equals the
        // mirror. The regression we guard is the resize, asserted above.
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains('A') && s.contains('T'), "content painted; {s:?}");

        // A no-grow probe via an explicit alt-screen reference: a 20-wide
        // mirror written the same way, never resized, has the identical grid.
        let mut reference = GhosttyTerminal::new(TerminalOptions {
            cols: mirror_cols,
            rows: mirror_rows,
            max_scrollback: 10_000,
        })
        .expect("reference");
        reference.vt_write(b"\x1b[?1049h");
        reference.vt_write(b"ABCDEFGHIJKLMNOPQRST\r\nABCDEFGHIJKLMNOPQRST");
        assert_eq!(
            reference.cols().expect("ref cols"),
            slot.terminal.cols().expect("slot cols"),
            "mirror width must equal the never-resized reference"
        );
    }
}
