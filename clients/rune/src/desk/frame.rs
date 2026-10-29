//! The shown terminal's frame: its size, the drawn receipt that clears a
//! delivery fence, and the cursor's blink.

use std::sync::Arc;
use std::time::Duration;

use phux_client_runtime::control::Status;
use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, ViewId};
use rune::Tx;

use super::{Desk, Painted};

impl Desk {
    /// Ghostty's cursor blink: 600 ms phases while the window has focus and
    /// the program asked for a blinking cursor, restarting on typing. Wakes
    /// only for the next flip, so an idle window draws two frames a second.
    pub(super) fn blink(&mut self, client: &Client, tx: &mut Tx) -> bool {
        const PHASE: f64 = 0.6;
        let blinking = self.focused
            && self
                .shown
                .as_ref()
                .and_then(|s| client.acquire_view(s.view))
                .is_some_and(|f| f.cursor.blinking && f.cursor.visible);
        let on = if blinking {
            let elapsed = (self.clock - self.blink_since).max(0.0);
            let into = elapsed % (2.0 * PHASE);
            let next = if into < PHASE {
                PHASE - into
            } else {
                2.0 * PHASE - into
            };
            tx.wake_after(Duration::from_secs_f64(next.max(0.01)));
            into < PHASE
        } else {
            true
        };
        let changed = on != self.blink_on;
        self.blink_on = on;
        if changed {
            self.generation += 1;
        }
        changed
    }

    /// Typing shows the cursor and restarts its blink.
    pub(super) fn restart_blink(&mut self) {
        self.blink_since = self.clock;
        self.blink_on = true;
    }

    /// The grid area moved or resized.
    pub fn laid_out(&mut self, area: [f32; 2]) {
        self.area = area;
        self.fit();
    }

    /// Propose the shown terminal's size from the grid area. The runtime
    /// refuses it for observers and before the terminal is ready; a later
    /// drain retries.
    pub(super) fn fit(&mut self) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        if self.area[0] < 1.0 || self.area[1] < 1.0 {
            return;
        }
        let metrics = self.metrics();
        let size = metrics.fit(self.area);
        let cell = metrics.cell_px();
        if self
            .fitted
            .as_ref()
            .is_some_and(|(id, fitted, px)| id == &shown.terminal && *fitted == size && *px == cell)
        {
            return;
        }
        let outcome = client.with_control(|control| {
            control.resize_terminal_with_cell(
                &shown.terminal,
                size.0.into(),
                size.1.into(),
                Some(cell),
            )
        });
        if outcome == phux_client_runtime::control::TerminalResizeOutcome::Queued {
            self.fitted = Some((shown.terminal.clone(), size, cell));
        }
    }

    /// Clear the delivery fence of the frame Rune just drew, only when that
    /// exact publication is still current on the same connection. This is
    /// a next-frame receipt; it never clears a newer fence.
    pub fn drawn(&self) {
        let Some(painted) = self.painted.take() else {
            return;
        };
        let Some(client) = &self.client else { return };
        if client.connection_epoch() != painted.epoch {
            return;
        }
        let current = client.acquire_view(painted.view);
        if !current.is_some_and(|frame| Arc::ptr_eq(&frame, &painted.frame)) {
            return;
        }
        client.with_control(|control| {
            control.acknowledge_projection_if(&painted.terminal, painted.fence)
        });
    }

    /// Note the frame a view build is about to paint, when it can clear a
    /// fence: attached, and at the live tail the input landed in.
    pub fn painting(&self, view: ViewId, frame: &Arc<GridFrame>) {
        let painted = self.client.as_ref().and_then(|client| {
            if self.status != Status::Attached || !frame.scrollbar.at_tail() {
                return None;
            }
            let fence =
                client.with_control(|control| control.projection_fence(&frame.terminal_id))?;
            Some(Painted {
                terminal: frame.terminal_id.clone(),
                view,
                fence,
                frame: Arc::clone(frame),
                epoch: client.connection_epoch(),
            })
        });
        self.painted.replace(painted);
    }
}
