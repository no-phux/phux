//! Consumer commands and outbound frame construction.

use std::collections::HashSet;

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

    /// Queue an embedder-correlated `ATTACH` on a manually driven plane.
    ///
    /// This is the stable-C-ABI seam: the embedder chooses the attach id and
    /// pixel geometry, while the runtime remains authoritative for lifecycle,
    /// kernel ownership, and subsequent frame handling.
    pub fn attach_explicit(
        &mut self,
        attach_id: u32,
        target: AttachTarget,
        viewport: ViewportInfo,
        request_scrollback: bool,
        scrollback_limit_lines: u32,
        role_policy: Option<phux_protocol::wire::frame::RolePolicy>,
    ) -> bool {
        if self.options.automatic_lifecycle
            || !self.handshake_ready
            || self.status == Status::Attached
            || self.active_attach_id.is_some()
            || attach_id == 0
        {
            return false;
        }
        self.options.viewport = (viewport.cols, viewport.rows);
        self.options.scrollback_lines = scrollback_limit_lines;
        self.options.attach_role = role_policy;
        self.attach_target = Some(target.clone());
        self.active_attach_id = Some(attach_id);
        self.queue_frame(&FrameKind::Attach {
            attach_id,
            target,
            viewport,
            request_scrollback,
            scrollback_limit_lines,
            role_policy,
        });
        true
    }

    /// Select a session, switching a healthy connection with per-terminal
    /// attach/detach commands instead of redialing it.
    ///
    /// Returns whether the driver must resync because the target cannot be
    /// resolved from the live topology (notably a new `CreateIfMissing`).
    /// The connection-level attach remains the home session and its pumps
    /// stay open; only foreign-session panes need explicit subscriptions.
    pub fn attach_session(&mut self, target: AttachTarget) -> bool {
        if self.status != Status::Attached {
            self.attach_target = Some(target);
            return self.handshake_ready;
        }
        let Some(session_id) = self.session_id_for_target(&target) else {
            if matches!(target, AttachTarget::CreateIfMissing { .. }) {
                self.attach_target = Some(target);
                return true;
            }
            return false;
        };
        self.attach_target = Some(target);
        if self.selected_session == Some(session_id) {
            return false;
        }
        self.switch_live_session(session_id);
        false
    }

    fn session_id_for_target(&self, target: &AttachTarget) -> Option<u32> {
        let topology = self.topology.as_ref()?;
        match target {
            AttachTarget::ByName(name) | AttachTarget::CreateIfMissing { name, .. } => {
                topology.session_named(name).map(|session| session.id)
            }
            AttachTarget::ById(id) => topology
                .sessions
                .iter()
                .any(|session| session.id == id.get())
                .then_some(id.get()),
            _ => None,
        }
    }

    fn switch_live_session(&mut self, session_id: u32) {
        let target = self.session_terminals(session_id);
        let home = self
            .attached_session
            .map(|id| self.session_terminals(id))
            .unwrap_or_default();
        let leftovers = self.foreign_subscriptions_outside(&target, &home);
        let additions = self.new_foreign_subscriptions(session_id, target);
        self.selected_session = Some(session_id);
        for id in leftovers {
            self.detach_terminal(&id);
        }
        for id in additions {
            self.attach_terminal(&id);
        }
    }

    fn session_terminals(&self, session_id: u32) -> HashSet<ResourceId> {
        self.topology
            .iter()
            .flat_map(|topology| &topology.panes)
            .filter(|pane| pane.session_id == session_id)
            .map(|pane| pane.terminal_id.clone())
            .collect()
    }

    fn foreign_subscriptions_outside(
        &self,
        target: &HashSet<ResourceId>,
        home: &HashSet<ResourceId>,
    ) -> Vec<ResourceId> {
        self.terminal_attached
            .iter()
            .filter(|id| !target.contains(*id) && !home.contains(*id))
            .cloned()
            .collect()
    }

    fn new_foreign_subscriptions(
        &self,
        session_id: u32,
        target: HashSet<ResourceId>,
    ) -> Vec<ResourceId> {
        if self.attached_session == Some(session_id) {
            return Vec::new();
        }
        target
            .into_iter()
            .filter(|id| !self.own_spawns.contains(id))
            .collect()
    }

    /// The client's viewport changed: the attached session's terminals and
    /// every default-policy per-terminal subscription are resized. Explicit
    /// preserving subscriptions and viewers skip per-terminal resize fanout.
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
        let foreign: Vec<ResourceId> = self
            .terminal_attached
            .iter()
            .filter(|id| self.follows_global_geometry(id))
            .cloned()
            .collect();
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
        if self.attach_terminals.contains(terminal_id)
            || self.own_spawns.contains(terminal_id)
            || self.terminal_attached.contains(terminal_id)
            || self.attach_request_for_terminal(terminal_id).is_some()
        {
            return 0;
        }
        self.queue_terminal_attach(terminal_id)
    }

    fn queue_terminal_attach(&mut self, terminal_id: &ResourceId) -> u32 {
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
        if self.follows_global_geometry(terminal_id) {
            self.queue_frame(&FrameKind::ResizeTerminal {
                terminal_id: terminal_id.clone(),
                cols,
                rows,
            });
        }
        request_id
    }

    /// Drop a per-terminal subscription (`Command::DetachResource`); the
    /// reply is [`Event::TerminalDetached`]. Never for a terminal of the
    /// attached session: its stream rides the session pumps.
    pub fn detach_terminal(&mut self, terminal_id: &ResourceId) -> u32 {
        self.preserve_terminal_geometry.remove(terminal_id);
        self.geometry_bootstrapped.remove(terminal_id);
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
            || self.attach_request_for_terminal(terminal_id).is_some()
        {
            return StreamRecovery::Noop;
        }
        if !self.stream_recoveries.insert(terminal_id.clone()) {
            return StreamRecovery::Noop;
        }
        if self.attach_terminals.contains(terminal_id) || self.own_spawns.contains(terminal_id) {
            // Their streams ride the original session/spawn pump. Adding a
            // per-terminal attach would double-pump output on this socket.
            return StreamRecovery::Reconnect;
        }
        if self.can_attach_foreign_stream(terminal_id) {
            // Recovery deliberately replaces an admitted foreign stream;
            // ordinary attach calls remain idempotent.
            self.queue_terminal_attach(terminal_id);
            return StreamRecovery::Attached;
        }
        StreamRecovery::Reconnect
    }

    pub(super) fn attach_request_for_terminal(&self, terminal_id: &ResourceId) -> Option<u32> {
        self.pending.iter().find_map(|(request_id, pending)| {
            matches!(pending, Pending::AttachTerminal(id) if id == terminal_id)
                .then_some(*request_id)
        })
    }

    fn can_attach_foreign_stream(&self, terminal_id: &ResourceId) -> bool {
        if self.status != Status::Attached {
            return false;
        }
        self.topology
            .as_ref()
            .and_then(|topology| topology.pane(terminal_id))
            .is_some_and(|pane| Some(pane.session_id) != self.attached_session)
    }

    /// Spawn a terminal (`SPAWN_RESOURCE`); the reply is
    /// [`Event::TerminalSpawned`] with the returned correlation, and the
    /// server pumps the new terminal's output to this client.
    pub fn spawn_terminal(&mut self, request: SpawnRequest) -> u32 {
        let request_id = self.next_request_id();
        self.pending.insert(request_id, Pending::Spawn);
        // A spawn into the attached (home) session needs no owner; any
        // other session is addressed through one of its existing panes.
        let target_session = request.session_id.or(self.selected_session);
        let owner_terminal = target_session
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
