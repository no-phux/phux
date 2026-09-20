//! Shared bookkeeping and event queue maintenance.

use super::{
    CommandResult, ControlPlane, EVENT_QUEUE_CAP, ErrorCode, Event, Pending, ServerFeature, Status,
};

impl ControlPlane {
    /// Resolve every outstanding correlation as failed: the socket that
    /// carried it is gone. Topology refreshes drop silently, as a fresh
    /// `ATTACHED` is on its way.
    pub(super) fn fail_pending(&mut self, message: &str) {
        let pending: Vec<(u32, Pending)> = self.pending.drain().collect();
        for (request_id, pending) in pending {
            match pending {
                Pending::AttachTerminal(terminal_id) => {
                    self.push_event(Event::TerminalAttached {
                        request_id,
                        terminal_id,
                        error: Some(message.to_owned()),
                    });
                }
                Pending::DetachTerminal(terminal_id) => {
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
                Pending::RefreshTopology => {}
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
            self.events.retain(Event::is_lossless);
            self.events.push(Event::TopologyChanged);
        }
        self.events.push(event);
    }
}
