//! View geometry snapshots, repaint classification, and PTY reflow.

use super::super::{
    AttachError, Connection, HashMap, RepaintLevel, ResourceId, SidebarReservation,
    emit_moved_tiles, emit_view_reflow, view_rects,
};
use super::PriorView;

/// How much of the view a batch that reported `layout_changed` must redraw,
/// from the pre-batch and current tilings:
///
/// * the same panes on the same cells: only focus (the heavy borders) moved,
///   so repaint chrome in place — [`RepaintLevel::Chrome`];
/// * the same rectangles holding different panes: redraw the panes in place
///   with no whole-screen clear — [`RepaintLevel::Panes`];
/// * moved rectangles, or a pane with no published grid to draw: a full
///   clear-and-recompose — [`RepaintLevel::Full`].
///
/// The middle arm is what keeps a tab or window click from clearing and
/// re-compositing the whole screen — the flash a pane on the alternate
/// screen shows on a terminal that ignores DEC 2026. It is taken only when
/// the new tiling fully covers the old cells: an identical rectangle
/// multiset, and every pane `pane_paints` (has a slot and a published
/// replica), so no stale cell can survive the skipped clear.
///
/// This rect diff cannot see boxes drawn over unchanged tiles, so the
/// caller (`SessionLoop::classify_repaint`) overrides it to
/// [`RepaintLevel::Full`] first when the batch dismissed a modal overlay
/// (palette, picker, prompt, copy-mode; focus-only palette actions too) or
/// opened or closed the floating overlay. That trades the Chrome/Panes
/// no-clear path for a clear that removes the stale box; on a terminal
/// without DEC 2026 the dismiss can flash, which is the documented "On
/// dismiss, repaint everything" behavior of `handle_overlay_key`.
pub(in crate::attach::driver::loop_state) fn classify_repaint(
    before: &HashMap<ResourceId, crate::layout::Rect>,
    after: &HashMap<ResourceId, crate::layout::Rect>,
    pane_paints: impl Fn(&ResourceId) -> bool,
) -> RepaintLevel {
    if before == after {
        return RepaintLevel::Chrome;
    }
    if super::same_rects(before, after) && after.keys().all(pane_paints) {
        return RepaintLevel::Panes;
    }
    RepaintLevel::Full
}

/// Whether two tilings paint the same rectangles, ignoring which pane holds
/// each (a pane swap, or a switch between same-shaped windows). Order-free.
pub(in crate::attach::driver::loop_state) fn same_rects(
    a: &HashMap<ResourceId, crate::layout::Rect>,
    b: &HashMap<ResourceId, crate::layout::Rect>,
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let sorted = |m: &HashMap<ResourceId, crate::layout::Rect>| {
        let mut rects: Vec<(u16, u16, u16, u16)> =
            m.values().map(|r| (r.y, r.x, r.w, r.h)).collect();
        rects.sort_unstable();
        rects
    };
    sorted(a) == sorted(b)
}

impl super::super::SessionLoop {
    /// How much of the view a batch that reported `layout_changed` must
    /// redraw, from the pre-batch and current tilings. A dismissed modal or a
    /// toggled floating overlay forces [`RepaintLevel::Full`]; otherwise see
    /// [`classify_repaint`] for the rect-diff rule. The pane liveness probe
    /// reads this loop's slots and published grids. Unchanged tiles over a
    /// pane whose viewport the batch scrolled still repaint that pane's rows
    /// ([`RepaintLevel::Rows`]).
    pub(super) fn classify_repaint(
        &self,
        prior: &PriorView,
        sidebar: Option<SidebarReservation>,
    ) -> RepaintLevel {
        // A floating box came or went, or a modal was dismissed, over
        // unchanged tiles: only a full clear-and-recompose removes the box and
        // restores the pane cells it covered.
        let floating = crate::attach::floating::floating_pane(&self.mirror.panes).is_some();
        if prior.floating != floating || (prior.overlay && !self.overlays.is_active()) {
            return RepaintLevel::Full;
        }
        let after = view_rects(
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            self.content(sidebar),
            self.viewport_dims,
        );
        let level = super::classify_repaint(&prior.rects, &after, |id| {
            self.mirror.panes.contains_key(id)
                && crate::attach::pane_state::published_terminal(&self.mirror.engine_kernel, id)
                    .is_some()
        });
        if level == RepaintLevel::Chrome && self.viewport_offsets(&after) != prior.viewports {
            return RepaintLevel::Rows;
        }
        level
    }

    /// The history viewport offset of every pane the view draws: the tiled
    /// panes in `rects` plus the floating overlay's pane. A pane with no
    /// published grid has no offset and is left out.
    pub(super) fn viewport_offsets(
        &self,
        rects: &HashMap<ResourceId, crate::layout::Rect>,
    ) -> HashMap<ResourceId, u64> {
        rects
            .keys()
            .chain(crate::attach::floating::floating_pane(&self.mirror.panes))
            .filter_map(|id| {
                let terminal =
                    crate::attach::pane_state::published_terminal(&self.mirror.engine_kernel, id)?;
                let offset = terminal.scrollbar().ok()?.offset;
                Some((id.clone(), offset))
            })
            .collect()
    }

    /// Resize PTYs whose tile changed because this batch moved the view.
    pub(super) async fn reflow_after_input(
        &self,
        conn: &mut Connection,
        sidebar: Option<SidebarReservation>,
        prior: &PriorView,
        layout_changed: bool,
    ) -> Result<(), AttachError> {
        // Resize PTYs when client-local geometry (zoom, sidebar) changed.
        if self.mirror.zoomed != prior.zoomed || sidebar != prior.sidebar {
            emit_view_reflow(
                conn,
                &self.mirror.workspace,
                self.mirror.zoomed.as_ref(),
                &prior.rects,
                self.content(sidebar),
                self.resize_cell_px,
            )
            .await?;
        } else if layout_changed
            && self.mirror.workspace.active == prior.active_window
            && !self.detach_pending
        {
            // This window's own layout moved (`resize-pane`, a divider drag,
            // a swap): resize exactly the panes whose tile changed. A switch
            // to another window is not a resize (bootstrap sized its panes),
            // and nothing may follow a sent `DETACH`.
            let rects = view_rects(
                &self.mirror.workspace,
                self.mirror.zoomed.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            );
            emit_moved_tiles(conn, &prior.rects, &rects, self.resize_cell_px).await?;
        }
        Ok(())
    }
}
