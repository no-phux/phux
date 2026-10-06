use phux_core::ids::ResourceId;

use super::{ClientId, HEADLESS_TERMINAL_DIMS, ServerState};
use crate::terminal_actor::ResizeRequest;

/// Cell pixel size implied by one viewport report (`pixel / cells`); `None`
/// without metrics or for degenerate ones (sub-pixel cells).
fn viewport_cell_px(v: &phux_protocol::wire::frame::ViewportInfo) -> Option<(u16, u16)> {
    if v.cols == 0 || v.rows == 0 {
        return None;
    }
    let w = v.pixel_w? / v.cols;
    let h = v.pixel_h? / v.rows;
    (w > 0 && h > 0).then_some((w, h))
}

/// The smallest grid a vote-free detach may leave an unwatched Terminal at
/// (ADR-0145): 10 columns fit a short prompt and a typed command, and 3 rows
/// fit the prompt, its output, and the next prompt. A tile below this only
/// comes from a degenerate outer window, never a usable layout.
pub(crate) const MIN_USABLE_TERMINAL_DIMS: (u16, u16) = (10, 3);

impl ServerState {
    /// Subscribed panes of the client's attached session. Foreign resource
    /// subscriptions, including geometry-preserving views, do not become
    /// viewport targets. Authorization uses this same target set.
    #[must_use]
    pub(crate) fn viewport_terminals(&self, client: ClientId) -> Vec<ResourceId> {
        let Some(attached) = self.clients.attached.get(&client) else {
            return Vec::new();
        };
        self.session_terminals(attached.session)
            .into_iter()
            .filter(|terminal| self.subscribers_for_terminal(*terminal).contains(&client))
            .collect()
    }

    /// Record `client`'s current outer viewport (`phux-nk07`), as carried by
    /// `ATTACH` or a live `VIEWPORT_RESIZE`. No-op for an unattached client.
    pub fn set_client_viewport(
        &mut self,
        client: ClientId,
        viewport: phux_protocol::wire::frame::ViewportInfo,
    ) {
        // Direct fields, so the borrow splits from `lifecycle`; stamp only an
        // attached client's announcement.
        if let Some(c) = self.clients.attached.get_mut(&client) {
            self.lifecycle.viewport_clock += 1;
            c.viewport = Some(viewport);
            c.viewport_seq = self.lifecycle.viewport_clock;
        }
    }

    /// The authoritative `(cols, rows)` for a Terminal from its subscribers'
    /// viewports and the `window-size` policy. `None` under `Manual` or with
    /// no usable viewport; zero dimensions are ignored. `latest` serves the
    /// `Latest` policy.
    #[must_use]
    pub fn resolve_terminal_geometry(
        &self,
        terminal: ResourceId,
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

    /// The cell pixel size a Terminal should report, from the most recent
    /// usable report among its subscribers (recency, not policy: a cell size
    /// belongs to one display). Pixels are then `cells x cell size`, keeping
    /// `ws_xpixel / ws_col` exact. `None` until some report has metrics.
    #[must_use]
    pub fn resolve_terminal_cell_px(&self, terminal: ResourceId) -> Option<(u16, u16)> {
        self.subscribers_for_terminal(terminal)
            .iter()
            .filter_map(|cid| self.clients.attached.get(cid))
            .filter_map(|c| Some((c.viewport_seq, viewport_cell_px(c.viewport.as_ref()?)?)))
            .max_by_key(|&(seq, _)| seq)
            .map(|(_, cell)| cell)
    }

    /// Recompute a departed view's Terminals; with no usable viewport left
    /// they return to the headless size. `Manual` is left alone.
    ///
    /// A view that cast no vote re-resolves nothing (L1 §9.2), so its
    /// explicit sizes outlive it. The exception (ADR-0145) is a Terminal it
    /// leaves with no subscriber at all and smaller than
    /// [`MIN_USABLE_TERMINAL_DIMS`] on either axis. Unwatched at that size,
    /// a shell cannot show a prompt and a command, so it returns to the
    /// headless size. A normally sized pane is never touched.
    pub(super) fn restore_terminal_geometry_after_detach(
        &mut self,
        terminals: Vec<ResourceId>,
        voted: bool,
    ) {
        if self.config.window_size == phux_config::WindowSize::Manual {
            return;
        }
        for terminal in terminals {
            if !voted && !self.is_stranded_below_usable(terminal) {
                continue;
            }
            let latest = self.latest_terminal_viewport(terminal);
            let (cols, rows) = self
                .resolve_terminal_geometry(terminal, latest)
                .unwrap_or(HEADLESS_TERMINAL_DIMS);
            let cell_px = self.resolve_terminal_cell_px(terminal);
            if self.retained_exit(terminal).is_some() {
                continue;
            }
            let Some(Ok(handle)) = self.resource_handle(terminal).map(|h| h.terminal()) else {
                continue;
            };
            if handle
                .resize
                .try_send(ResizeRequest {
                    cols,
                    rows,
                    cell_px,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .is_ok()
                && let Some(pane) = self.registry_mut().terminal_mut(terminal)
            {
                pane.dims = (cols, rows);
            }
        }
    }

    /// No subscriber is left to size `terminal`, and it is smaller than
    /// [`MIN_USABLE_TERMINAL_DIMS`] on either axis.
    fn is_stranded_below_usable(&self, terminal: ResourceId) -> bool {
        let (min_cols, min_rows) = MIN_USABLE_TERMINAL_DIMS;
        self.subscribers_for_terminal(terminal).is_empty()
            && self
                .registry()
                .terminal(terminal)
                .is_some_and(|pane| pane.dims.0 < min_cols || pane.dims.1 < min_rows)
    }

    fn session_terminals(&self, session: phux_core::ids::SessionId) -> Vec<ResourceId> {
        self.registry()
            .session(session)
            .into_iter()
            .flat_map(|session| session.windows.iter())
            .filter_map(|window| self.registry().window(*window))
            .flat_map(|window| window.slots.iter().copied())
            .collect()
    }

    fn latest_terminal_viewport(
        &self,
        terminal: ResourceId,
    ) -> Option<phux_protocol::wire::frame::ViewportInfo> {
        self.subscribers_for_terminal(terminal)
            .iter()
            .filter_map(|client| self.clients.attached.get(client))
            .filter_map(|client| Some((client.viewport_seq, client.viewport?)))
            .filter(|(_, viewport)| viewport.cols > 0 && viewport.rows > 0)
            .max_by_key(|(seq, _)| *seq)
            .map(|(_, viewport)| viewport)
    }
}
