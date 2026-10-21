//! Terminal creation with an explicit reconnect policy for interactive setup.
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::SpawnResource;

use super::super::{
    ControlPlane, FrameKind, GroupId, Pending, ServerFeature, SpawnRequest, Status,
};

impl ControlPlane {
    /// Spawn a terminal (`SPAWN_RESOURCE`); the reply is
    /// [`crate::control::Event::TerminalSpawned`] with the returned correlation, and the
    /// server pumps the new terminal's output to this client.
    pub fn spawn_terminal(&mut self, request: SpawnRequest) -> u32 {
        self.spawn_terminal_with_replay(request, true)
    }

    /// Submit one terminal spawn without retaining it for reconnect replay.
    ///
    /// Use for interactive setup such as provider login: a lost receipt is an
    /// unknown outcome, never permission to start authentication after reconnect.
    pub fn spawn_terminal_once(&mut self, request: SpawnRequest) -> u32 {
        self.spawn_terminal_with_replay(request, false)
    }

    fn spawn_terminal_with_replay(&mut self, request: SpawnRequest, allow_replay: bool) -> u32 {
        let request_id = self.next_request_id();
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
        let keyed = allow_replay
            && self.status == Status::Attached
            && self.server_has(ServerFeature::SpawnIdempotency);
        // UUID v4 sets the version bits, so its bytes cannot be all zero.
        let resource = keyed.then(|| {
            Box::new(
                SpawnResource::default()
                    .with_idempotency_key(IdempotencyKey::new(uuid::Uuid::new_v4().into_bytes())),
            )
        });
        let frame = FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: request.command,
            cwd: request.cwd,
            env: request.env,
            term: None,
            satellite: None,
            owner_terminal,
            agent_session: None,
            resource,
            // Without a geometry vote the server's default size applies.
            initial_size: request
                .initial_size
                .or_else(|| self.votes_on_geometry().then_some(self.viewport())),
        };
        self.pending.insert(
            request_id,
            Pending::Spawn {
                replay: keyed.then(|| frame.clone()),
                needs_replay: false,
            },
        );
        self.queue_frame(&frame);
        request_id
    }
}
