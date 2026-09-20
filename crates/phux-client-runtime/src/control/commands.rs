//! Consumer commands and outbound frame construction.

use super::{
    AttachTarget, Command, ControlPlane, Event, FrameKind, GroupId, Pending, ResourceId,
    ServerFeature, SpawnRequest, Status, StreamRecovery, ViewportInfo, encode,
};

impl ControlPlane {
    // ----- frames out and events out --------------------------------------

    /// Every encoded frame queued since the last take, in send order.
    pub fn take_outbound(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outbound)
    }

    /// Every event queued since the last drain, in order, preceded by one
    /// [`Event::TerminalChanged`] per terminal that changed.
    pub fn take_events(&mut self) -> Vec<Event> {
        let mut events: Vec<Event> = self
            .damaged
            .drain(..)
            .map(|terminal_id| Event::TerminalChanged { terminal_id })
            .collect();
        events.append(&mut self.events);
        events
    }

    // ----- the common surface --------------------------------------------

    /// Retarget the session every connection attaches. Returns whether a
    /// reconnect is needed to honor it (a live connection is already
    /// attached elsewhere); the driver then resyncs.
    pub fn attach_session(&mut self, target: AttachTarget) -> bool {
        self.attach_target = Some(target);
        self.handshake_ready
    }

    /// The client's viewport changed: the attached session's terminals and
    /// every per-terminal subscription are resized.
    pub fn resize_viewport(&mut self, cols: u16, rows: u16) {
        let viewport = (cols.max(1), rows.max(1));
        if self.options.viewport == viewport {
            return;
        }
        self.options.viewport = viewport;
        if !self.handshake_ready {
            return;
        }
        self.queue_frame(&FrameKind::ViewportResize {
            viewport: ViewportInfo::new(viewport.0, viewport.1),
        });
        // VIEWPORT_RESIZE reaches only the ATTACHed session's terminals; a
        // per-terminal subscription keeps its geometry without the verb.
        let foreign: Vec<ResourceId> = self.terminal_attached.iter().cloned().collect();
        for terminal_id in foreign {
            self.queue_frame(&FrameKind::ResizeTerminal {
                terminal_id,
                cols: viewport.0,
                rows: viewport.1,
            });
        }
    }

    /// Subscribe to one terminal's stream on the live socket
    /// (`Command::AttachResource`); the reply is [`Event::TerminalAttached`]
    /// with the returned correlation.
    pub fn attach_terminal(&mut self, terminal_id: &ResourceId) -> u32 {
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::AttachTerminal(terminal_id.clone()));
        self.terminal_attached.insert(terminal_id.clone());
        let (cols, rows) = self.options.viewport;
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::AttachResource {
                terminal_id: terminal_id.clone(),
                role_policy: self.options.attach_role,
            },
        });
        // ATTACH_RESOURCE does not resize; reflow the terminal to this
        // viewport as a session ATTACH would have.
        self.queue_frame(&FrameKind::ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols,
            rows,
        });
        request_id
    }

    /// Drop a per-terminal subscription (`Command::DetachResource`); the
    /// reply is [`Event::TerminalDetached`]. Never for a terminal of the
    /// attached session: its stream rides the session pumps.
    pub fn detach_terminal(&mut self, terminal_id: &ResourceId) -> u32 {
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::DetachTerminal(terminal_id.clone()));
        self.terminal_attached.remove(terminal_id);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::DetachResource {
                terminal_id: terminal_id.clone(),
            },
        });
        request_id
    }

    /// Make one terminal's stream live again, preferring a per-terminal
    /// re-attach over a reconnect.
    pub fn ensure_stream(&mut self, terminal_id: &ResourceId) -> StreamRecovery {
        if self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.is_closed(terminal_id))
            || !self.stream_recoveries.insert(terminal_id.clone())
        {
            return StreamRecovery::Noop;
        }
        let pane_session = self
            .topology
            .as_ref()
            .and_then(|topology| topology.pane(terminal_id))
            .map(|pane| pane.session_id);
        let foreign_live = self.status == Status::Attached
            && pane_session.is_some()
            && pane_session != self.attached_session;
        if foreign_live {
            self.attach_terminal(terminal_id);
            StreamRecovery::Attached
        } else {
            StreamRecovery::Reconnect
        }
    }

    /// Spawn a terminal (`SPAWN_RESOURCE`); the reply is
    /// [`Event::TerminalSpawned`] with the returned correlation, and the
    /// server pumps the new terminal's output to this client.
    pub fn spawn_terminal(&mut self, request: SpawnRequest) -> u32 {
        let request_id = self.next_request_id();
        // A spawn into the attached (home) session needs no owner; any
        // other session is addressed through one of its existing panes.
        let owner_terminal = request
            .session_id
            .filter(|session| Some(*session) != self.attached_session)
            .and_then(|session| {
                self.topology
                    .as_ref()
                    .and_then(|topology| topology.first_pane_of(session).cloned())
            });
        self.queue_frame(&FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: request.command,
            cwd: request.cwd,
            env: request.env,
            term: None,
            satellite: None,
            owner_terminal,
            agent_session: None,
            resource: None,
            initial_size: Some(request.initial_size.unwrap_or(self.options.viewport)),
        });
        request_id
    }

    /// Terminate a terminal's process (`Command::KillResource`); the reply
    /// is [`Event::TerminalKilled`], and the close itself arrives as
    /// [`Event::TerminalClosed`].
    pub fn kill_terminal(&mut self, terminal_id: &ResourceId) -> u32 {
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::Kill(terminal_id.clone()));
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::KillResource {
                terminal_id: terminal_id.clone(),
                operation_id: None,
            },
        });
        request_id
    }

    /// Close a batch of terminals atomically (`CLOSE_TAB_RESOURCES`);
    /// `None` when the server did not advertise it. The reply is
    /// [`Event::TerminalsClosed`].
    pub fn close_terminals(&mut self, ids: Vec<ResourceId>) -> Option<u32> {
        if !self.server_has(ServerFeature::CloseTabResources) {
            return None;
        }
        let request_id = self.next_request_id();
        self.pending.insert(request_id, Pending::Close);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::CloseTabResources { ids },
        });
        Some(request_id)
    }

    /// Re-read the session graph on the live socket (`GET_STATE`); the
    /// result is [`Event::TopologyChanged`]. `None` before `HELLO_OK`.
    pub fn refresh_topology(&mut self) -> Option<u32> {
        if !self.handshake_ready {
            return None;
        }
        Some(self.queue_refresh_topology())
    }

    /// Send one key on the raw path. `false` when the terminal is fenced
    /// behind an acknowledged input with unknown delivery.
    /// Subscribe to the server-wide event stream from `after_seq`
    /// (ADR-0123); the cursor is honored only on a server that advertises
    /// `EVENT_JOURNAL`.
    pub fn subscribe_events(&mut self, after_seq: Option<u64>) {
        self.options.event_after_seq = after_seq;
        if self.handshake_ready {
            let after_seq = after_seq.filter(|_| self.server_has(ServerFeature::EventJournal));
            self.queue_frame(&FrameKind::SubscribeEvents {
                terminal: None,
                after_seq,
            });
        }
    }

    /// Ask the server to end the attach; the session closes when
    /// `DETACHED { Requested }` arrives.
    pub fn detach(&mut self) {
        self.detach_requested = true;
        self.queue_frame(&FrameKind::Detach);
    }

    /// Extension point: send any `COMMAND` and receive its reply as
    /// [`Event::CommandResult`].
    pub fn send_command(&mut self, command: Command) -> u32 {
        let request_id = self.next_request_id();
        self.pending.insert(request_id, Pending::Extension);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command,
        });
        request_id
    }

    /// Extension point: the next correlation id, for a binding that builds
    /// a frame of its own and wants its reply through [`Event::Frame`].
    pub fn next_request_id(&mut self) -> u32 {
        let id = self.request_seq;
        self.request_seq = self.request_seq.wrapping_add(1).max(1);
        id
    }

    /// Extension point: queue any frame, encoded.
    pub fn queue_frame(&mut self, frame: &FrameKind) {
        self.outbound.push(encode(frame));
    }
}
