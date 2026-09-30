//! Paint composition for the attach driver.
//!
//! `paint_full_frame` (clear and repaint everything, after layout mutations, resize, or attach), the
//! incremental `paint_focused_pane` + `paint_bar_after_pane` path, the
//! in-place chrome repaint, and the chrome geometry (`content_layout`).

use std::collections::HashMap;
use std::io::Write;
use std::time::SystemTime;

use libghostty_vt::Terminal as GhosttyTerminal;
use phux_protocol::ids::ResourceId;

use super::chrome_ctx::ChromeCtx;
use super::pane_state::{AttachKernel, PaneSlot, published_replica};
use crate::layout::LayoutState;
use crate::render::chrome::status_bar::{
    BarInset, ComposePolicy, Position, StatusBarPainter, make_context,
};

use super::render::SyncOutput;

/// Hide the cursor for the duration of a composited frame, so it never skates
/// across a half-painted grid on a terminal that ignores mode 2026.
const CURSOR_HIDE: &[u8] = b"\x1b[?25l";

/// One composited frame: a DEC 2026 block that swallows the flushes of
/// everything painted inside it and ships once, as one write and one flush.
///
/// A `Write` wrapper, so each painter keeps the flushes it needs standalone.
/// The block's depth is taken in [`Self::begin`], before any painter runs,
/// so the pane renderer's own [`SyncOutput`] nests instead of closing the
/// frame early. The prologue is buffered and shipped by [`Self::end`] only if
/// a painter wrote something, so an idle frame costs zero bytes and no
/// writer-thread wake. The buffer comes from a thread-local pool.
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
    /// Scratch buffer reused across composited frames (the paint path is one
    /// thread; a nested frame just takes a fresh buffer).
    static FRAME_BODY: std::cell::RefCell<Vec<u8>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl<'a, W: Write> FrameBlock<'a, W> {
    /// Begin a composited frame on `inner`: the block opens now, but nothing
    /// reaches `inner` until [`Self::end`].
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

    /// Whether any painter inside this frame has emitted a byte (an unchanged
    /// frame needs no cursor tail).
    pub(super) const fn opened(&self) -> bool {
        self.painted
    }

    /// Close the block and ship it: `?2026l`, one write, one flush. A frame in
    /// which nothing was written closes to a no-op.
    pub(super) fn end(mut self) -> std::io::Result<()> {
        if let Some(sync) = self.sync.take() {
            let _ = sync.end(&mut self.body);
        }
        let shipped = if self.painted {
            phux_client::perf::PAINTS.add(1);
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
    /// Release the sync-output depth if `end` was never reached.
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

/// Memoized pane tiling for the hot path.
///
/// `compute_layout_in` walks the tree, allocates rects, and rasterizes
/// dividers; the output path used to run it three times a frame for a layout
/// that almost never changes. The key is the whole tiling input (layout with
/// its focus, content rect, viewport) checked by structural equality, so a
/// hit is exact.
#[derive(Debug, Default)]
struct LayoutCache {
    /// The inputs the cached tiling was computed from.
    key: Option<(LayoutState, crate::layout::Rect, (u16, u16))>,
    /// The tiling itself, dropped whenever the key moves.
    tiling: Option<crate::multi_pane::PaneLayout>,
    /// How many times the tiling was actually computed (asserted by tests).
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
            phux_client::perf::LAYOUTS.add(1);
            crate::multi_pane::compute_layout_in(layout, content, viewport_dims)
        })
    }
}

thread_local! {
    /// One cache per attach thread. A hit is a structural match on the full
    /// key, so sharing across session loops can only miss, never answer
    /// wrongly, and it avoids threading another parameter through
    /// `handle_server_frame`.
    static LAYOUT_CACHE: std::cell::RefCell<LayoutCache> =
        std::cell::RefCell::new(LayoutCache::default());
}

/// Run `read` against the memoized tiling of `layout` inside `content`; copy
/// out what is needed. Computes in place if the cache is already borrowed.
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
        phux_client::perf::LAYOUTS.add(1);
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
    terminal_id: &ResourceId,
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

/// The server-authoritative mirror grid `(cols, rows)` a pane is letterboxed
/// with (the rect dims on error, degrading to the clamp paint). Every paint
/// path must use the same dims, or an undersized mirror paints at two
/// origins and shows doubled text.
pub(super) fn mirror_dims(
    terminal: &GhosttyTerminal<'_, '_>,
    rect: crate::layout::Rect,
) -> (u16, u16) {
    let cols = terminal.cols().unwrap_or(rect.w);
    let rows = terminal.rows().unwrap_or(rect.h);
    (cols, rows)
}

