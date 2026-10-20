//! Shared bookkeeping and event queue maintenance.

use super::{
    CLIPBOARD_QUEUE_BYTES, CommandResult, ControlPlane, EVENT_QUEUE_CAP, ErrorCode, Event, Pending,
    ServerFeature, SessionRecovery, Status,
};

impl ControlPlane {
    /// Choose who validates the home session before the next reconnect attaches.
    /// This does not replace the current socket or change its selected session.
    pub const fn set_session_recovery(&mut self, recovery: SessionRecovery) {
        self.options.session_recovery = recovery;
    }

    /// Begin following native conversations after an inventory-only connection
    /// becomes a working client. Repeated calls do not repeat the catalog read.
    pub fn watch_agent_sessions(&mut self) {
        if self.options.subscribe_agent_sessions {
            return;
        }
        self.options.subscribe_agent_sessions = true;
        if self.handshake_ready {
            self.queue_refresh_topology();
        }
    }

    pub(super) fn fail_pending_spawns(&mut self) {
        let spawns: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(id, pending)| matches!(pending, Pending::Spawn { .. }).then_some(*id))
            .collect();
        for request_id in spawns {
            self.pending.remove(&request_id);
            self.push_event(Event::TerminalSpawned {
                request_id,
                terminal_id: None,
                error: Some("server restarted before the spawn was confirmed; check its panes before retrying".to_owned()),
            });
        }
    }

    /// Resolve every outstanding correlation as failed: the socket that
    /// carried it is gone. Topology refreshes drop silently, as a fresh
    /// `ATTACHED` is on its way.
    pub(super) fn fail_pending(&mut self, message: &str, retry_spawns: bool) {
        let pending: Vec<(u32, Pending)> = self.pending.drain().collect();
        for (request_id, pending) in pending {
            match pending {
                Pending::Spawn {
                    replay: Some(frame),
                    ..
                } if retry_spawns => {
                    self.pending.insert(
                        request_id,
                        Pending::Spawn {
                            replay: Some(frame),
                            needs_replay: true,
                        },
                    );
                }
                Pending::Spawn { .. } => {
                    self.push_event(Event::TerminalSpawned {
                        request_id,
                        terminal_id: None,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::AttachTerminal(terminal_id) => {
                    self.push_event(Event::TerminalAttached {
                        request_id,
                        terminal_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::DetachTerminal { terminal_id, .. } => {
                    self.push_event(Event::TerminalDetached {
                        request_id,
                        terminal_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::Kill(terminal_id) => {
                    self.push_event(Event::TerminalKilled {
                        request_id,
                        terminal_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::Close => {
                    self.push_event(Event::TerminalsClosed {
                        request_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::NativeAgent {
                    agent_session_id,
                    operation_id,
                } => {
                    // The daemon may have dispatched before the reply was lost.
                    // Never turn a socket failure into a second model prompt.
                    self.push_event(Event::NativeAgentResult {
                        agent_session_id,
                        operation_id,
                        disposition: phux_protocol::native_agent::NativeDisposition::Unknown,
                        detail: Some(message.to_owned()),
                    });
                }
                // Refreshes and agent subscriptions need no receipt: the next
                // attach reads and subscribes again. Durable uploads retain
                // their id and acknowledged offset for the next connection;
                // transcribe correlations were resolved by
                // `reset_extension_correlations`.
                Pending::RefreshTopology
                | Pending::OrgPage(_)
                | Pending::OrgArrangement(_)
                | Pending::PutFile(_)
                | Pending::Transcribe(_)
                | Pending::Checkout(_)
                | Pending::AgentSubscription(_)
                | Pending::AgentRelease => {}
                Pending::Extension => {
                    self.push_event(Event::CommandResult {
                        request_id,
                        result: CommandResult::Error {
                            code: ErrorCode::InvalidCommand,
                            message: message.to_owned(),
                        },
                    });
                }
            }
        }
    }

    /// Reissue only keyed spawns. A lost reply says nothing about whether the
    /// server ran the operation; its dedupe record makes this repeat safe.
    pub(super) fn replay_pending_spawns(&mut self) {
        let mut requests: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(id, pending)| {
                if let Pending::Spawn {
                    replay: Some(frame),
                    needs_replay: true,
                } = pending
                {
                    Some((*id, frame.clone()))
                } else {
                    None
                }
            })
            .collect();
        requests.sort_unstable_by_key(|(id, _)| *id);
        for (request_id, frame) in requests {
            if self.server_has(ServerFeature::SpawnIdempotency) {
                self.queue_frame(&frame);
                if let Some(Pending::Spawn { needs_replay, .. }) = self.pending.get_mut(&request_id)
                {
                    *needs_replay = false;
                }
            } else {
                self.pending.remove(&request_id);
                self.push_event(Event::TerminalSpawned {
                    request_id,
                    terminal_id: None,
                    error: Some(
                        "the new server cannot safely retry this spawn; check its panes".to_owned(),
                    ),
                });
            }
        }
    }

    // ----- small helpers -----------------------------------------------

    pub(super) fn server_has(&self, feature: ServerFeature) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has(feature))
    }

    pub(super) fn set_status(&mut self, status: Status) {
        if status.is_terminal() {
            self.clipboard_focus = None;
        }
        if status != Status::Attached {
            // Clipboard requests expire with the connection, unlike durable
            // replies. Never deliver an undrained write after reconnect/close.
            self.events
                .retain(|event| !matches!(event, Event::ClipboardWrite { .. }));
            self.clipboard_event_bytes = 0;
        }
        if self.status == status {
            return;
        }
        self.status = status;
        self.push_event(Event::StatusChanged(status));
    }

    /// Test seam: the pump's quiet probe waits until the session is attached,
    /// and opening a connection always passes through `Connecting` first.
    #[cfg(test)]
    pub(crate) fn set_status_for_test(&mut self, status: Status) {
        self.set_status(status);
    }

    pub(super) fn now_ms(&self) -> u64 {
        u64::try_from(self.clock_origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(super) fn next_delivery_id(&mut self) -> u64 {
        let id = self.input_delivery_seq.max(1);
        self.input_delivery_seq = id.wrapping_add(1).max(1);
        id
    }

    pub(super) fn push_event(&mut self, event: Event) {
        let clipboard_bytes = match &event {
            Event::ClipboardWrite { text, .. } => text.0.capacity(),
            _ => 0,
        };
        // The engine admits at most 1 MiB, but typed embedders can supply
        // their own effects. Never retain an allocation over the queue cap.
        if clipboard_bytes > CLIPBOARD_QUEUE_BYTES {
            self.events_dropped = self.events_dropped.saturating_add(1);
            return;
        }
        if self.events.len() >= EVENT_QUEUE_CAP
            || self.clipboard_event_bytes + clipboard_bytes > CLIPBOARD_QUEUE_BYTES
        {
            let queued = self.events.len();
            self.events.retain(Event::is_lossless);
            self.clipboard_event_bytes = 0;
            // Every droppable event before the newest connection boundary is
            // gone, and what remains is correlated by id, not by position, so
            // older boundaries separate nothing and would grow without bound
            // across a long reconnect loop nobody drains.
            let newest = self
                .events
                .iter()
                .rposition(|event| matches!(event, Event::ConnectionOpened { .. }));
            let mut index = 0;
            self.events.retain(|event| {
                let keep =
                    !matches!(event, Event::ConnectionOpened { .. }) || Some(index) == newest;
                index += 1;
                keep
            });
            self.events_dropped = self
                .events_dropped
                .saturating_add(queued - self.events.len());
            self.events.push(Event::TopologyChanged);
            if self.handshake_ready {
                self.recover_roster();
            }
        }
        self.clipboard_event_bytes += clipboard_bytes;
        self.events.push(event);
    }
}

#[cfg(test)]
mod tests {
    use phux_client_core::engine::ClipboardText;
    use phux_protocol::ResourceId;

    use super::*;
    use crate::control::ControlOptions;

    #[test]
    fn undrained_clipboard_payloads_are_byte_bounded_and_drain_resets_the_budget() {
        let mut plane = ControlPlane::new(ControlOptions::default());
        for _ in 0..6 {
            plane.push_event(Event::ClipboardWrite {
                terminal_id: ResourceId::local(7),
                text: ClipboardText("x".repeat(1024 * 1024)),
            });
        }
        let flooded = plane.take_observation();
        assert!(
            flooded.events_dropped,
            "byte cap trips before the event-count cap"
        );
        let payload_bytes: usize = flooded
            .events
            .iter()
            .filter_map(|event| match event {
                Event::ClipboardWrite { text, .. } => Some(text.0.capacity()),
                _ => None,
            })
            .sum();
        assert!(payload_bytes > 0 && payload_bytes <= CLIPBOARD_QUEUE_BYTES);
        plane.push_event(Event::ClipboardWrite {
            terminal_id: ResourceId::local(7),
            text: ClipboardText("next".into()),
        });
        let after_drain = plane.take_observation();
        assert!(!after_drain.events_dropped);
        assert_eq!(after_drain.events.len(), 1);
    }

    #[test]
    fn clipboard_budget_counts_allocation_capacity_not_just_text_length() {
        let mut plane = ControlPlane::new(ControlOptions::default());
        let mut text = String::with_capacity(CLIPBOARD_QUEUE_BYTES + 1);
        text.push('x');
        plane.push_event(Event::ClipboardWrite {
            terminal_id: ResourceId::local(7),
            text: ClipboardText(text),
        });
        let dropped = plane.take_observation();
        assert!(dropped.events_dropped);
        assert!(dropped.events.is_empty());
    }
}
