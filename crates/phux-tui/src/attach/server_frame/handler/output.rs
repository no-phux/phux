//! `RESOURCE_OUTPUT`: feed the replica, then paint the composited frame.

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;

use super::attached::{initial_pane_dims, paint_permitted};
use super::{FrameCtx, FrameOutcome, KernelRoute};
use crate::attach::outcome::AttachError;
use crate::attach::paint::{SidebarReservation, StatusBarPaint, content_rect, paint_focused_pane};
use crate::attach::pane_state::{AttachKernel, PaneSlot, published_replica};
use crate::attach::render::ReplicaWalk;
use crate::layout::{ArrangementDoc, LayoutState, Rect};
use crate::predict::{PredictionState, reconcile_terminal_output_per_cell_at, render_overlay};
use crate::render::chrome::status_bar::StatusBarPainter;

/// Fold applied VT bytes into the pane's chrome caches and paint the result.
/// Metadata (title, sync-output state) refreshes even when the paint is
/// withheld; a withheld frame tiles nothing and composes no chrome.
pub(super) fn handle_terminal_output<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    seq: u64,
    bytes: &[u8],
    route: KernelRoute,
) -> Result<FrameOutcome, AttachError> {
    let damaged = route.damaged(terminal_id);
    let ack = route.ack;
    // Live output can retire scrollback; every exit carries the notice.
    let notices = route.notices;
    // The span's CLOSE duration is the per-frame client paint cost.
    ctx.frame_span
        .record("terminal_id", tracing::field::debug(terminal_id));
    ctx.frame_span.record("seq", seq);
    ctx.frame_span.record("bytes", bytes.len());
    phux_client::perf::FRAMES.add(1);
    let walk = published_replica(ctx.engine_kernel, terminal_id).ok_or_else(|| {
        AttachError::Protocol(format!(
            "RESOURCE_OUTPUT targeted unpublished {terminal_id:?}"
        ))
    })?;
    let terminal = walk.terminal;
    let bar = ctx.status_bar.as_ref().map(|p| p.position());
    let content = content_rect(ctx.viewport_dims, bar, ctx.sidebar);
    // Tile only for a never-seen pane; the steady state is one lookup.
    if !ctx.panes.contains_key(terminal_id) {
        let (cols, rows) = initial_pane_dims(ctx, terminal_id, content);
        ctx.panes
            .insert(terminal_id.clone(), PaneSlot::new_with_size(cols, rows)?);
    }
    let Some(slot) = ctx.panes.get_mut(terminal_id) else {
        return Err(AttachError::Protocol(format!(
            "pane slot missing for {terminal_id:?} after seeding"
        )));
    };
    let title_changed = slot.title_changed(terminal);
    let sync_output_active = slot.update_sync_output(terminal, tokio::time::Instant::now());
    if !damaged {
        return Ok(FrameOutcome {
            ack,
            notices,
            ..FrameOutcome::default()
        });
    }
    // ADR-0147: panes under the floating overlay pause like under a modal.
    let floating = crate::attach::floating::floating_pane(ctx.panes).cloned();
    let under_floating = floating.as_ref().is_some_and(|id| id != terminal_id);
    if !paint_permitted(
        ctx.overlay_active || under_floating,
        ctx.defer_paint,
        sync_output_active,
    ) {
        phux_client::perf::SKIPPED.add(1);
        return Ok(FrameOutcome {
            ack,
            authoritative_damage: vec![terminal_id.clone()],
            chrome_dirty: title_changed,
            notices,
            ..FrameOutcome::default()
        });
    }
    let status_bar_painted = paint_output_frame(
        OutputFrame {
            out: ctx.out,
            kernel: ctx.engine_kernel,
            panes: ctx.panes,
            workspace: ctx.workspace,
            zoomed: ctx.zoomed.as_ref(),
            focused_resource: floating.as_ref().or(ctx.focused_resource.as_ref()),
            status_bar: ctx.status_bar.as_deref_mut(),
            sidebar: ctx.sidebar,
            viewport_dims: ctx.viewport_dims,
            session_name: ctx.session_name.as_str(),
            predict: ctx.predict,
        },
        std::slice::from_ref(terminal_id),
    );
    Ok(FrameOutcome {
        ack,
        authoritative_damage: vec![terminal_id.clone()],
        painted_output: (ctx.focused_resource.as_ref() == Some(terminal_id))
            .then(|| terminal_id.clone()),
        chrome_dirty: title_changed,
        notices,
        status_bar_painted,
        ..FrameOutcome::default()
    })
}