/// Render one pane into `rect` (the caller's tiling; the content rect for a
/// single-pane bootstrap). Resizes nothing: the mirror grid is
/// server-authoritative. Returns the renderer's outer-viewport cursor, or
/// `None` without a slot or with the cursor hidden.
pub(super) fn paint_focused_pane<W: Write>(
    out: &mut W,
    rect: crate::layout::Rect,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    kernel: &AttachKernel,
    focused: &ResourceId,
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

/// The single composite end-of-frame cursor authority (ADR-0029): the only
/// place that emits the frame's cursor placement (CUP + DECTCEM) and flushes.
///
/// A known `cursor` `(row, col)` is shown there. Otherwise `fallback_origin`
/// (the focused pane's origin `(x, y)`) parks it hidden, so it never strands
/// at the bar's tail; with neither it parks hidden at the viewport origin.
/// The flush is load-bearing: a CUP without a newline sits in a line-buffered
/// stdout until the next pane output, which an idle pane never sends.
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

/// Clear the viewport and paint every pane + dividers + bar from scratch.
///
/// The composite is buffered in a [`FrameBlock`] and reaches `out` as ONE
/// write and ONE flush, so the stdout queue delivers or drops it whole (a
/// frame split across chunks could lose its `ED2` but keep its terminator).
pub(super) fn paint_full_frame<W: super::RenderSink>(
    out: &mut W,
    layout_state: &LayoutState,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    kernel: &AttachKernel,
    focused_resource: Option<&ResourceId>,
    chrome: &mut ChromeCtx<'_>,
) -> StatusBarPaint {
    let mut block = FrameBlock::begin(out);
    let (painted, composed) = paint_full_frame_into(
        &mut block,
        layout_state,
        panes,
        kernel,
        focused_resource,
        chrome,
    );
    seal_frame(block, painted, composed, chrome.status_bar.as_deref_mut())
}

/// ADR-0105: the empty state's title line.
pub(super) const EMPTY_SESSION_TITLE: &str = "Empty session";
/// ADR-0105: the empty state's explanation line.
pub(super) const EMPTY_SESSION_BODY: &str = "Open a new window to start a terminal.";

/// The lines a keep-empty session with no windows shows (ADR-0105). The hint
/// names the chord bound to `new-window`, or the command palette when no
/// chord is bound.
pub(super) fn empty_session_lines(new_window_chord: Option<&str>) -> [String; 3] {
    let hint = new_window_chord.map_or_else(
        || "Run new-window from the command palette.".to_owned(),
        |chord| format!("{chord}  new window"),
    );
    [
        EMPTY_SESSION_TITLE.to_owned(),
        EMPTY_SESSION_BODY.to_owned(),
        hint,
    ]
}

/// Paint a keep-empty session that holds no windows (ADR-0105): the chrome
/// as usual and [`empty_session_lines`] centered in the content area. One
/// frame block like [`paint_full_frame`]; the cursor ends parked and hidden
/// at the content origin, since there is no pane to own it.
pub(super) fn paint_empty_session<W: super::RenderSink>(
    out: &mut W,
    chrome: &mut ChromeCtx<'_>,
    lines: &[String],
) -> StatusBarPaint {
    let mut block = FrameBlock::begin(out);
    let ContentLayout {
        rect: content,
        rail,
    } = chrome.content_layout();
    let origin = Some((content.x, content.y));
    let _ = block.write_all(b"\x1b[2J\x1b[H");
    write_centered_lines(&mut block, content, lines);
    repaint_sidebar_strip(&mut block, chrome, rail);
    let painted = paint_bar_after_pane(&mut block, chrome, None, origin, true);
    let composed = end_of_frame_cursor(&mut block, None, origin).is_ok();
    seal_frame(block, painted, composed, chrome.status_bar.as_deref_mut())
}

/// Force the sidebar strip to re-emit after a clear wiped its columns. A
/// no-op without a reservation or a painter.
fn repaint_sidebar_strip<W: Write>(out: &mut W, chrome: &mut ChromeCtx<'_>, rail: Option<u16>) {
    let (Some(res), Some(painter)) = (chrome.sidebar, chrome.sidebar_painter.as_deref_mut()) else {
        return;
    };
    painter.invalidate();
    painter.set_rule(sidebar_rule(res.edge));
    painter.set_junction(rail);
    let _ = painter.paint(out, sidebar_rect(chrome.viewport, res));
}

/// Write `lines` centered in `rect`, each clipped to the rect's width.
fn write_centered_lines<W: Write>(out: &mut W, rect: crate::layout::Rect, lines: &[String]) {
    let count = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let top = rect.y.saturating_add(rect.h.saturating_sub(count) / 2);
    let bottom = rect.y.saturating_add(rect.h);
    for (row, line) in (top..bottom).zip(lines) {
        let text: String = line.chars().take(usize::from(rect.w)).collect();
        let width = u16::try_from(text.chars().count()).unwrap_or(rect.w);
        let col = rect.x.saturating_add(rect.w.saturating_sub(width) / 2);
        let _ = super::render::write_cup(out, row, col);
        let _ = out.write_all(text.as_bytes());
    }
}

/// Ship a composited frame and reconcile the bar cache with what actually
/// reached the sink: a failed frame reports `NotPublished` and invalidates
/// the painter. Pane fronts need no forgetting: frames sealed here paint
/// panes forced, which records nothing.
fn seal_frame<W: Write>(
    block: FrameBlock<'_, W>,
    painted: StatusBarPaint,
    composed: bool,
    status_bar: Option<&mut StatusBarPainter>,
) -> StatusBarPaint {
    if composed && block.end().is_ok() {
        return painted;
    }
    if !matches!(painted, StatusBarPaint::NotPublished)
        && let Some(painter) = status_bar
    {
        painter.invalidate();
    }
    StatusBarPaint::NotPublished
}

/// [`paint_full_frame`]'s body, emitting into the frame block. Returns the
/// bar outcome and whether the composition itself succeeded (the cursor tail
/// landed); shipping is the caller's.
fn paint_full_frame_into<W: Write>(
    out: &mut W,
    layout_state: &LayoutState,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    kernel: &AttachKernel,
    focused_resource: Option<&ResourceId>,
    chrome: &mut ChromeCtx<'_>,
) -> (StatusBarPaint, bool) {
    let viewport_dims = chrome.viewport;
    // The whole-repaint duration: the client-side render-lag signal.
    let _paint = tracing::debug_span!(
        "paint_full_frame",
        cols = viewport_dims.0,
        rows = viewport_dims.1,
        panes = panes.len()
    )
    .entered();
    let _timed = phux_client::perf::PAINT_FULL.timer();
    let ContentLayout {
        rect: content,
        rail,
    } = chrome.content_layout();
    let multi = super::multi_pane::compute_layout_in(layout_state, content, viewport_dims);
    // ED2 + home, inside the frame block's transaction.
    let _ = out.write_all(b"\x1b[2J\x1b[H");
    // Forget every front at the clear itself, not trusting each pane's
    // forced paint to get that far.
    super::pane_state::invalidate_all_fronts(panes);
    // Non-focused panes first, then chrome; the focused pane paints LAST so
    // it owns final cursor placement.
    for (id, rect) in &multi.rects {
        if Some(id) == focused_resource {
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
        focused_resource,
        chrome.theme,
        |id| super::pane_state::pane_label(panes_ref, id),
    );
    // The ED2 cleared the strip, so force it to re-emit.
    repaint_sidebar_strip(out, chrome, rail);
    // The ED2 above cleared the bar row, so force a re-emit even if the
    // bar's content is byte-identical to the previous frame.
    let status_bar_painted = paint_bar_after_pane(out, chrome, None, None, true);
    // The focused render may be a no-op, so always end with an explicit
    // cursor placement rather than wherever the bar left it.
    let final_cursor = focused_resource.and_then(|fid| {
        let rect = multi.rects.get(fid).copied().unwrap_or(content);
        paint_focused_pane(out, rect, panes, kernel, fid, true)
    });
    // The focused pane's Rect origin is the fallback cursor parking spot when
    // `final_cursor` is None. All cursor placement + the flush
    // is owned by the one composite authority.
    let fallback_origin = focused_resource
        .and_then(|fid| multi.rects.get(fid).copied())
        .map(|r| (r.x, r.y));
    let cursor_published = end_of_frame_cursor(out, final_cursor, fallback_origin).is_ok();
    (status_bar_painted, cursor_published)
}

/// Repaint ONLY the chrome (dividers/titles, sidebar strip, status bar) in
/// place: what `RepaintLevel::Chrome` drains to, so agent-state changes do
/// not strobe the screen.
///
/// No `ED2`, no pane render (`panes` is shared for exactly that reason), and
/// no painter-cache invalidation, so an unchanged strip costs nothing. It
/// always ends in its own [`end_of_frame_cursor`], not
/// [`paint_bar_after_pane`]'s (which returns early without a bar), because the
/// strip moved the host cursor. One frame block, like [`paint_full_frame`].
pub(super) fn paint_chrome_in_place<W: super::RenderSink>(
    out: &mut W,
    layout_state: &LayoutState,
    panes: &HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    chrome: &mut ChromeCtx<'_>,
) -> StatusBarPaint {
    let mut block = FrameBlock::begin(out);
    let (painted, composed) =
        paint_chrome_in_place_into(&mut block, layout_state, panes, focused_resource, chrome);
    seal_frame(block, painted, composed, chrome.status_bar.as_deref_mut())
}

/// [`paint_chrome_in_place`]'s body, emitting into the frame block.
fn paint_chrome_in_place_into<W: Write>(
    out: &mut W,
    layout_state: &LayoutState,
    panes: &HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    chrome: &mut ChromeCtx<'_>,
) -> (StatusBarPaint, bool) {
    let viewport_dims = chrome.viewport;
    let _paint = tracing::debug_span!(
        "paint_chrome_in_place",
        cols = viewport_dims.0,
        rows = viewport_dims.1,
    )
    .entered();
    let _timed = phux_client::perf::PAINT_CHROME.timer();
    let ContentLayout {
        rect: content,
        rail,
    } = chrome.content_layout();
    let multi = super::multi_pane::compute_layout_in(layout_state, content, viewport_dims);
    // The focused pane's LAST authoritative cursor — read, never re-derived by
    // a render. `None` (hidden / not yet rendered) falls back to the pane's
    // rect origin, hidden, exactly as every other paint tail does.
    let restore = focused_resource
        .and_then(|fid| panes.get(fid))
        .and_then(|slot| slot.renderer.last_cursor());
    let fallback = focused_resource
        .and_then(|fid| multi.rects.get(fid))
        .map(|r| (r.x, r.y));
    // Pane titles are chrome, so the grid repaints here; the skip-cell
    // carve-out keeps it off pane interiors.
    let _ = crate::render::chrome::dividers::render_dividers(
        out,
        &multi,
        content,
        rail,
        focused_resource,
        chrome.theme,
        |id| super::pane_state::pane_label(panes, id),
    );
    if let (Some(res), Some(painter)) = (chrome.sidebar, chrome.sidebar_painter.as_deref_mut()) {
        painter.set_rule(sidebar_rule(res.edge));
        painter.set_junction(rail);
        let _ = painter.paint(out, sidebar_rect(viewport_dims, res));
    }
    // `bar_row_clobbered = false`: nothing cleared the bar row, so the
    // painter's cache decides. Skipped entirely when the config has no bar.
    let (sidebar, session_name) = (chrome.sidebar, chrome.session_name);
    let status_bar_painted =
        chrome
            .status_bar
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
    // The cursor tail runs on every path, bar or no bar.
    let cursor_placed = end_of_frame_cursor(out, restore, fallback).is_ok();
    (status_bar_painted, cursor_placed)
}

/// Restore the status row after a pane render, then place the cursor via
/// [`end_of_frame_cursor`] (`restore_cursor`, else `fallback_origin` hidden).
/// No-op without a painter.
///
/// Pass `fallback_origin = None` where a later pane render owns the cursor.
/// `bar_row_clobbered` bypasses the painter's cache: only a caller that
/// physically cleared the bar row (the full-frame `ED2`) passes `true`; on
/// the output hot path an unchanged bar emits nothing.
pub(super) fn paint_bar_after_pane<W: Write>(
    out: &mut W,
    chrome: &mut ChromeCtx<'_>,
    restore_cursor: Option<(u16, u16)>,
    fallback_origin: Option<(u16, u16)>,
    bar_row_clobbered: bool,
) -> StatusBarPaint {
    let (viewport_dims, sidebar, session_name) =
        (chrome.viewport, chrome.sidebar, chrome.session_name);
    let Some(painter) = chrome.status_bar.as_deref_mut() else {
        // With no bar there is no cursor tail below, so publish the pane's
        // bytes here.
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

/// Emit the status-bar row and nothing else: the cursor tail is the
/// caller's (see [`paint_bar_after_pane`] for `bar_row_clobbered`).
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
    if bar_row_clobbered {
        painter.invalidate();
    }
    match painter.paint_outcome(
        out,
        // Yield the sidebar's columns so the window tabs start
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

/// Close a composited frame with its chrome tail: the bar row, the one
/// cursor placement, then the block's single flush. A frame that emitted
/// nothing closes to nothing. A failed close invalidates the bar cache and
/// reports `NotPublished`.
pub(super) fn close_frame_with_chrome<W: Write>(
    block: FrameBlock<'_, W>,
    chrome: &mut ChromeCtx<'_>,
    cursor: Option<(u16, u16)>,
    fallback_origin: Option<(u16, u16)>,
    compose: ComposePolicy,
) -> StatusBarPaint {
    let bar = BarTail {
        status_bar: chrome.status_bar.as_deref_mut(),
        viewport: chrome.viewport,
        sidebar: chrome.sidebar,
        session_name: chrome.session_name,
    };
    close_frame_reporting(block, bar, cursor, fallback_origin, compose).0
}

/// The slice of a frame's chrome its closing bar row paints from: the part
/// of [`ChromeCtx`] a pane-output frame (which has no strip painter or
/// theme) can also supply.
pub(super) struct BarTail<'a> {
    /// The status-bar painter, or `None` for a bar-less config.
    pub(super) status_bar: Option<&'a mut StatusBarPainter>,
    /// The outer terminal viewport, `(cols, rows)`.
    pub(super) viewport: (u16, u16),
    /// This frame's sidebar reservation; the bar yields its columns.
    pub(super) sidebar: Option<SidebarReservation>,
    /// The attached session's name, as the bar renders it.
    pub(super) session_name: &'a str,
}

/// [`close_frame_with_chrome`], also reporting whether the frame shipped (a
/// failed frame's pane fronts must be forgotten).
pub(super) fn close_frame_reporting<W: Write>(
    mut block: FrameBlock<'_, W>,
    bar: BarTail<'_>,
    cursor: Option<(u16, u16)>,
    fallback_origin: Option<(u16, u16)>,
    compose: ComposePolicy,
) -> (StatusBarPaint, bool) {
    let BarTail {
        mut status_bar,
        viewport: viewport_dims,
        sidebar,
        session_name,
    } = bar;
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
        return (painted, true);
    }
    if !matches!(painted, StatusBarPaint::NotPublished)
        && let Some(painter) = status_bar
    {
        painter.invalidate();
    }
    (StatusBarPaint::NotPublished, false)
}

/// Which edge a reserved sidebar strip docks to (mirrors
/// `phux_config::SidebarPosition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarEdge {
    /// Dock on the left; panes tile to its right.
    Left,
    /// Dock on the right; panes tile to its left.
    Right,
}

/// The sidebar's reservation: `width` columns on `edge`. Built once per frame
/// and threaded to every layout site so they agree on the inset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SidebarReservation {
    /// The edge the strip docks to.
    pub edge: SidebarEdge,
    /// Strip width in columns.
    pub width: u16,
}

/// Fold the sidebar's on/off state and geometry into the per-frame
/// reservation. The strip yields (`None`) below `width + min_pane_cols`, so it
/// never starves the panes it exists to navigate. Automatic width (`0`) is a
/// quarter of the viewport clamped to `28..=40`. The single decision point for
/// every layout site.
pub(super) const fn sidebar_reservation(
    outer_cols: u16,
    enabled: bool,
    width: u16,
    edge: SidebarEdge,
    min_pane_cols: u16,
) -> Option<SidebarReservation> {
    // Automatic sizing follows the viewport, never changing titles or counts:
    // background activity must not reflow the terminal under someone's hands.
    let width = if width == 0 {
        let preferred = outer_cols / 4;
        if preferred < 28 {
            28
        } else if preferred > 40 {
            40
        } else {
            preferred
        }
    } else {
        width
    };
    if enabled && width <= outer_cols && outer_cols - width >= min_pane_cols {
        Some(SidebarReservation { edge, width })
    } else {
        None
    }
}

/// The residual content `Rect` panes tile into: [`content_layout`]'s rect.
pub(super) fn content_rect(
    outer: (u16, u16),
    bar: Option<Position>,
    sidebar: Option<SidebarReservation>,
) -> crate::layout::Rect {
    content_layout(outer, bar, sidebar).rect
}

/// The pane area AND the pane-grid rail row above it. The rail is REPORTED,
/// not inferred from `rect.y`: on a two-row viewport with a top bar `rect.y`
/// is 1 but row 0 is the bar's.
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
    // A top-docked bar pushes the content down one row; the
    // bottom (default) reservation keeps the pre-knob `y: 0` origin.
    let y = match bar {
        Some(Position::Top) => 1,
        Some(Position::Bottom) | None => 0,
    };
    // One row above the panes for the pane-grid rail (the top rule holding
    // top-row titles), unconditional so a split never moves the panes; yielded
    // on a viewport too short to spare it.
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

/// The strip's separator rule faces the panes: trailing on a left dock,
/// leading on a right dock.
pub(super) const fn sidebar_rule(edge: SidebarEdge) -> crate::render::chrome::sidebar::SidebarRule {
    use crate::render::chrome::sidebar::SidebarRule;
    match edge {
        SidebarEdge::Left => SidebarRule::Trailing,
        SidebarEdge::Right => SidebarRule::Leading,
    }
}

/// The sidebar strip's `Rect`: its columns over the FULL height, bar row
/// included (the bar yields via [`bar_inset`]). Strip, bar, and content tile
/// the viewport without overlap, which mouse routing relies on.
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

/// Columns the status bar yields to a docked sidebar (its span is the content
/// rect's horizontal extent); `BarInset::NONE` without one.
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
    use crate::attach::pane_state::published_test_state;
    use crate::attach::render::SYNC_OUTPUT_END;
    use crate::layout::Rect;
    use crate::layout::{LayoutNode, SplitDir};
    use crate::render::ChromeBreakpoints;
    use crate::render::chrome::sidebar::SidebarPainter;
    use crate::render::theme::Theme;
    use phux_config::widget::WidgetRegistry;
    use phux_config::{StatusCfg, Widget};

    const LEFT20: SidebarReservation = SidebarReservation {
        edge: SidebarEdge::Left,
        width: 20,
    };
    const RIGHT20: SidebarReservation = SidebarReservation {
        edge: SidebarEdge::Right,
        width: 20,
    };

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    fn leaf_layout(id: &ResourceId) -> LayoutState {
        LayoutState {
            tree: Some(LayoutNode::Leaf(id.clone())),
            focus: Some(id.clone()),
        }
    }

    fn split_layout(left: &ResourceId, right: &ResourceId) -> LayoutState {
        LayoutState {
            tree: Some(LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(left.clone())),
                right: Box::new(LayoutNode::Leaf(right.clone())),
            }),
            focus: Some(left.clone()),
        }
    }

    fn build_painter() -> StatusBarPainter {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let bar = phux_config::widget::StatusBar::build(&cfg, &WidgetRegistry::with_builtins())
            .expect("bar build");
        StatusBarPainter::new(bar, Position::Bottom)
    }

    /// A left-docked sidebar painter primed with one window.
    fn build_sidebar() -> SidebarPainter {
        let mut painter = SidebarPainter::new(Theme::default());
        painter.set_windows(vec![phux_config::widget::WindowInfo {
            name: "editor".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
            exited: None,
            badge: None,
        }]);
        painter
    }

    /// ADR-0105: the empty state says what happened and names the
    /// `new-window` chord (or the palette), with no pane owning the cursor.
    #[test]
    fn empty_session_state_names_the_new_window_action() {
        let lines = empty_session_lines(Some("C-a c"));
        assert_eq!(
            lines[..3],
            [
                "Empty session",
                "Open a new window to start a terminal.",
                "C-a c  new window"
            ]
        );
        assert!(empty_session_lines(None)[2].contains("command palette"));
        let mut out: Vec<u8> = Vec::new();
        let theme = Theme::default();
        let mut chrome = ChromeCtx {
            viewport: (80, 24),
            sidebar: None,
            status_bar: None,
            sidebar_painter: None,
            session_name: "parked",
            theme: &theme,
        };
        let _ = paint_empty_session(&mut out, &mut chrome, &lines);
        let text = String::from_utf8_lossy(&out);
        assert!(
            lines.iter().all(|line| text.contains(line.as_str())),
            "{text:?}"
        );
        assert!(text.contains("\x1b[?25l"));
    }

    /// An unchanged layout tiles once however often it is read, and every
    /// component of the key (tree, content rect, viewport) forces a retile.
    #[test]
    fn the_layout_cache_retiles_exactly_when_its_key_changes() {
        let (id, other) = (ResourceId::local(1), ResourceId::local(2));
        let content = rect(0, 0, 80, 23);
        let mut cache = LayoutCache::default();
        for _ in 0..16 {
            assert_eq!(
                cache
                    .get(&leaf_layout(&id), content, (80, 24))
                    .rects
                    .get(&id),
                Some(&content)
            );
        }
        assert_eq!(cache.misses, 1);
        let inset = rect(20, 0, 60, 23);
        for (layout, content, viewport, misses) in [
            (leaf_layout(&other), content, (80, 24), 2),
            (leaf_layout(&other), inset, (80, 24), 3),
            (leaf_layout(&other), inset, (100, 30), 4),
            (leaf_layout(&other), inset, (100, 30), 4),
        ] {
            let got = cache
                .get(&layout, content, viewport)
                .rects
                .get(&other)
                .copied();
            assert_eq!((cache.misses, got), (misses, Some(content)));
        }
    }

    /// The strip yields rather than starving the panes: reserved only when
    /// `width + min_pane_cols` fits; automatic width grows with the viewport
    /// (28..=40); the leftover content, strip, and bar inset always tile.
    #[test]
    fn the_sidebar_reserves_only_what_the_panes_can_afford() {
        let min = ChromeBreakpoints::DEFAULT.min_pane_cols;
        let res = |cols, width, min| sidebar_reservation(cols, true, width, SidebarEdge::Left, min);
        assert_eq!(res(59, 20, min), None);
        assert_eq!(res(60, 20, min), Some(LEFT20));
        assert!(
            res(50, 10, min).is_some(),
            "a narrower strip is affordable sooner"
        );
        assert_eq!(
            res(55, 20, 30),
            Some(LEFT20),
            "a lowered floor keeps the strip"
        );
        assert_eq!(res(70, 20, 60), None, "a raised floor takes it away");
        for cols in [0u16, 60, 200] {
            assert_eq!(
                sidebar_reservation(cols, false, 20, SidebarEdge::Left, min),
                None
            );
        }
        for cols in 0u16..=120 {
            let sidebar = res(cols, 20, min);
            let w = content_rect((cols, 24), Some(Position::Bottom), sidebar).w;
            assert!(
                if sidebar.is_some() {
                    w >= min
                } else {
                    w == cols
                },
                "cols={cols}"
            );
        }
        for edge in [SidebarEdge::Left, SidebarEdge::Right] {
            for (cols, expected) in [
                (67, None),
                (68, Some(28)),
                (80, Some(28)),
                (120, Some(30)),
                (144, Some(36)),
                (160, Some(40)),
                (240, Some(40)),
            ] {
                let res = sidebar_reservation(cols, true, 0, edge, 40);
                assert_eq!(res.map(|r| r.width), expected, "cols={cols}");
                if let Some(res) = res {
                    let content = content_rect((cols, 24), Some(Position::Bottom), Some(res));
                    assert!(content.w >= 40);
                    assert_eq!(content.w + sidebar_rect((cols, 24), res).w, cols);
                    assert_eq!(
                        bar_inset((cols, 24), Some(res)).span(cols),
                        (content.x, content.w)
                    );
                }
            }
            assert_eq!(
                sidebar_reservation(200, true, 22, edge, 40).map(|r| r.width),
                Some(22)
            );
            assert!(sidebar_reservation(u16::MAX, true, u16::MAX, edge, 40).is_none());
        }
    }

    /// The content rect folds off the bar row (a top bar shifts the origin),
    /// a rail row above the panes when there is a row to spare, and the
    /// sidebar columns; the rail is REPORTED, since on a two-row viewport with
    /// a top bar `rect.y - 1` is the bar's row.
    #[test]
    fn content_layout_folds_bar_rail_and_sidebar() {
        let top = Some(Position::Top);
        let bottom = Some(Position::Bottom);
        for (outer, bar, sidebar, want, rail) in [
            ((80, 24), None, None, rect(0, 1, 80, 23), Some(0)),
            ((80, 24), bottom, None, rect(0, 1, 80, 22), Some(0)),
            ((200, 50), bottom, None, rect(0, 1, 200, 48), Some(0)),
            ((80, 24), top, None, rect(0, 2, 80, 22), Some(1)),
            ((80, 24), top, Some(LEFT20), rect(20, 2, 60, 22), Some(1)),
            ((80, 24), None, Some(LEFT20), rect(20, 1, 60, 23), Some(0)),
            ((80, 24), bottom, Some(RIGHT20), rect(0, 1, 60, 22), Some(0)),
            ((40, 2), top, None, rect(0, 1, 40, 1), None),
            ((10, 2), bottom, None, rect(0, 0, 10, 1), None),
            ((10, 1), None, None, rect(0, 0, 10, 1), None),
            ((10, 1), bottom, None, rect(0, 0, 10, 0), None),
            ((10, 1), top, None, rect(0, 1, 10, 0), None),
        ] {
            let layout = content_layout(outer, bar, sidebar);
            assert_eq!(
                (layout.rect, layout.rail),
                (want, rail),
                "{outer:?} {bar:?} {sidebar:?}"
            );
            assert_eq!(content_rect(outer, bar, sidebar), layout.rect);
        }
        let huge = SidebarReservation {
            edge: SidebarEdge::Left,
            width: 999,
        };
        let clamped = content_rect((80, 24), None, Some(huge));
        assert_eq!(
            (clamped.x, clamped.w),
            (80, 0),
            "an over-wide strip clamps, no underflow"
        );
        assert_eq!(bar_inset((80, 24), Some(huge)).span(80).1, 0);
    }

    /// The strip is full height at its edge; the bar yields exactly its
    /// columns, so `bar_inset`'s span is the content rect's horizontal extent.
    #[test]
    fn the_strip_is_full_height_and_the_bar_yields_its_columns() {
        assert_eq!(sidebar_rect((80, 24), LEFT20), rect(0, 0, 20, 24));
        assert_eq!(sidebar_rect((80, 24), RIGHT20), rect(60, 0, 20, 24));
        assert_eq!(bar_inset((80, 24), None), BarInset::NONE);
        assert_eq!(
            bar_inset((80, 24), Some(LEFT20)),
            BarInset { left: 20, right: 0 }
        );
        assert_eq!(
            bar_inset((80, 24), Some(RIGHT20)),
            BarInset { left: 0, right: 20 }
        );
        for res in [LEFT20, RIGHT20] {
            let content = content_rect((80, 24), Some(Position::Bottom), Some(res));
            assert_eq!(
                bar_inset((80, 24), Some(res)).span(80),
                (content.x, content.w)
            );
        }
    }

    /// ADR-0029: the composite cursor emitter's three-way fallback.
    #[test]
    fn end_of_frame_cursor_resolves_all_three_cases() {
        for (cursor, fallback, want) in [
            (Some((2, 4)), None, "\x1b[3;5H\x1b[?25h"),
            (None, Some((3, 5)), "\x1b[6;4H\x1b[?25l"),
            (None, None, "\x1b[1;1H\x1b[?25l"),
        ] {
            let mut out = Vec::new();
            end_of_frame_cursor(&mut out, cursor, fallback).expect("write");
            assert_eq!(String::from_utf8(out).unwrap(), want);
        }
    }

    /// Paint a full frame of `layout` over published panes into `out`.
    fn full_frame<W: Write>(
        out: &mut W,
        layout: &LayoutState,
        entries: &[(&ResourceId, u16, u16, &[u8])],
        viewport: (u16, u16),
        bar: Option<&mut StatusBarPainter>,
        sidebar: Option<(SidebarReservation, &mut SidebarPainter)>,
    ) -> StatusBarPaint {
        let (kernel, _, mut panes) = published_test_state(entries);
        let (res, strip) = sidebar.map_or((None, None), |(res, strip)| (Some(res), Some(strip)));
        let theme = Theme::default();
        let mut chrome = ChromeCtx {
            viewport,
            sidebar: res,
            status_bar: bar,
            sidebar_painter: strip,
            session_name: "demo",
            theme: &theme,
        };
        paint_full_frame(
            out,
            layout,
            &mut panes,
            &kernel,
            layout.focus.as_ref(),
            &mut chrome,
        )
    }

    /// A large-viewport truecolor repaint is bigger than the stdout sink's
    /// backlog cap in ONE chunk: the size premise behind the writer's
    /// oversized-frame rule (the cap once rejected it, freezing the screen).
    #[test]
    fn a_large_truecolor_full_frame_exceeds_the_sink_backlog_cap() {
        const COLS: u16 = 100;
        const ROWS: u16 = 40;
        let pane = ResourceId::local(1);
        // Every cell a different fg+bg, so no SGR run can coalesce.
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
        let mut out: Vec<u8> = Vec::new();
        full_frame(
            &mut out,
            &leaf_layout(&pane),
            &[(&pane, COLS, ROWS, &vt)],
            (COLS, ROWS),
            None,
            None,
        );
        let per_cell = out.len() / (usize::from(COLS) * usize::from(ROWS));
        assert!(
            per_cell >= 20,
            "a truecolor cell costs ~40 bytes; got {per_cell}"
        );
        assert!(per_cell * 250 * 70 > crate::attach::stdout_writer::CAP_BYTES);
    }

    /// A full frame composites both panes and the divider inside one
    /// synchronized ED2 transaction, reaching the sink as ONE write and ONE
    /// flush (so the stdout queue delivers or drops it whole), and ends with
    /// an explicit cursor placement.
    #[test]
    fn a_full_frame_is_one_synchronized_chunk() {
        let (left, right) = (ResourceId::local(1), ResourceId::local(2));
        let layout = split_layout(&left, &right);
        let entries: [(&ResourceId, u16, u16, &[u8]); 2] =
            [(&left, 80, 24, b"hello\r\n"), (&right, 80, 24, b"")];

        let mut out: Vec<u8> = Vec::new();
        full_frame(&mut out, &layout, &entries, (80, 24), None, None);
        let s = String::from_utf8_lossy(&out);
        assert!(s.starts_with("\x1b[?2026h\x1b[?25l\x1b[2J\x1b[H"), "{s:?}");
        assert!(s.ends_with("\x1b[?2026l"), "{s:?}");
        assert!(
            s.contains(";41H") || s.contains(";40H"),
            "divider CUP; {s:?}"
        );
        assert!(s.contains("\x1b[?25h") || s.contains("\x1b[?25l"));

        let mut bar = build_painter();
        let mut strip = build_sidebar();
        let mut sink = ChunkCountingSink::default();
        let outcome = full_frame(
            &mut sink,
            &layout,
            &entries,
            (80, 24),
            Some(&mut bar),
            Some((LEFT20, &mut strip)),
        );
        assert_ne!(outcome, StatusBarPaint::NotPublished);
        assert_eq!((sink.writes, sink.flushes), (1, 1));
        let s = String::from_utf8_lossy(&sink.bytes);
        assert!(
            s.starts_with("\x1b[?2026h") && s.ends_with("\x1b[?2026l"),
            "{s:?}"
        );
        assert_eq!(
            s.matches("\x1b[?2026h").count(),
            1,
            "nested guards do not reopen"
        );
    }

    /// Fails the frame at its tail: the write carrying `?2026l`, or the flush
    /// after it.
    struct TailFailSink {
        fail_sync_end: bool,
        fail_final_flush: bool,
        sync_end_seen: bool,
    }

    impl Write for TailFailSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.ends_with(SYNC_OUTPUT_END) {
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

    /// A frame whose terminator or final flush failed never publishes the bar.
    #[test]
    fn a_failed_frame_tail_does_not_publish_the_bar() {
        let id = ResourceId::local(1);
        for (fail_sync_end, fail_final_flush) in [(true, false), (false, true)] {
            let mut sink = TailFailSink {
                fail_sync_end,
                fail_final_flush,
                sync_end_seen: false,
            };
            let mut bar = build_painter();
            let outcome = full_frame(
                &mut sink,
                &leaf_layout(&id),
                &[(&id, 80, 24, b"")],
                (80, 24),
                Some(&mut bar),
                None,
            );
            assert_eq!(outcome, StatusBarPaint::NotPublished);
        }
    }

    /// Counts sink-visible writes and flushes (the stdout queue's chunks).
    #[derive(Default)]
    struct ChunkCountingSink {
        writes: usize,
        flushes: usize,
        bytes: Vec<u8>,
    }

    impl Write for ChunkCountingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn bar_after_pane(
        painter: &mut StatusBarPainter,
        cursor: Option<(u16, u16)>,
        fallback: Option<(u16, u16)>,
        clobbered: bool,
    ) -> String {
        let mut out = Vec::new();
        let theme = Theme::default();
        let mut chrome = ChromeCtx {
            viewport: (80, 24),
            sidebar: None,
            status_bar: Some(painter),
            sidebar_painter: None,
            session_name: "demo",
            theme: &theme,
        };
        paint_bar_after_pane(&mut out, &mut chrome, cursor, fallback, clobbered);
        String::from_utf8_lossy(&out).into_owned()
    }

    /// The bar-then-cursor tail: a known cursor is restored visible; unknown,
    /// it parks hidden at the pane origin, else at (0,0) hidden rather than
    /// stranding at the bar's last cell.
    #[test]
    fn paint_bar_after_pane_places_the_cursor() {
        let s = bar_after_pane(&mut build_painter(), Some((4, 7)), Some((0, 0)), true);
        assert!(s.contains("\x1b[5;8H") && s.contains("\x1b[?25h"), "{s:?}");
        assert!(!s.contains("\x1b[1;1H"), "{s:?}");

        let s = bar_after_pane(&mut build_painter(), None, Some((3, 5)), true);
        let cup = s.rfind("\x1b[6;4H").expect("fallback CUP");
        assert!(
            s[cup..].contains("\x1b[?25l") && !s[cup..].contains("\x1b[?25h"),
            "{s:?}"
        );

        let s = bar_after_pane(&mut build_painter(), None, None, true);
        assert!(
            s.contains("\x1b[24;1H") && s.contains("\x1b[1;1H\x1b[?25l"),
            "{s:?}"
        );
        assert!(!s.contains("\x1b[?25h"), "{s:?}");
    }

    /// On the hot path an unchanged bar is not re-emitted, but a row the frame
    /// clobbered (ED2) is, even when unchanged.
    #[test]
    fn paint_bar_after_pane_re_emits_only_a_changed_or_clobbered_bar() {
        for clobbered in [false, true] {
            let mut painter = build_painter();
            assert!(
                bar_after_pane(&mut painter, Some((4, 7)), None, clobbered).contains("\x1b[24;1H")
            );
            let second = bar_after_pane(&mut painter, Some((4, 7)), None, clobbered);
            assert_eq!(second.contains("\x1b[24;1H"), clobbered, "{second:?}");
            assert!(second.contains("\x1b[5;8H"), "{second:?}");
        }
    }

    fn chrome_in_place<W: Write>(
        out: &mut W,
        layout: &LayoutState,
        panes: &HashMap<ResourceId, PaneSlot>,
        bar: Option<&mut StatusBarPainter>,
        strip: &mut SidebarPainter,
    ) {
        let theme = Theme::default();
        let mut chrome = ChromeCtx {
            viewport: (80, 24),
            sidebar: Some(LEFT20),
            status_bar: bar,
            sidebar_painter: Some(strip),
            session_name: "demo",
            theme: &theme,
        };
        paint_chrome_in_place(out, layout, panes, layout.focus.as_ref(), &mut chrome);
    }

    /// The anti-strobe contract: the in-place chrome paint is one
    /// transaction chunk that never clears the viewport or re-renders a pane,
    /// ends in the composite cursor, and keeps the strip's cache (an
    /// unchanged strip re-emits nothing).
    #[test]
    fn paint_chrome_in_place_never_clears_or_repaints_panes() {
        let id = ResourceId::local(1);
        let layout = LayoutState {
            tree: None,
            focus: Some(id.clone()),
        };
        let mut slot = PaneSlot::new_with_size(60, 23).expect("slot");
        slot.terminal.vt_write(b"PANEBODY");
        let panes = HashMap::from([(id, slot)]);
        let mut bar = build_painter();
        let mut strip = build_sidebar();
        let mut sink = ChunkCountingSink::default();
        chrome_in_place(&mut sink, &layout, &panes, Some(&mut bar), &mut strip);
        assert_eq!((sink.writes, sink.flushes), (1, 1));
        let s = String::from_utf8_lossy(&sink.bytes);
        assert!(
            s.starts_with("\x1b[?2026h") && s.ends_with("\x1b[?2026l"),
            "{s:?}"
        );
        assert!(!s.contains("\x1b[2J") && !s.contains("PANEBODY"), "{s:?}");
        assert!(s.contains("\x1b[?25h") || s.contains("\x1b[?25l"));
        assert!(s.contains("\x1b[2;1H"), "the strip painted; {s:?}");

        let mut again: Vec<u8> = Vec::new();
        chrome_in_place(&mut again, &layout, &panes, Some(&mut bar), &mut strip);
        assert!(!String::from_utf8_lossy(&again).contains("\x1b[2;1H"));
    }

    /// With a sidebar and no status bar the in-place paint still ends with
    /// the cursor tail (it once stranded in the strip), parked hidden at the
    /// focused pane's origin right of the strip and under the rail.
    #[test]
    fn paint_chrome_in_place_restores_the_cursor_without_a_status_bar() {
        let id = ResourceId::local(1);
        let panes = HashMap::from([(id.clone(), PaneSlot::new_with_size(60, 24).expect("slot"))]);
        let mut out: Vec<u8> = Vec::new();
        chrome_in_place(
            &mut out,
            &leaf_layout(&id),
            &panes,
            None,
            &mut build_sidebar(),
        );
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("\x1b[2;1H"), "{s:?}");
        assert!(s.ends_with("\x1b[2;21H\x1b[?25l\x1b[?2026l"), "{s:?}");
    }
}
