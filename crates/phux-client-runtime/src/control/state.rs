//! Shared bookkeeping and event queue maintenance.

use super::{
    CommandResult, ControlPlane, EVENT_QUEUE_CAP, ErrorCode, Event, Pending, ServerFeature, Status,
};

impl ControlPlane {
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
        if self.status == status {
            return;
        }
        self.status = status;
        self.push_event(Event::StatusChanged(status));
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
        if self.events.len() >= EVENT_QUEUE_CAP {
            let queued = self.events.len();
            self.events.retain(Event::is_lossless);
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
        self.events.push(event);
    }
}
