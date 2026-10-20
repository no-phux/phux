//! Pane replies: spawned splits, kill answers, refusals, and the resized layout.

use super::*;

impl Connection {
    pub(super) fn restore_layout(&mut self) {
        if let Some(focused) = &self.focused_terminal
            && let Some(index) = self.terminal_order.iter().position(|id| id == focused)
            && index >= 4
        {
            self.terminal_order.swap(0, index);
        }
        self.terminal_order.truncate(4);
        let mut ids = self.terminal_order.iter().take(4);
        self.layout = ids.next().cloned().map(Layout::Leaf);
        let mut previous = self.terminal_order.first().cloned();
        for id in ids {
            if let (Some(layout), Some(target)) = (&mut self.layout, &previous) {
                layout.insert(target, id.clone(), Axis::Vertical);
            }
            previous = Some(id.clone());
        }
    }

    pub(super) fn pane_resize_frames(&mut self) -> Vec<Vec<u8>> {
        self.pane_rects()
            .into_iter()
            .filter_map(|(terminal_id, rect)| {
                let geometry = CanonicalGeometry {
                    cols: rect.cols,
                    rows: rect.rows,
                };
                let previous = self
                    .pane_sizes
                    .insert(terminal_id.clone(), geometry)
                    .or_else(|| {
                        self.kernel
                            .as_ref()
                            .and_then(|kernel| kernel.published(&terminal_id))
                            .map(|replica| replica.geometry())
                    });
                if previous == Some(geometry) {
                    return None;
                }
                Some(encode(&FrameKind::ResizeTerminal {
                    terminal_id,
                    cols: rect.cols,
                    rows: rect.rows,
                    cell_px: self.cell_px,
                }))
            })
            .collect()
    }

    /// Resize every visible resource to its local pane rectangle.
    pub fn resize_panes(&mut self, cols: u16, rows: u16) -> Vec<Vec<u8>> {
        if self.layout.is_none() {
            return self.resize_frame(cols, rows).into_iter().collect();
        }
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.failed || (cols, rows) == (self.cols, self.rows) {
            return Vec::new();
        }
        self.cols = cols;
        self.rows = rows;
        self.pane_resize_frames()
    }

    pub(super) fn pane_refusal(&mut self, message: String) -> Outcome {
        self.agent_discoveries.clear();
        let resource = self
            .pending_split
            .take()
            .and_then(|pending| pending.resource);
        self.pending_kill = None;
        self.pane_error = Some(message);
        // A spawn this connection made and never presented is cleaned up with
        // KILL_RESOURCE, which every server honors without an activation read.
        let send = resource
            .into_iter()
            .map(|terminal_id| {
                self.retiring_panes.push(terminal_id.clone());
                let request_id = self.next_pane_request();
                Self::legacy_kill_frame(request_id, terminal_id)
            })
            .collect();
        Outcome {
            send,
            panes: true,
            ..Outcome::default()
        }
    }

    pub(super) fn reduce_pane_frame(&mut self, frame: &FrameKind) -> Option<Outcome> {
        match frame {
            FrameKind::ResourceSpawned { request_id, result }
                if self
                    .pending_split
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == *request_id) =>
            {
                Some(self.accept_spawn(result))
            }
            FrameKind::CommandResult { request_id, result } => {
                self.accept_pane_command(*request_id, result)
            }
            FrameKind::Error {
                request_id,
                message,
                ..
            } => {
                let correlated = request_id.is_some_and(|id| {
                    self.pending_split
                        .as_ref()
                        .is_some_and(|pending| pending.request_id == id)
                        || self
                            .pending_kill
                            .as_ref()
                            .is_some_and(|pending| pending.request_id == id)
                });
                if correlated {
                    Some(self.pane_refusal(message.clone()))
                } else {
                    self.pane_error = Some(message.clone());
                    Some(Outcome {
                        panes: true,
                        ..Outcome::default()
                    })
                }
            }
            _ => None,
        }
    }

    fn accept_spawn(&mut self, result: &SpawnResult) -> Outcome {
        let Some(terminal_id) = result.spawned_id().cloned() else {
            return self.pane_refusal(format!(
                "Could not split this pane: {result:?}. Try again or close another pane."
            ));
        };
        let request_id = self.next_pane_request();
        if let Some(pending) = self.pending_split.as_mut() {
            pending.resource = Some(terminal_id.clone());
            pending.request_id = request_id;
        }
        Outcome {
            send: vec![encode(&FrameKind::Command {
                request_id,
                command: Command::attach_terminal(terminal_id, None),
            })],
            panes: true,
            ..Outcome::default()
        }
    }

    fn accept_pane_command(&mut self, request_id: u32, result: &CommandResult) -> Option<Outcome> {
        let split = self
            .pending_split
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id);
        let kill = self
            .pending_kill
            .as_ref()
            .filter(|pending| pending.request_id == request_id);
        if !split && kill.is_none() {
            return None;
        }
        let resolving = kill.is_some_and(|pending| pending.resolving.is_some());
        match result {
            CommandResult::Error { message, .. } => {
                return Some(self.pane_refusal(format!("Pane operation refused: {message}")));
            }
            CommandResult::Refused(refusal) => {
                return Some(
                    self.pane_refusal(format!("Pane operation refused: {}", refusal.as_str())),
                );
            }
            CommandResult::OkWith(CommandValue::OrganizationPage(page)) if resolving => {
                return Some(self.resolve_kill(page));
            }
            _ if resolving => {
                return Some(self.pane_refusal(
                    "Pane operation refused: the server did not list the session.".to_owned(),
                ));
            }
            _ => {}
        }
        if split && let Some(pending) = self.pending_split.as_mut() {
            pending.acknowledged = true;
        }
        Some(Outcome {
            panes: true,
            ..Outcome::default()
        })
    }

    pub(super) fn finish_split(&mut self, outcome: &mut Outcome) {
        let Some(pending) = self.pending_split.as_ref() else {
            return;
        };
        let Some(id) = pending.resource.as_ref() else {
            return;
        };
        if !pending.acknowledged || self.pane_terminal(id).is_none() {
            return;
        }
        let id = id.clone();
        if let Some(layout) = self.layout.as_mut() {
            if !layout.insert(&pending.target, id.clone(), pending.axis)
                && let Some(target) = self.terminal_order.first()
            {
                // The requested source may naturally exit while spawn is in flight.
                layout.insert(target, id.clone(), pending.axis);
            }
        } else {
            self.layout = Some(Layout::Leaf(id.clone()));
        }
        self.terminal_order.push(id.clone());
        self.focused_terminal = Some(id);
        self.cancel_path_query();
        self.pending_split = None;
        outcome.panes = true;
        outcome.badges = true;
        outcome.render = true;
        outcome.send.extend(self.pane_resize_frames());
        self.reconcile_agent_discoveries(outcome);
    }
}
