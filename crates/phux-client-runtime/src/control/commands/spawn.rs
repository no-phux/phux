//! Terminal creation with an explicit reconnect policy for interactive setup.
#[cfg(test)]
#[path = "spawn_tests.rs"]
mod tests;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::SpawnResource;

use super::super::{
    ControlPlane, Event, FrameKind, GroupId, Pending, ServerFeature, SpawnRequest, Status,
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
        // A foreign session is addressed by an explicit member of any kind.
        // Never omit the owner and accidentally execute in the home session.
        let target_session = request.session_id.or(self.selected_session);
        let owner_terminal = match target_session
            .filter(|session| Some(*session) != self.attached_session)
        {
            Some(session) => {
                let Some(owner) = self.spawn_session_owner(session) else {
                    self.push_event(Event::TerminalSpawned {
                        request_id, terminal_id: None,
                        error: Some("selected session has no confirmed live resource to host the new terminal; refresh or open a terminal in that session first".to_owned()),
                    });
                    return request_id;
                };
                Some(owner)
            }
            None => None,
        };
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

    fn spawn_session_owner(&self, session: u32) -> Option<phux_protocol::ids::ResourceId> {
        let member = self
            .organization
            .is_live()
            .then(|| {
                let record = self
                    .organization
                    .session_by_live(phux_protocol::ids::SessionId::new(session))?;
                self.organization
                    .members(&record.key)
                    .find_map(|resource| resource.live.clone())
            })
            .flatten();
        member.or_else(|| self.topology.as_ref()?.first_pane_of(session).cloned())
    }
}
