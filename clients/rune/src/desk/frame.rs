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

/// How long the title bar shows the terminal's size after it changes.
const SIZE_GLANCE: f64 = 1.2;

impl Desk {
    /// Ghostty's cursor blink: 600 ms phases while the window has focus and
    /// the program asked for a blinking cursor, restarting on typing. Wakes
    /// only for the next flip, so an idle window draws two frames a second.
    /// A connection that is down holds the cursor steady: a blink would make
    /// a screen that has stopped look alive, and redraw it for nothing.
    pub(super) fn blink(&mut self, client: &Client, tx: &mut Tx) -> bool {
        const PHASE: f64 = 0.6;
        let blinking = self.focused
            && self.link_is_up()
            && self
                .keyboard()
                .and_then(|(_, view)| client.acquire_view(view?))
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

    /// The terminal's size shows in the title bar only while the window is
    /// resized, as Ghostty's resize overlay does: for [`SIZE_GLANCE`]
    /// seconds after the last change, never at rest or for a launch's fit.
    /// Remembers the grid it shows and wakes to hide it.
    pub(super) fn glance_size(&mut self, client: &Client, tx: &mut Tx) -> bool {
        let size = self
            .connection
            .shown
            .as_ref()
            .and_then(|shown| client.acquire_view(shown.view?))
            .map(|frame| (frame.cols, frame.rows));
        let changed = size.is_some() && size != self.size_seen;
        if size.is_some() {
            self.size_seen = size;
        }
        if self.size_glance().is_some() {
            tx.wake_after(Duration::from_secs_f64(
                (self.size_until - self.clock).max(0.01),
            ));
        }
        let shown = self.size_glance().is_some();
        let flipped = shown != self.size_was_shown;
        self.size_was_shown = shown;
        (changed && shown) || flipped
    }

    /// The window changed size: show the terminal's size for a moment.
    pub(crate) fn glance_resize(&mut self) {
        self.size_until = self.clock + SIZE_GLANCE;
    }

    /// The grid size to show in the title bar, while a resize is fresh.
    pub fn size_glance(&self) -> Option<(u16, u16)> {
        self.size_seen.filter(|_| self.clock < self.size_until)
    }

    /// The shown terminal's grid size as last seen.
    pub fn last_size(&self) -> Option<(u16, u16)> {
        self.size_seen
    }

    /// Typing shows the cursor and restarts its blink.
    pub(crate) fn restart_blink(&mut self) {
        self.blink_since = self.clock;
        self.blink_on = true;
    }

    /// A pane's grid area in points, from its last layout.
    pub fn pane_area(&self, terminal: &ResourceId) -> Option<[f32; 2]> {
        self.connection.areas.get(terminal).copied()
    }

    /// Output that arrived below a pane scrolled into history.
    pub fn unread_rows(&self, terminal: &ResourceId) -> u64 {
        let info = self
            .connection
            .client
            .as_ref()
            .and_then(|c| c.engine().zip(self.view_of(terminal)))
            .and_then(|(engine, view)| engine.view_replica_info(view).ok());
        info.and_then(|i| i.history).map_or(0, |h| h.unread_rows)
    }

    /// A pane's grid moved or resized: `[x, y, w, h]` in window points.
    pub fn laid_out(&mut self, terminal: ResourceId, [x, y, w, h]: [f32; 4]) {
        if self.phone.is_some()
            && !self
                .connection
                .shown
                .as_ref()
                .is_some_and(|s| s.resource == terminal)
        {
            return;
        }
        self.connection.origins.insert(terminal.clone(), [x, y]);
        self.connection.areas.insert(terminal, [w, h]);
        self.fit();
    }

    /// Propose every pane's size from its grid area. The runtime refuses it
    /// for observers and before a terminal is ready; a later drain retries.
    pub(super) fn fit(&mut self) {
        // A divider on the move resizes only what is drawn; the programs get
        // one resize on release, not a redraw per frame of the drag.
        if self.connection.dragging.is_some() {
            return;
        }
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let mut on_screen: Vec<ResourceId> = if self.phone.is_some() {
            Vec::new()
        } else {
            self.connection.others.keys().cloned().collect()
        };
        on_screen.extend(self.connection.shown.as_ref().map(|s| s.resource.clone()));
        on_screen.extend(self.connection.popup.terminal().cloned());
        let metrics = self.metrics();
        let cell = metrics.cell_px();
        for terminal in on_screen {
            self.fit_terminal(&client, terminal, &metrics, cell);
        }
    }

    /// Using this window takes back panes another client resized (a phone,
    /// say), as typing on the phone does: the size follows whoever is
    /// driving. A fit the live grid no longer shows is forgotten and re-sent.
    pub(super) fn reclaim_fits(&mut self) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let stale: Vec<ResourceId> = self
            .connection
            .fitted
            .iter()
            .filter(|(terminal, (size, _))| {
                self.view_of(terminal)
                    .and_then(|view| client.acquire_view(view))
                    .is_some_and(|frame| (frame.cols, frame.rows) != *size)
            })
            .map(|(terminal, _)| terminal.clone())
            .collect();
        if stale.is_empty() {
            return;
        }
        for terminal in &stale {
            self.connection.fitted.remove(terminal);
        }
        self.fit();
    }