/// Everything one composited output frame paints from; built by the
/// `RESOURCE_OUTPUT` arm and by the driver's pacer settle.
pub(in crate::attach) struct OutputFrame<'a, W> {
    pub(in crate::attach) out: &'a mut W,
    pub(in crate::attach) kernel: &'a AttachKernel,
    pub(in crate::attach) panes: &'a mut HashMap<ResourceId, PaneSlot>,
    pub(in crate::attach) workspace: &'a ArrangementDoc,
    /// The pane zoomed to fill the window, if any.
    pub(in crate::attach) zoomed: Option<&'a ResourceId>,
    pub(in crate::attach) focused_resource: Option<&'a ResourceId>,
    pub(in crate::attach) status_bar: Option<&'a mut StatusBarPainter>,
    pub(in crate::attach) sidebar: Option<SidebarReservation>,
    pub(in crate::attach) viewport_dims: (u16, u16),
    pub(in crate::attach) session_name: &'a str,
    pub(in crate::attach) predict: &'a mut PredictionState,
}

/// Composite and ship ONE frame covering every pane in `targets`: a single
/// DEC 2026 block with each pane interior, the predictive overlay, the bar,
/// and the cursor, then one flush. A paced settle passes several panes so
/// they cannot tear across blocks.
pub(in crate::attach) fn paint_output_frame<W: crate::attach::RenderSink>(
    paint: OutputFrame<'_, W>,
    targets: &[ResourceId],
) -> StatusBarPaint {
    let OutputFrame {
        out,
        kernel,
        panes,
        workspace,
        zoomed,
        focused_resource,
        status_bar,
        sidebar,
        viewport_dims,
        session_name,
        predict,
    } = paint;
    // Tile against the zoom-honoring view; panes off it get no rect.
    let Some(active_ls) = workspace.render_group(zoomed) else {
        return StatusBarPaint::NotPublished;
    };
    let active_ls = active_ls.as_ref();
    let bar = status_bar.as_ref().map(|p| p.position());
    let content = content_rect(viewport_dims, bar, sidebar);
    // Its own span, so traces separate paint cost from `vt_apply`.
    let _paint_trigger = tracing::debug_span!("paint_trigger", rows = viewport_dims.1).entered();
    let mut block = crate::attach::paint::FrameBlock::begin(out);
    for terminal_id in targets {
        let rect = crate::attach::paint::tiled_rect(active_ls, content, viewport_dims, terminal_id);
        let Some(walk) = published_replica(kernel, terminal_id) else {
            continue;
        };
        if focused_resource == Some(terminal_id) {
            // A focused pane with no tile is the floating overlay's box
            // (ADR-0147) or the single-pane bootstrap's content rect.
            let rect = rect.unwrap_or_else(|| {
                crate::attach::floating::untiled_rect(panes, terminal_id, content)
            });
            paint_focused_interior(&mut block, rect, panes, kernel, terminal_id, walk, predict);
        } else if let Some(rect) = rect {
            // Non-focused panes repaint on their own output; no rect, no paint.
            paint_background_interior(&mut block, rect, panes, terminal_id, walk);
        }
    }
    let (painted, shipped) = finish_output_frame(
        block,
        status_bar,
        &FrameTail {
            focused_resource,
            panes,
            active_ls,
            content,
            viewport_dims,
            sidebar,
            session_name,
        },
    );
    if !shipped {
        // The panes recorded cells the terminal may never have
        // received, and their dirty bits are already cleared.
        crate::attach::pane_state::invalidate_all_fronts(panes);
    }
    painted
}

/// The chrome-and-cursor tail of a composited frame.
struct FrameTail<'a> {
    focused_resource: Option<&'a ResourceId>,
    panes: &'a HashMap<ResourceId, PaneSlot>,
    active_ls: &'a LayoutState,
    content: Rect,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &'a str,
}

