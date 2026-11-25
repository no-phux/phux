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

/// Local submission evidence. A queue or journal entry is not a host commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowSubmission {
    /// No command or deferred work was accepted.
    NotQueued { reason: String },
    /// One wire request was queued; its reply retains this correlation.
    Queued { request_id: u32 },
    /// The existing organization journal owns retries under this exact identity.
    Journaled {
        request_id: Option<u32>,
        operation: phux_protocol::ids::OperationId,
    },
}

impl ControlPlane {
    /// Canonical settlement plus the established legacy event projection.
    pub(in crate::control) fn publish_spawn_outcome(
        &mut self,
        request_id: u32,
        outcome: super::super::TerminalCreationOutcome,
    ) {
        use super::super::TerminalCreationOutcome;
        let (terminal_id, error) = match &outcome {
            TerminalCreationOutcome::LiveTarget { terminal_id } => (Some(terminal_id.clone()), None),
            TerminalCreationOutcome::Refused { message }
            | TerminalCreationOutcome::Unknown { message } => (None, Some(message.clone())),
        };
        self.push_event(Event::TerminalCreation { request_id, outcome });
        self.push_event(Event::TerminalSpawned { request_id, terminal_id, error });
    }
    /// Submit a spawn with truthful queue/journal evidence, without selecting or
    /// creating a session implicitly. Later receipts are separate events.
    pub fn submit_spawn(&mut self, request: SpawnRequest) -> WorkflowSubmission {
        self.submit_spawn_with_replay(request, true)
    }

    /// Submit once-only setup without allowing ambiguous reconnect replay.
    pub fn submit_spawn_once(&mut self, request: SpawnRequest) -> WorkflowSubmission {
        self.submit_spawn_with_replay(request, false)
    }

    fn submit_spawn_with_replay(&mut self, request: SpawnRequest, allow_replay: bool) -> WorkflowSubmission {
        if !self.handshake_ready || (self.attached_session.is_none()
            && request.session_id.or(self.selected_session).is_none()) {
            return WorkflowSubmission::NotQueued { reason: "session_unavailable".into() };
        }
        let request_id = self.next_request_id();
        self.spawn_terminal_with_replay(request_id, request, allow_replay)
    }

    /// Exact create: a colliding name is refused by the host, never attached.
    /// The retained operation is reconciled by the existing journal after loss.
    pub fn create_session(
        &mut self,
        name: String,
        lifetime: phux_protocol::wire::frame::SessionLifetime,
        launch: Option<Box<LaunchSpec>>,
    ) -> WorkflowSubmission {
        if phux_client_core::rename::check_session_name(&name).is_err() {
            return WorkflowSubmission::NotQueued { reason: "invalid_session_name".into() };
        }
        if !self.handshake_ready || !self.organization_supported() || !self.org.ready {
            return WorkflowSubmission::NotQueued { reason: "organization_unavailable".into() };
        }
        match self.submit_organization(Command::SessionCreate {
            operation: mint_operation(), name, lifetime, attrs: Vec::new(), launch,
        }) {
            Ok(operation) => WorkflowSubmission::Journaled { request_id: None, operation },
            Err(error) => WorkflowSubmission::NotQueued { reason: error.to_string() },
        }
    }

    /// Resolve existence at the host under one retained operation. Never use
    /// a replica existence check plus create as a substitute.
    pub fn ensure_session(
        &mut self,
        name: String,
        lifetime: phux_protocol::wire::frame::SessionLifetime,
        launch: Option<Box<LaunchSpec>>,
    ) -> WorkflowSubmission {
        if phux_client_core::rename::check_session_name(&name).is_err() {
            return WorkflowSubmission::NotQueued { reason: "invalid_session_name".into() };
        }
        if !self.handshake_ready || !self.org.ready || !self.organization_supported() {
            return WorkflowSubmission::NotQueued { reason: "organization_unavailable".into() };
        }
        if !self.server.as_ref().is_some_and(|server| server.has_ext(ServerFeatureExt::SessionEnsure)) {
            return WorkflowSubmission::NotQueued { reason: "ensure_session_unsupported".into() };
        }
        match self.submit_organization(Command::SessionEnsure {
            operation: mint_operation(), name, lifetime, launch, attrs: Vec::new(),
        }) {
            Ok(operation) => WorkflowSubmission::Journaled { request_id: None, operation },
            Err(error) => WorkflowSubmission::NotQueued { reason: error.to_string() },
        }
    }
}

