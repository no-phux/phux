//! The focused view: grid, viewport and scrolling, input frames, and history demand.

use super::*;

impl Connection {
    /// Current styled grid from a published replica, or the initial blank grid.
    #[must_use]
    pub fn grid(&self) -> Grid {
        self.published_terminal()
            .map_or_else(|| self.blank.grid(), |terminal| terminal.grid())
    }

    /// Whether canvas paint is allowed past the aggregate first-damage barrier.
    #[must_use]
    pub const fn render_visible(&self) -> bool {
        self.render_visible
    }

    /// Current published grid dimensions in cells.
    #[must_use]
    pub fn dims(&self) -> (u16, u16) {
        self.published_geometry()
            .map_or((self.cols, self.rows), |geometry| {
                (geometry.cols, geometry.rows)
            })
    }

    /// Scroll the focused replica's viewport `rows` rows (negative is up,
    /// into scrollback). Local only: the server's pane never moves. Returns
    /// whether a published replica took the scroll.
    pub fn scroll_viewport(&self, rows: i32) -> bool {
        let Some(terminal) = self.published_terminal() else {
            return false;
        };
        terminal.scroll_viewport(rows);
        true
    }

    /// Return a scrolled-back viewport to the live screen. Returns whether
    /// it moved (and so needs a repaint).
    pub fn scroll_to_bottom(&self) -> bool {
        match self.published_terminal() {
            Some(terminal) if terminal.viewport_scrolled() => {
                terminal.scroll_to_bottom();
                true
            }
            _ => false,
        }
    }

    /// The title the focused replica's program set (OSC 0/2), empty if none.
    #[must_use]
    pub fn title(&self) -> String {
        self.published_terminal()
            .map(Terminal::title)
            .unwrap_or_default()
    }

    /// Whether the focused replica's viewport is scrolled back.
    #[must_use]
    pub fn viewport_scrolled(&self) -> bool {
        self.published_terminal()
            .is_some_and(Terminal::viewport_scrolled)
    }

