//! Fitting a pane's capacity to its terminal: the settled PTY resize
//! proposal and the visible-owner claim that admits it.

use super::*;

/// How long a pane's capacity must hold still before it becomes a PTY resize.
/// Layout settling, window drags and split drags produce bursts of sizes;
/// every intermediate resize makes a shell redraw its prompt into history.
/// A settling proposal requests frames, so the final size always goes out.
pub(super) const FIT_SETTLE: Duration = Duration::from_millis(60);

/// Server `DEFAULT_CELL_PX` and the TUI `HOST_CELL_PX_FALLBACK`. Desktop must
/// not import that private constant. A first look at this pitch sends nothing.
const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

/// `(cols, rows)` plus the integer cell pitch reported with them.
pub(super) type FitProposal = ((u16, u16), (u16, u16));

/// A proposal waiting out [`FIT_SETTLE`], armed for one presentation generation.
#[derive(Clone, Copy)]
pub(super) struct PendingFit {
    proposal: FitProposal,
    pub(super) since: Instant,
    identity: Identity,
}

/// Token of the view that last painted as the visible size owner.
///
/// Hiding releases paint state and keeps this input, including a fit timer.
/// The timer delivers only while the token still matches, so a hidden former
/// owner cannot resize over the view that has since taken ownership.
fn fit_claims() -> MutexGuard<'static, BTreeMap<(usize, ResourceId), u64>> {
    static CLAIMS: Mutex<BTreeMap<(usize, ResourceId), u64>> = Mutex::new(BTreeMap::new());
    CLAIMS.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn next_fit_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn claim_key(client: &Arc<Client>, terminal: &ResourceId) -> (usize, ResourceId) {
    (Arc::as_ptr(client) as usize, terminal.clone())
}

/// Whether a settled fit may still be delivered. `claimed` is the visible
/// size owner's token and `live` is the generation published now.
pub(super) fn admit_delayed_fit(
    claimed: Option<u64>,
    token: u64,
    armed: Identity,
    live: Option<Identity>,
) -> bool {
    claimed == Some(token) && live == Some(armed)
}

