//! Panes: geometry, focus, and the split, close, and kill requests.

use super::*;

impl Connection {
    /// Overall canvas dimensions, independent of the focused terminal.
    #[must_use]
    pub const fn canvas_dims(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Visible terminals and their local rectangles (at most four).
    #[must_use]
    pub fn pane_rects(&self) -> Vec<(ResourceId, PaneRect)> {
        let mut panes = Vec::with_capacity(4);
        if let Some(layout) = &self.layout {
            layout.rects(
                PaneRect {
                    x: 0,
                    y: 0,
                    cols: self.cols,
                    rows: self.rows,
                },
                &mut panes,
            );
        }
        panes
    }

    /// Current keyboard target, only after its replica has been published.
    #[must_use]
    pub fn focused_pane(&self) -> Option<ResourceId> {
        self.first_published_terminal()
    }

    /// Whether a server-authoritative split or kill is outstanding.
    #[must_use]
    pub fn pane_pending(&self) -> bool {
        self.pending_split.is_some() || self.pending_kill.is_some()
    }

    /// Last actionable pane refusal; does not fail the connection.
    #[must_use]
    pub fn pane_error(&self) -> Option<&str> {
        self.pane_error.as_deref()
    }

    pub(crate) fn set_pane_error(&mut self, message: String) {
        self.pane_error = Some(message);
    }

    /// Published engine for a resource.
    #[must_use]
    pub fn pane_terminal(&self, id: &ResourceId) -> Option<&Terminal> {
        self.kernel.as_ref()?.published_engine(id)?.terminal()
    }

    /// Focus one published, visible terminal.
    ///
    /// # Errors
    /// Refuses missing or bootstrapping terminals.
    pub fn focus_pane(&mut self, id: &ResourceId) -> Result<(), String> {
        if self.failed
            || !self.pane_rects().iter().any(|(pane, _)| pane == id)
            || self.pane_terminal(id).is_none()
        {
            return Err("That pane is not ready for input yet.".to_owned());
        }
        self.focused_terminal = Some(id.clone());
        self.cancel_path_query();
        self.pane_error = None;
        Ok(())
    }

    /// Cycle keyboard focus through visible, published terminals.
    ///
    /// # Errors
    /// Refuses when no terminal is ready.
    pub fn focus_next_pane(&mut self) -> Result<(), String> {
        let panes: Vec<_> = self
            .pane_rects()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| self.pane_terminal(id).is_some())
            .collect();
        if panes.is_empty() {
            return Err("No terminal is ready for input.".to_owned());
        }
        let index = panes
            .iter()
            .position(|id| Some(id) == self.focused_terminal.as_ref())
            .map_or(0, |index| (index + 1) % panes.len());
        self.focus_pane(&panes[index])
    }

    pub(super) fn next_pane_request(&mut self) -> u32 {
        self.pane_request_id = self.pane_request_id.wrapping_add(1).max(0x1000);
        self.pane_request_id
    }

    fn pane_operation_ready(&self) -> Result<(), String> {
        if self.failed || !self.attach_ready {
            return Err("Reconnect the session before changing panes.".to_owned());
        }
        if self.pane_pending() {
            return Err("Wait for the current pane operation to finish.".to_owned());
        }
        Ok(())
    }