    /// Record a new viewport and, once the handshake is done, encode the
    /// `VIEWPORT_RESIZE` announcing it. Before `HELLO_OK` the `ATTACH`
    /// carries the new size instead, so no frame is needed; an unchanged
    /// size sends nothing. The server resizes the pane (subject to its
    /// multi-client size policy) and the replica follows its re-bootstrap.
    pub fn resize_frame(&mut self, cols: u16, rows: u16) -> Option<Vec<u8>> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.failed || (cols, rows) == (self.cols, self.rows) {
            return None;
        }
        self.cols = cols;
        self.rows = rows;
        self.kernel.as_ref()?;
        Some(encode(&FrameKind::ViewportResize {
            viewport: self.viewport(),
        }))
    }

    /// Set the pixel size of one cell as the client draws it and reports
    /// pointer positions in. The next `ATTACH` or `VIEWPORT_RESIZE` reports
    /// the viewport's size in those pixels, from which the server sizes the
    /// cells its mouse encoder divides positions by (SPEC L1 §9.2.1);
    /// without it the server uses another client's cells or its 8x16
    /// default.
    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell_px = Some((width.max(1), height.max(1)));
    }

    /// The viewport as `ATTACH` and `VIEWPORT_RESIZE` report it: cells, and
    /// pixels when the cell size is known and the grid fits the wire's u16.
    pub(super) fn viewport(&self) -> ViewportInfo {
        let pixels = self.cell_px.and_then(|(width, height)| {
            Some((
                self.cols.checked_mul(width)?,
                self.rows.checked_mul(height)?,
            ))
        });
        ViewportInfo::new(self.cols, self.rows).with_pixels(
            pixels.map(|(width, _)| width),
            pixels.map(|(_, height)| height),
        )
    }

    /// Encode an eligible structured key event for the focused published pane.
    #[must_use]
    pub fn key_frame(&mut self, event: KeyEvent) -> Option<Vec<u8>> {
        self.input_frame(InputEvent::Key(event))
    }

    /// Encode an eligible input atom (key, paste, ...) for the focused
    /// published pane; `None` while no pane is eligible for input.
    #[must_use]
    pub fn input_frame(&mut self, event: InputEvent) -> Option<Vec<u8>> {
        if self.failed {
            return None;
        }
        let terminal_id = self.first_published_terminal()?;
        let kernel = self.kernel.as_ref()?;
        if !matches!(
            kernel.input_eligibility(&terminal_id),
            InputEligibility::Eligible { .. }
        ) {
            return None;
        }
        let (outcome, applied) = self.apply_kernel(KernelInput::Action(KernelAction::Input {
            terminal_id: &terminal_id,
            event: &event,
        }));
        if applied {
            outcome.send.into_iter().next()
        } else {
            None
        }
    }

    pub(crate) fn pane_focus_frames(&mut self, previous: Option<ResourceId>) -> Vec<Vec<u8>> {
        use phux_protocol::input::focus::FocusEvent;
        let mut frames = Vec::new();
        for (id, focus) in [
            (previous, FocusEvent::Lost),
            (self.focused_pane(), FocusEvent::Gained),
        ] {
            let Some(id) = id else {
                continue;
            };
            if !self
                .pane_terminal(&id)
                .is_some_and(|terminal| terminal.dec_mode(1004))
            {
                continue;
            }
            let event = InputEvent::Focus(focus);
            let (outcome, _) = self.apply_kernel(KernelInput::Action(KernelAction::Input {
                terminal_id: &id,
                event: &event,
            }));
            frames.extend(outcome.send);
        }
        frames
    }

    pub(super) fn apply_kernel(&mut self, input: KernelInput<'_>) -> (Outcome, bool) {
        let Some(kernel) = self.kernel.as_mut() else {
            self.effects.clear();
            return (
                self.protocol_failure("stateful frame arrived before HELLO_OK"),
                false,
            );
        };
        let result = kernel.update_at(browser_monotonic_ms(), input, &mut self.effects);
        let focused = self.focused_terminal.as_ref();
        let mut outcome = Outcome::default();
        for effect in self.effects.as_slice() {
            match effect {
                KernelEffect::Send(KernelSend::Input { terminal_id, event }) => {
                    outcome
                        .send
                        .push(encode(&(*event).clone().into_frame(terminal_id.clone())));
                }
                KernelEffect::Send(KernelSend::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                }) => outcome.send.push(encode(&FrameKind::FrameAck {
                    terminal_id: terminal_id.clone(),
                    stream_id: *stream_id,
                    bootstrap_id: *bootstrap_id,
                    seq: *seq,
                })),
                KernelEffect::Send(send @ KernelSend::HistoryRequest { .. }) => {
                    outcome.send.extend(history_request_frame(send));
                }
                KernelEffect::Send(KernelSend::SubscribeEvents {
                    terminal,
                    after_seq,
                }) => {
                    outcome.send.push(encode(&FrameKind::SubscribeEvents {
                        terminal: terminal.clone(),
                        after_seq: *after_seq,
                    }));
                }
                KernelEffect::Damage(_) => outcome.render = true,
                KernelEffect::AgentPublication(_) => outcome.badges = true,
                KernelEffect::Status(KernelStatus::Engine {
                    key,
                    status: EngineStatus::Bell,
                }) => {
                    if focused == Some(&key.terminal_id) || focused.is_none() {
                        outcome.bell = true;
                    }
                }
                KernelEffect::Status(_) | KernelEffect::Job(_) => {}
            }
        }
        if outcome.render {
            self.render_visible = true;
        }
        if outcome.fatal.is_some() {
            self.failed = true;
            return (outcome, false);
        }
        match result {
            Ok(()) => (outcome, true),
            Err(error) => {
                self.failed = true;
                outcome.fatal = Some(error.to_string());
                (outcome, false)
            }
        }
    }

    /// Retire expired staging; a failed new split never disconnects its siblings.
    pub fn expire_bootstrap_staging(&mut self) -> Outcome {
        let Some(kernel) = self.kernel.as_mut() else {
            self.effects.clear();
            return Outcome::default();
        };
        let expired = kernel.expire_bootstrap_staging(browser_monotonic_ms(), &mut self.effects);
        if expired == 0 {
            self.effects.clear();
            return Outcome::default();
        }
        let split = self
            .pending_split
            .as_ref()
            .and_then(|pending| pending.resource.as_ref());
        let only_split = self.effects.as_slice().iter().all(|effect| {
            !matches!(effect, KernelEffect::Status(KernelStatus::ResyncRequired { terminal_id, .. })
                if Some(terminal_id) != split)
        });
        self.effects.clear();
        if split.is_some() && only_split {
            self.pane_refusal("New pane bootstrap timed out. The other panes are still available; try splitting again.".to_owned())
        } else {
            self.protocol_failure("terminal bootstrap staging timed out")
        }
    }

    pub(super) fn protocol_failure(&mut self, message: &str) -> Outcome {
        self.fail_protocol(message);
        Outcome {
            fatal: Some(message.to_owned()),
            ..Outcome::default()
        }
    }

    /// Ask for older history once the focused viewport scrolls near the
    /// oldest loaded row: a native bootstrap delivers history a page at a
    /// time, on demand. Returns the `HISTORY_REQUEST` to send, if any; an
    /// in-flight request is not repeated.
    pub fn history_demand(&mut self) -> Vec<Vec<u8>> {
        let Some(terminal_id) = self.first_published_terminal() else {
            return Vec::new();
        };
        let Some(terminal) = self.published_terminal() else {
            return Vec::new();
        };
        let rows_from_oldest = usize::try_from(terminal.scrollbar().offset).unwrap_or(usize::MAX);
        let Some(kernel) = self.kernel.as_mut() else {
            return Vec::new();
        };
        self.effects.clear();
        kernel.prefetch_history(&terminal_id, rows_from_oldest, &mut self.effects);
        let frames = self
            .effects
            .as_slice()
            .iter()
            .filter_map(|effect| match effect {
                KernelEffect::Send(send) => history_request_frame(send),
                _ => None,
            })
            .collect();
        self.effects.clear();
        frames
    }

    pub(super) fn first_published_terminal(&self) -> Option<ResourceId> {
        let kernel = self.kernel.as_ref()?;
        if let Some(focused) = self.focused_terminal.as_ref()
            && kernel.published(focused).is_some()
        {
            return Some(focused.clone());
        }
        self.terminal_order
            .iter()
            .find(|terminal_id| kernel.published(terminal_id).is_some())
            .cloned()
    }

    /// The focused pane's published replica: what the canvas shows, and
    /// what search, copy, links, and mouse-mode checks read.
    #[must_use]
    pub fn terminal(&self) -> Option<&Terminal> {
        self.published_terminal()
    }

    /// The engine instance every replica of this connection runs on.
    #[must_use]
    pub fn vt(&self) -> &Rc<Vt> {
        &self.vt
    }

    fn published_terminal(&self) -> Option<&Terminal> {
        let terminal_id = self.first_published_terminal()?;
        let kernel = self.kernel.as_ref()?;
        kernel.published_engine(&terminal_id)?.terminal()
    }

    fn published_geometry(&self) -> Option<CanonicalGeometry> {
        let terminal_id = self.first_published_terminal()?;
        self.kernel
            .as_ref()?
            .published(&terminal_id)
            .map(|replica| replica.geometry())
    }
}
