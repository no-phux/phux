//! The shown terminal's frame: its size, the drawn receipt that clears a
//! delivery fence, and the cursor's blink.

use std::sync::Arc;
use std::time::Duration;

use phux_client_runtime::control::Status;
use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, ViewId};
use phux_protocol::ids::ResourceId;
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

    /// A pane's grid area in points, from its last layout.
    pub fn pane_area(&self, terminal: &ResourceId) -> Option<[f32; 2]> {
        self.areas.get(terminal).copied()
    }

    /// Output that arrived below a pane scrolled into history.
    pub fn unread_rows(&self, terminal: &ResourceId) -> u64 {
        let info = self
            .client
            .as_ref()
            .and_then(|c| c.engine().zip(self.view_of(terminal)))
            .and_then(|(engine, view)| engine.view_replica_info(view).ok());
        info.and_then(|i| i.history).map_or(0, |h| h.unread_rows)
    }

    /// A pane's grid area moved or resized.
    pub fn laid_out(&mut self, terminal: ResourceId, area: [f32; 2]) {
        self.areas.insert(terminal, area);
        self.fit();
    }

    /// Propose every pane's size from its grid area. The runtime refuses it
    /// for observers and before a terminal is ready; a later drain retries.
    pub(super) fn fit(&mut self) {
        // A divider on the move resizes only what is drawn; the programs get
        // one resize on release, not a redraw per frame of the drag.
        if self.dragging.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let mut on_screen: Vec<ResourceId> = self.others.keys().cloned().collect();
        on_screen.extend(self.shown.as_ref().map(|s| s.terminal.clone()));
        let metrics = self.metrics();
        let cell = metrics.cell_px();
        for terminal in on_screen {
            let Some(area) = self.areas.get(&terminal).copied() else {
                continue;
            };
            if area[0] < 1.0 || area[1] < 1.0 {
                continue;
            }
            let size = metrics.fit(area);
            if self.fitted.get(&terminal) == Some(&(size, cell)) {
                continue;
            }
            let outcome = client.with_control(|control| {
                control.resize_terminal_with_cell(
                    &terminal,
                    size.0.into(),
                    size.1.into(),
                    Some(cell),
                )
            });
            if outcome == phux_client_runtime::control::TerminalResizeOutcome::Queued {
                self.fitted.insert(terminal, (size, cell));
            }
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