/// Close a composited frame: bar, cursor, epilogue, one flush. The second
/// value is whether it shipped.
fn finish_output_frame<W: crate::attach::RenderSink>(
    block: crate::attach::paint::FrameBlock<'_, W>,
    status_bar: Option<&mut StatusBarPainter>,
    tail: &FrameTail<'_>,
) -> (StatusBarPaint, bool) {
    let focused_cursor = tail
        .focused_resource
        .and_then(|fid| tail.panes.get(fid))
        .and_then(|slot| slot.renderer.last_cursor());
    // With no cursor, park at the focused pane's origin, never the bar's tail.
    let fallback_origin = tail
        .focused_resource
        .and_then(|fid| {
            crate::attach::paint::tiled_rect(tail.active_ls, tail.content, tail.viewport_dims, fid)
        })
        .map_or(Some((0, 0)), |r| Some((r.x, r.y)));
    let bar = crate::attach::paint::BarTail {
        status_bar,
        viewport: tail.viewport_dims,
        sidebar: tail.sidebar,
        session_name: tail.session_name,
    };
    crate::attach::paint::close_frame_reporting(
        block,
        bar,
        focused_cursor,
        fallback_origin,
        // A pane-output frame changes no bar input, so the widget pipeline
        // runs only if a setter already marked the strip dirty.
        crate::render::chrome::status_bar::ComposePolicy::WhenDirty,
    )
}

/// Render the focused pane's interior and reconcile predictions against the
/// cells that just landed.
fn paint_focused_interior<W: crate::attach::RenderSink>(
    out: &mut W,
    rect: Rect,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    kernel: &AttachKernel,
    fid: &ResourceId,
    walk: ReplicaWalk<'_, 'static, 'static>,
    predict: &mut PredictionState,
) {
    let _ = paint_focused_pane(out, rect, panes, kernel, fid, false);
    // Reconcile and overlay run in pane-local coordinates.
    let (focused_cursor_local, pane_origin) = panes.get(fid).map_or((None, (0, 0)), |s| {
        (s.renderer.last_cursor_local(), s.renderer.last_origin())
    });
    // ADR-0090: sync the screen mode first; a screen switch drops predictions
    // anchored to the other screen.
    predict.set_alt_screen(crate::attach::input_dispatch::terminal_in_alt_screen(
        walk.terminal,
    ));
    // Per-cell reconcile of pending predictions (see [`crate::predict`]).
    let now_ms = crate::attach::input_dispatch::predict_now_ms();
    if let Some((row, col)) = focused_cursor_local {
        let _stats = reconcile_terminal_output_per_cell_at(predict, row, col, now_ms, |r, c| {
            panes.get_mut(fid).and_then(|s| {
                // Full grapheme clusters, so multi-codepoint predictions match.
                s.renderer
                    .read_grapheme_string_at(walk, r, c)
                    .ok()
                    .flatten()
            })
        });
    } else {
        // Cursor hidden: cannot anchor, drain wholesale.
        predict.clear();
    }
    // Paint surviving predictions, gated by the ADR-0090 display policy.
    if predict.should_display(now_ms) {
        let _ = render_overlay(predict, pane_origin, out);
        // The guesses now sit over the pane's cells; the front
        // buffer must not keep claiming what was there before them.
        if let Some(slot) = panes.get_mut(fid) {
            crate::attach::pane_state::invalidate_predicted_rows(slot, predict);
        }
    }
}

/// Repaint a non-focused pane on its own output (dirty rows only); the
/// frame tail restores the focused cursor.
fn paint_background_interior<W: crate::attach::RenderSink>(
    out: &mut W,
    rect: Rect,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    walk: ReplicaWalk<'_, 'static, 'static>,
) {
    let Some(slot) = panes.get_mut(terminal_id) else {
        return;
    };
    // Letterbox like `paint_full_frame`, or dirty rows land offset from
    // full-frame rows (doubled text).
    let mirror = crate::attach::paint::mirror_dims(walk.terminal, rect);
    let _ = slot.renderer.render_at_letterboxed(
        walk,
        out,
        (rect.x, rect.y),
        (rect.w, rect.h),
        mirror,
        false,
    );
}