    /// Request a real terminal spawn; commit layout only after bootstrap and ack.
    ///
    /// # Errors
    /// Refuses invalid axes, an unready connection, small panes, or a fifth pane.
    pub fn split_pane_frame(&mut self, axis: &str) -> Result<Vec<u8>, String> {
        let axis = Axis::parse(axis)?;
        self.pane_operation_ready()?;
        let panes = self.pane_rects();
        if panes.len() + self.retiring_panes.len() >= 4 {
            return Err("Four terminals are open or closing. Wait for cleanup or close a pane before splitting.".to_owned());
        }
        let target = self
            .focused_pane()
            .ok_or("No terminal is ready to split.")?;
        let rect = panes
            .iter()
            .find(|(id, _)| id == &target)
            .map(|(_, rect)| *rect)
            .ok_or("The focused terminal is not visible.")?;
        let (_, new_rect) = rect
            .split(axis)
            .ok_or("This pane is too small to split. Enlarge the terminal first.")?;
        let request_id = self.next_pane_request();
        self.pending_split = Some(PendingSplit {
            request_id,
            target: target.clone(),
            axis,
            resource: None,
            acknowledged: false,
        });
        self.pane_error = None;
        Ok(encode(&FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: target.host().cloned(),
            owner_terminal: Some(target),
            agent_session: None,
            initial_size: self
                .spawn_initial_size
                .then_some((new_rect.cols, new_rect.rows)),
            resource: None,
        }))
    }

    /// Close the focused pane's view. Presentation only: the terminal keeps
    /// running in its session and its stream stays attached; nothing is sent
    /// but the resizes of the panes that take over its space.
    ///
    /// # Errors
    /// Refuses the last view or a pending pane operation.
    pub fn close_view(&mut self) -> Result<Vec<Vec<u8>>, String> {
        self.pane_operation_ready()?;
        if self.pane_rects().len() <= 1 {
            return Err(
                "Cannot close the last view. Use Release Session to leave the session.".to_owned(),
            );
        }
        let terminal_id = self
            .focused_pane()
            .ok_or("No terminal is ready to close.")?;
        self.terminal_order.retain(|id| id != &terminal_id);
        self.layout = self
            .layout
            .take()
            .and_then(|layout| layout.remove(&terminal_id));
        self.pane_sizes.remove(&terminal_id);
        self.cancel_path_query();
        self.focused_terminal = None;
        self.focused_terminal = self.first_published_terminal();
        self.pane_error = None;
        Ok(self.pane_resize_frames())
    }

    /// Kill the focused terminal, ending its process; the pane stays visible
    /// until `RESOURCE_CLOSED`. A server advertising `ORGANIZATION` receives
    /// `RESOURCE_TERMINATE` for the terminal's current activation, read from
    /// the session's organization page first; any other server receives
    /// `KILL_RESOURCE`.
    ///
    /// # Errors
    /// Refuses the last pane or another pending operation.
    pub fn kill_pane_frame(&mut self) -> Result<Vec<u8>, String> {
        self.pane_operation_ready()?;
        if self.pane_rects().len() <= 1 {
            return Err(
                "Cannot kill the last pane. Use Release Session to end the session.".to_owned(),
            );
        }
        let terminal_id = self.focused_pane().ok_or("No terminal is ready to kill.")?;
        let request_id = self.next_pane_request();
        self.pane_error = None;
        if self.organization
            && let Some(session) = self.session_key
        {
            self.pending_kill = Some(PendingKill {
                request_id,
                target: terminal_id,
                resolving: Some(session),
            });
            return Ok(Self::organization_page_frame(request_id, session, None));
        }
        self.pending_kill = Some(PendingKill {
            request_id,
            target: terminal_id.clone(),
            resolving: None,
        });
        Ok(Self::legacy_kill_frame(request_id, terminal_id))
    }

    fn organization_page_frame(
        request_id: u32,
        session: SessionKey,
        cursor: Option<Vec<u8>>,
    ) -> Vec<u8> {
        encode(&FrameKind::Command {
            request_id,
            command: Command::GetOrganization {
                filter: OrgFilter::Session(session),
                page_size: 0,
                cursor,
            },
        })
    }

    pub(super) fn legacy_kill_frame(request_id: u32, terminal_id: ResourceId) -> Vec<u8> {
        encode(&FrameKind::Command {
            request_id,
            command: Command::KillResource {
                terminal_id,
                operation_id: None,
            },
        })
    }

    /// A fresh, non-zero operation id for one organization command.
    fn next_operation(&mut self) -> OperationId {
        self.operation_counter = self.operation_counter.wrapping_add(1).max(1);
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&self.operation_counter.to_le_bytes());
        // The counter keeps ids distinct on this connection; the random half
        // keeps them distinct from other connections of the same owner.
        let random = (js_sys::Math::random() * f64::from(u32::MAX)) as u32;
        let stamp = browser_monotonic_ms() as u32;
        bytes[8..12].copy_from_slice(&random.to_le_bytes());
        bytes[12..].copy_from_slice(&stamp.to_le_bytes());
        OperationId::new(bytes).unwrap_or_else(|| unreachable!("the counter half is non-zero"))
    }

    /// Advance a kill whose organization page arrived: terminate the
    /// target's current activation, read the next page, or, when no page
    /// names the target, fall back to `KILL_RESOURCE`, which a translating
    /// server still honors as a kill.
    pub(super) fn resolve_kill(&mut self, page: &OrganizationPage) -> Outcome {
        let Some(pending) = self.pending_kill.as_ref() else {
            return Outcome::default();
        };
        let target = pending.target.clone();
        let session = pending.resolving;
        let request_id = self.next_pane_request();
        let activation = page
            .resources
            .iter()
            .find(|record| record.live.as_ref() == Some(&target))
            .and_then(|record| record.activation);
        let (frame, resolving) = match (activation, &page.next_cursor, session) {
            (Some(activation), _, _) => {
                let operation = self.next_operation();
                let frame = encode(&FrameKind::Command {
                    request_id,
                    command: Command::ResourceTerminate {
                        operation,
                        target: LiveTarget {
                            resource: target,
                            instance: activation.instance,
                            activation: activation.id,
                        },
                        mode: TerminateMode::Graceful,
                    },
                });
                (frame, None)
            }
            (None, Some(cursor), Some(session)) => (
                Self::organization_page_frame(request_id, session, Some(cursor.clone())),
                Some(session),
            ),
            (None, _, _) => (Self::legacy_kill_frame(request_id, target), None),
        };
        if let Some(pending) = self.pending_kill.as_mut() {
            pending.request_id = request_id;
            pending.resolving = resolving;
        }
        Outcome {
            send: vec![frame],
            panes: true,
            ..Outcome::default()
        }
    }
}