const NO_SESSION_HOST: &str = "selected session has no confirmed live resource to host the new terminal, and the server cannot create one in it directly; refresh or open a terminal in that session first";

impl ControlPlane {
    /// Spawn a terminal (`SPAWN_RESOURCE`); the reply is
    /// [`crate::control::Event::TerminalSpawned`] with the returned correlation, and the
    /// server pumps the new terminal's output to this client.
    pub fn spawn_terminal(&mut self, request: SpawnRequest) -> u32 {
        self.legacy_spawn(request, true)
    }

    /// Submit one terminal spawn without retaining it for reconnect replay.
    ///
    /// Use for interactive setup such as provider login: a lost receipt is an
    /// unknown outcome, never permission to start authentication after reconnect.
    pub fn spawn_terminal_once(&mut self, request: SpawnRequest) -> u32 {
        self.legacy_spawn(request, false)
    }

    fn legacy_spawn(&mut self, request: SpawnRequest, allow_replay: bool) -> u32 {
        let request_id = self.next_request_id();
        if let WorkflowSubmission::NotQueued { reason } =
            self.spawn_terminal_with_replay(request_id, request, allow_replay)
        {
            self.publish_spawn_outcome(request_id,
                super::super::TerminalCreationOutcome::Refused { message: reason });
        }
        request_id
    }

    fn spawn_terminal_with_replay(&mut self, request_id: u32, request: SpawnRequest, allow_replay: bool) -> WorkflowSubmission {
        // A foreign session is addressed by an explicit member of any kind.
        // Never omit the owner and accidentally execute in the home session.
        let target_session = request.session_id.or(self.selected_session);
        let foreign = target_session.filter(|session| Some(*session) != self.attached_session);
        let owner_terminal = foreign.map(|session| (session, self.spawn_session_owner(session)));
        let owner_terminal = match owner_terminal {
            Some((session, None)) => {
                if !allow_replay {
                    return WorkflowSubmission::NotQueued { reason: NO_SESSION_HOST.into() };
                }
                return match self.create_in_session(request_id, session, request) {
                    Ok(operation) => WorkflowSubmission::Journaled { request_id: Some(request_id), operation },
                    Err(reason) => WorkflowSubmission::NotQueued { reason },
                };
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
        WorkflowSubmission::Queued { request_id }
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

    /// Create a terminal in `session` by its organization key
    /// (`RESOURCE_CREATE` placed in a new group), for a session with no live
    /// member a `SPAWN_RESOURCE` could name as owner. The operation journal
    /// resends it under the same id, so a reconnect never starts two.
    fn create_in_session(&mut self, request_id: u32, session: u32, request: SpawnRequest) -> Result<phux_protocol::ids::OperationId, String> {
        let placed = self
            .server
            .as_ref()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::CreatePlacement));
        if !placed || !self.organization_supported() || !self.organization.is_live() {
            return Err(NO_SESSION_HOST.into());
        }
        let Some(key) = self
            .organization
            .session_by_live(SessionId::new(session))
            .map(|record| record.key)
        else {
            return Err(NO_SESSION_HOST.into());
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
        let operation = self.journal_org_operation(command).map_err(|error| error.to_string())?;
        self.org.spawns.push((operation, request_id));
        self.flush_org_journal();
        Ok(operation)
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