    fn fit_terminal(
        &mut self,
        client: &Client,
        terminal: ResourceId,
        metrics: &crate::grid::Metrics,
        cell: (u16, u16),
    ) {
        if self.view_of(&terminal).is_none() {
            return;
        }
        let area = self.connection.areas.get(&terminal).copied();
        let Some(area) = area.or_else(|| self.slot_area(&terminal)) else {
            return;
        };
        if area[0] < 1.0 || area[1] < 1.0 {
            return;
        }
        let size = metrics.fit(area);
        if self.defer_phone_fit(&terminal, (size, cell))
            || self.connection.fitted.get(&terminal) == Some(&(size, cell))
        {
            return;
        }
        let outcome = client.with_control(|control| {
            control.resize_terminal_with_cell(&terminal, size.0.into(), size.1.into(), Some(cell))
        });
        if outcome == phux_client_runtime::control::TerminalResizeOutcome::Queued {
            self.connection.fitted.insert(terminal, (size, cell));
        }
    }

    /// Clear the delivery fence of the frame Rune just drew, only when that
    /// exact publication is still current on the same connection. This is
    /// a next-frame receipt; it never clears a newer fence.
    pub fn drawn(&self) {
        let Some(painted) = self.connection.painted.take() else {
            return;
        };
        let Some(client) = &self.connection.client else {
            return;
        };
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
        let painted = self.connection.client.as_ref().and_then(|client| {
            if self.connection.status != Status::Attached || !frame.scrollbar.at_tail() {
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
        self.connection.painted.replace(painted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_bar_shows_the_size_only_while_the_window_resizes() {
        let (client, mut rt) = crate::test_support::input_fixture_showing("");
        rt.frame(0.0);
        let mut at = |clock: f64| {
            rt.update(|desk, tx| {
                desk.clock = clock;
                desk.glance_size(&client, tx);
            });
            rt.state.size_glance()
        };
        // Attaching and the launch's first fits say nothing.
        assert_eq!(at(0.5), None);
        assert!(rt.state.last_size().is_some());
        rt.state.window_laid_out([1100.0, 720.0]);
        assert_eq!(
            rt.state.size_glance(),
            None,
            "the first layout is no resize"
        );
        rt.state.window_laid_out([900.0, 720.0]);
        assert_eq!(rt.state.size_glance(), rt.state.last_size());
        let mut at = |clock: f64| {
            rt.update(|desk, tx| {
                desk.clock = clock;
                desk.glance_size(&client, tx);
            });
            rt.state.size_glance()
        };
        assert!(at(1.0).is_some());
        assert_eq!(at(0.5 + SIZE_GLANCE + 0.01), None, "and goes once it rests");
    }

    #[test]
    fn the_cursor_blinks_only_while_the_connection_is_up() {
        // DECSCUSR 1: a blinking block.
        let (client, mut rt) = crate::test_support::input_fixture_showing("\x1b[1 q");
        rt.frame(0.0);
        let mut blink_at = |clock: f64, status: Status| {
            rt.update(|desk, tx| {
                desk.clock = clock;
                desk.connection.status = status;
                desk.blink(&client, tx);
            });
            (rt.state.blink_on, rt.state.generation)
        };
        let (on, drawn) = blink_at(0.1, Status::Attached);
        assert!(on);
        // The dark phase of a live cursor.
        let (on, dark) = blink_at(0.7, Status::Attached);
        assert!(!on && dark > drawn);
        // The link drops mid-phase: the cursor comes back and stays.
        let (on, steady) = blink_at(0.8, Status::Connecting);
        assert!(on && steady > dark);
        for clock in [1.3, 1.9, 2.5, 60.7] {
            assert_eq!(blink_at(clock, Status::Connecting), (true, steady));
        }
        // Up again, it blinks again.
        assert!(!blink_at(61.9, Status::Attached).0);
    }
}