impl TerminalInput {
    /// Propose this pane's cell capacity and integer cell pitch as the
    /// terminal's PTY geometry while this view owns it (desktop architecture,
    /// "Geometry"). The shell names one owner per terminal: its only view, or
    /// the view the user focused last. Other views crop or leave space.
    /// Proposals are edge-triggered: an unchanged proposal sends nothing, so
    /// views cannot oscillate. Gaining ownership or re-activating the window
    /// forgets the last proposal, so the owner reclaims a size another client
    /// changed. The first look also sends nothing when the grid already matches
    /// and the pitch is the server default of 8×16.
    ///
    /// A timer, not the next frame, sends the settled size: a background or
    /// occluded window may not paint again, and the resize must still go out.
    /// Hiding keeps this input and that timer. Delivery rechecks that this
    /// view is still the visible size owner of the generation that armed the
    /// proposal, so a hidden former owner cannot override the focused view.
    ///
    /// The painted cell pitch is not that proposal. The first time it changes,
    /// it is stored on the local replica immediately, for the size owner and
    /// for every other view, before settle, readiness, or the viewer gate.
    /// Unchanged paints do not republish. The server still learns the pitch
    /// only from the settled resize.
    pub fn fit(&mut self, owner: bool, window: &Window, cx: &mut Context<Self>) {
        self.apply_local_cell_pixels();
        let active = window.is_window_active();
        let reactivated = active && !self.fit_active;
        self.fit_active = active;
        if !owner {
            self.fitted = None;
            self.revoke_fit();
            return;
        }
        // Claim before deciding whether a resize is needed, so a focused view
        // whose grid already fits still retires a hidden owner's pending fit.
        self.claim_fit();
        if reactivated {
            self.fitted = None;
        }
        let Some(metrics) = self.metrics else {
            return;
        };
        let Some(capacity) = capacity(metrics) else {
            return;
        };
        let cell = cell_px(metrics);
        let Some(proposal) = pending_fit(self.fitted, capacity, cell, self.dimensions) else {
            self.fitted = Some((capacity, cell));
            self.settling = None;
            return;
        };
        if self
            .settling
            .is_some_and(|pending| pending.proposal == proposal)
        {
            return;
        }
        let Some(identity) = self.identity else {
            self.settling = None;
            return;
        };
        self.settling = Some(PendingFit {
            proposal,
            since: Instant::now(),
            identity,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIT_SETTLE).await;
            let _ = this.update(cx, |state, _| state.flush_fit());
        })
        .detach();
    }

    /// Send the pending proposal once it has held still for `FIT_SETTLE`. A
    /// newer proposal restarts the clock and its own timer sends it.
    ///
    /// Admission is the current visible owner and the armed generation.
    /// Nothing here acknowledges an unknown-delivery fence. A viewer role
    /// is refused by the resize itself, which sends nothing.
    pub(super) fn flush_fit(&mut self) {
        let Some(pending) = self.settling else {
            return;
        };
        if pending.since.elapsed() < FIT_SETTLE {
            return;
        }
        let Ok(client) = self.client() else {
            return;
        };
        let claimed = self.claimed_token(&client);
        let outcome = client.with_control(|control| {
            let live = self
                .live_frame(control)
                .ok()
                .map(|frame| Identity::of(control.connection_epoch(), &frame));
            if !admit_delayed_fit(claimed, self.fit_token, pending.identity, live) {
                return None;
            }
            let ((cols, rows), cell_px) = pending.proposal;
            Some(control.resize_terminal_with_cell(
                &self.terminal,
                cols.into(),
                rows.into(),
                Some(cell_px),
            ))
        });
        if outcome == Some(TerminalResizeOutcome::Queued) {
            self.fitted = Some(pending.proposal);
        }
        self.settling = None;
    }

    /// Store this paint's cell pitch locally when it changed. No server frame.
    fn apply_local_cell_pixels(&mut self) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let cell = cell_px(metrics);
        if self.applied_cell == Some(cell) {
            return;
        }
        let Ok(client) = self.client() else {
            return;
        };
        let stored = client.with_control(|control| {
            if control.engine().is_none() {
                return false;
            }
            control.set_local_cell_pixels(&self.terminal, cell.0, cell.1);
            true
        });
        if stored {
            self.applied_cell = Some(cell);
        }
    }

    fn claim_fit(&self) {
        if let Ok(client) = self.client() {
            fit_claims().insert(claim_key(&client, &self.terminal), self.fit_token);
        }
    }

    fn claimed_token(&self, client: &Arc<Client>) -> Option<u64> {
        fit_claims()
            .get(&claim_key(client, &self.terminal))
            .copied()
    }

    /// Drop a pending fit and release visible ownership when this input holds it.
    pub(super) fn revoke_fit(&mut self) {
        self.settling = None;
        self.release_fit_claim();
    }

    pub(super) fn release_fit_claim(&mut self) {
        let mut claims = fit_claims();
        if let Ok(client) = self.client() {
            let key = claim_key(&client, &self.terminal);
            if claims.get(&key) == Some(&self.fit_token) {
                claims.remove(&key);
            }
            return;
        }
        claims.retain(|_, token| *token != self.fit_token);
    }
}

/// Whole cells that fit the painted bounds. `None` for a collapsed pane.
pub(super) fn capacity(metrics: InputMetrics) -> Option<(u16, u16)> {
    let cols = (metrics.bounds.size.width / metrics.cell_width).floor();
    let rows = (metrics.bounds.size.height / metrics.line_height).floor();
    let clamp = |cells: f32| cells.min(f32::from(u16::MAX)) as u16;
    (cols >= 2.0 && rows >= 1.0).then(|| (clamp(cols), clamp(rows)))
}

pub(super) fn cell_px(metrics: InputMetrics) -> (u16, u16) {
    crate::terminal::geometry::cell_px(metrics.cell_width, metrics.line_height)
}

/// `None` when this proposal was already sent, or on the first look when the
/// grid matches and the pitch is [`DEFAULT_CELL_PX`]. Any other pitch is
/// reported so mouse encoding and image geometry share one cell. Pixel-only
/// changes preserve native history; only a text-grid reflow retires its cursors.
pub(super) fn pending_fit(
    fitted: Option<FitProposal>,
    capacity: (u16, u16),
    cell_px: (u16, u16),
    dimensions: (u16, u16),
) -> Option<FitProposal> {
    let proposal = (capacity, cell_px);
    if fitted == Some(proposal) {
        return None;
    }
    if fitted.is_none() && capacity == dimensions && cell_px == DEFAULT_CELL_PX {
        return None;
    }
    Some(proposal)
}
