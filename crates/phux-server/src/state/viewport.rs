use phux_core::ids::TerminalId;

use super::{ClientId, ServerState};

/// Derive the per-cell pixel size implied by one client's viewport report:
/// `pixel / cells`, floored. `None` when the report carries no pixel metrics
/// or they are degenerate — zero cells, or a pixel field smaller than the
/// cell count (a sub-pixel cell is a bogus report, not a tiny font).
fn viewport_cell_px(v: &phux_protocol::wire::frame::ViewportInfo) -> Option<(u16, u16)> {
    if v.cols == 0 || v.rows == 0 {
        return None;
    }
    let w = v.pixel_w? / v.cols;
    let h = v.pixel_h? / v.rows;
    (w > 0 && h > 0).then_some((w, h))
}

impl ServerState {
    /// Record `client`'s current outer viewport (`phux-nk07`), as carried by
    /// `ATTACH` or a live `VIEWPORT_RESIZE`. No-op for an unattached client.
    pub fn set_client_viewport(
        &mut self,
        client: ClientId,
        viewport: phux_protocol::wire::frame::ViewportInfo,
    ) {
        // Direct field access, not an accessor: `clients` and
        // `lifecycle` are disjoint fields, so the borrow checker splits
        // them. A `&mut self` accessor for the clock would borrow all of
        // `ServerState` and this would stop compiling.
        //
        // The stamp must tick inside the `if let`, not above it: an
        // announcement from an unattached client is a no-op and must not
        // burn a sequence number.
        if let Some(c) = self.clients.attached.get_mut(&client) {
            self.lifecycle.viewport_clock += 1;
            c.viewport = Some(viewport);
            c.viewport_seq = self.lifecycle.viewport_clock;
        }
    }

    /// Resolve the one authoritative `(cols, rows)` a Terminal's PTY should
    /// take, given the viewports of every client subscribed to it and the
    /// active `window-size` policy (`phux-nk07`).
    ///
    /// Returns `None` when the policy is `Manual` (geometry is fixed
    /// externally, never derived from views) or when no subscriber has
    /// announced a usable (non-zero) viewport yet — in both cases the caller
    /// leaves the PTY size unchanged. `latest` is the viewport of the client
    /// that just resized, used only by the `Latest` policy.
    ///
    /// Degenerate `0`-dimension viewports are ignored in the min/max so a
    /// transient resize-to-zero (a detaching client, a probe) can't collapse
    /// the shared grid.
    #[must_use]
    pub fn resolve_terminal_geometry(
        &self,
        terminal: TerminalId,
        latest: Option<phux_protocol::wire::frame::ViewportInfo>,
    ) -> Option<(u16, u16)> {
        use phux_config::WindowSize;
        match self.config.window_size {
            WindowSize::Manual => None,
            WindowSize::Latest => latest
                .filter(|v| v.cols > 0 && v.rows > 0)
                .map(|v| (v.cols, v.rows)),
            WindowSize::Smallest | WindowSize::Largest => {
                let viewports = self
                    .subscribers_for_terminal(terminal)
                    .iter()
                    .filter_map(|cid| self.clients.attached.get(cid).and_then(|c| c.viewport))
                    .filter(|v| v.cols > 0 && v.rows > 0);
                let mut acc: Option<(u16, u16)> = None;
                for v in viewports {
                    acc = Some(match (acc, self.config.window_size) {
                        (None, _) => (v.cols, v.rows),
                        (Some((c, r)), WindowSize::Smallest) => (c.min(v.cols), r.min(v.rows)),
                        (Some((c, r)), _) => (c.max(v.cols), r.max(v.rows)),
                    });
                }
                acc
            }
        }
    }

    /// Resolve the per-cell pixel size a Terminal should report — via the
    /// PTY `winsize` pixel fields and XTWINOPS size replies — from the most
    /// recent usable pixel report among the Terminal's subscribers.
    ///
    /// The resolved unit is *cell* size, not total pixels: the authoritative
    /// grid from [`Self::resolve_terminal_geometry`] may match no single
    /// client's viewport, so the Terminal's pixel size is `cells x cell size`
    /// computed at the point of use. That keeps the kernel-reported geometry
    /// self-consistent (`ws_xpixel / ws_col` is exactly the cell width —
    /// the division `kitten icat`-style preflights perform).
    ///
    /// Recency — not the `window-size` policy — picks the donor viewport:
    /// cell pixel size is a property of one physical display, and min/max
    /// over mixed-DPI viewports would synthesize a cell belonging to no real
    /// screen. `None` until some subscriber announces a viewport with usable
    /// pixel metrics; callers then leave the Terminal's pixel state alone.
    #[must_use]
    pub fn resolve_terminal_cell_px(&self, terminal: TerminalId) -> Option<(u16, u16)> {
        self.subscribers_for_terminal(terminal)
            .iter()
            .filter_map(|cid| self.clients.attached.get(cid))
            .filter_map(|c| Some((c.viewport_seq, viewport_cell_px(c.viewport.as_ref()?)?)))
            .max_by_key(|&(seq, _)| seq)
            .map(|(_, cell)| cell)
    }
}
