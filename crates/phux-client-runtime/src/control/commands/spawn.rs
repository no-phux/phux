//! Terminal creation with an explicit reconnect policy for interactive setup.
#[cfg(test)]
#[path = "spawn_tests.rs"]
mod tests;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::SpawnResource;

use phux_client_core::org_ops::OrgOutcome;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{ResourceKind, SessionId};
use phux_protocol::wire::frame::{Command, CreatePlacement, ExitPolicy, LaunchMode, LaunchSpec};

use super::super::org_plane::{mint_operation, outcome_sentence};
use super::super::{ControlPlane, Event, FrameKind, GroupId, Pending, SpawnRequest, Status};

const NO_SESSION_HOST: &str = "selected session has no confirmed live resource to host the new terminal, and the server cannot create one in it directly; refresh or open a terminal in that session first";

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
        let foreign = target_session.filter(|session| Some(*session) != self.attached_session);
        let owner_terminal = foreign.map(|session| (session, self.spawn_session_owner(session)));
        let owner_terminal = match owner_terminal {
            Some((session, None)) => {
                self.spawn_without_owner(request_id, session, request, allow_replay);
                return request_id;
            }
            Some((_, owner)) => owner,
            None => None,
        };
        let keyed = allow_replay && self.status == Status::Attached;
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

impl ControlPlane {
    /// A foreign session with no live member to address it through: create
    /// the terminal by the session's key, or settle the spawn as refused.
    /// A once-only spawn is never journaled, since the journal resends it.
    fn spawn_without_owner(
        &mut self,
        request_id: u32,
        session: u32,
        request: SpawnRequest,
        allow_replay: bool,
    ) {
        if allow_replay && self.create_in_session(request_id, session, request) {
            return;
        }
        self.push_event(Event::TerminalSpawned {
            request_id,
            terminal_id: None,
            error: Some(NO_SESSION_HOST.to_owned()),
        });
    }

    /// Create a terminal in `session` by its organization key
    /// (`RESOURCE_CREATE` placed in a new group), for a session with no live
    /// member a `SPAWN_RESOURCE` could name as owner. The operation journal
    /// resends it under the same id, so a reconnect never starts two. Returns
    /// false when the replica or server cannot take it.
    fn create_in_session(&mut self, request_id: u32, session: u32, request: SpawnRequest) -> bool {
        let placed = self
            .server
            .as_ref()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::CreatePlacement));
        if !placed || !self.organization_supported() || !self.organization.is_live() {
            return false;
        }
        let Some(key) = self
            .organization
            .session_by_live(SessionId::new(session))
            .map(|record| record.key)
        else {
            return false;
        };
        let mut launch = LaunchSpec::new(request.command.unwrap_or_default()).with_cwd(request.cwd);
        launch.env = request.env.unwrap_or_default();
        let command = Command::ResourceCreate {
            operation: mint_operation(),
            session: key,
            kind: ResourceKind::Terminal,
            launch: Some(Box::new(launch)),
            name: None,
            on_exit: ExitPolicy::Remove,
            launch_mode: LaunchMode::Immediate,
            placement: Some(Box::new(CreatePlacement::NewGroup)),
        };
        let Ok(operation) = self.journal_org_operation(command) else {
            return false;
        };
        self.org.spawns.push((operation, request_id));
        self.flush_org_journal();
        true
    }

    /// Settle a [`Self::create_in_session`] spawn as
    /// [`Event::TerminalSpawned`]; false for any other operation.
    pub(in crate::control) fn resolve_session_spawn(
        &mut self,
        report: &phux_client_core::org_ops::OrgReport,
    ) -> bool {
        let Some(position) = self
            .org
            .spawns
            .iter()
            .position(|(operation, _)| *operation == report.operation)
        else {
            return false;
        };
        let (_, request_id) = self.org.spawns.remove(position);
        let receipt = match &report.outcome {
            OrgOutcome::Committed(receipt) => receipt.target.as_ref(),
            _ => None,
        };
        let terminal_id = receipt.map(|target| target.resource.clone());
        let error = match &report.outcome {
            _ if terminal_id.is_some() => None,
            OrgOutcome::Committed(_) => Some("the server created no live terminal".to_owned()),
            outcome => Some(outcome_sentence(outcome)),
        };
        if let Some(id) = &terminal_id {
            // Unlike SPAWN_RESOURCE, a create does not pump output to its
            // requester: subscribe as an attach would.
            let _ = self.attach_terminal(id);
        }
        self.push_event(Event::TerminalSpawned {
            request_id,
            terminal_id: terminal_id.clone(),
            error,
        });
        if terminal_id.is_some() && self.handshake_ready && self.options.automatic_lifecycle {
            self.fence_topology_read();
            self.queue_refresh_topology();
        }
        true
    }
}
