//! Validated control commands and terminal launch requests.
use super::*;

impl Connection {
    /// Create a persistent session whose first terminal starts in
    /// `directory`. The outcome is [`Event::OrganizationOperation`] for the
    /// returned id.
    ///
    /// # Errors
    ///
    /// A sentence when the journal refuses the submission.
    pub fn create_session(&self, name: &str, directory: &str) -> Result<OperationId, String> {
        self.client
            .submit_organization(session_create(name, Some(directory.to_owned())))
            .map_err(|error| error.to_string())
    }

    /// Kill `session` with one `SESSION_KILL`. The result is
    /// [`Event::OrganizationOperation`] for the returned operation; `None`
    /// when the server does not take the command.
    #[must_use]
    pub fn kill_session(&self, session: SessionKey) -> Option<OperationId> {
        self.try_kill_session(session).ok()
    }

    /// Submit a session kill while preserving the failure for the UI.
    pub(crate) fn try_kill_session(&self, session: SessionKey) -> Result<OperationId, String> {
        if self.closed || self.status != Status::Attached {
            return Err("Not connected.".to_owned());
        }
        self.client
            .kill_session(session)
            .map_err(|error| error.to_string())
    }

    /// Ask the runtime to drop the socket and redial. A terminal client cannot reopen.
    pub fn reconnect(&self) -> anyhow::Result<()> {
        if self.client.status().is_terminal() || self.closed {
            anyhow::bail!("the runtime client is closed; connect again");
        }
        self.client.resync();
        Ok(())
    }

    /// End the session. The runtime does not reopen it.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.admitted.clear();
        self.client.close();
    }

    /// Ask for a fresh session list. `None` before the handshake or when closed.
    #[must_use]
    pub fn refresh_topology(&self) -> Option<u32> {
        if self.closed || self.status != Status::Attached {
            return None;
        }
        self.client.refresh_topology()
    }

    /// Queue a spawn. The session must already be in the topology; there is no fallback.
    pub fn spawn(&self, request: SpawnRequest) -> Result<u32, CommandError> {
        self.client
            .with_control(|control| queue_spawn(control, request))
    }

    /// Explicitly terminate a local terminal. Satellite ids are refused, not retried.
    pub fn terminate(
        &self,
        identity: &ConnectionIdentity,
        terminal: &ResourceId,
    ) -> Result<u32, CommandError> {
        let admitted = self.admitted.contains(terminal);
        self.client
            .with_control(|control| queue_termination(control, identity, terminal, admitted))
    }

    /// Current mutation identity, after the handshake.
    #[must_use]
    pub fn identity(&self) -> Option<ConnectionIdentity> {
        self.identity.clone()
    }

    /// Product feature names, including `path-query` and `spawn_initial_size` when advertised.
    #[must_use]
    pub fn features(&self) -> Vec<String> {
        let Some(server) = &self.server else {
            return Vec::new();
        };
        let mut names = status::negotiated_features(server);
        if server.has(ServerFeature::SpawnInitialSize)
            && !names.iter().any(|name| name == "spawn_initial_size")
        {
            names.push("spawn_initial_size".to_owned());
        }
        names
    }

    /// Hex server incarnation.
    #[must_use]
    pub fn server_id(&self) -> Option<String> {
        self.identity
            .as_ref()
            .map(|identity| identity.server_id.clone())
    }

    /// Current connection epoch.
    #[must_use]
    pub fn connection_epoch(&self) -> u64 {
        self.client.connection_epoch()
    }

    /// Bumps on every drain.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Attach session name.
    #[must_use]
    pub fn session_name(&self) -> &str {
        &self.session
    }

    /// Host path query. Zero means nothing was sent.
    #[must_use]
    pub fn path_query(
        &self,
        root: String,
        query: String,
        recursive: bool,
        host: Option<phux_protocol::ids::SatelliteHost>,
    ) -> u32 {
        self.client.path_query(root, query, recursive, host)
    }
}

/// `SESSION_CREATE` for a persistent session whose first terminal starts in `cwd`.
pub(crate) fn session_create(name: &str, cwd: Option<String>) -> Command {
    let mut launch = LaunchSpec::new(Vec::new());
    launch.cwd = cwd;
    Command::SessionCreate {
        operation: mint_operation(),
        name: name.to_owned(),
        lifetime: SessionLifetime::Persistent,
        attrs: Vec::new(),
        launch: Some(Box::new(launch)),
    }
}

fn queue_spawn(control: &mut ControlPlane, mut request: SpawnRequest) -> Result<u32, CommandError> {
    let not_connected = |reason| {
        command(
            reason,
            "Not connected",
            "Wait for the server, or reconnect with ⌘R.",
        )
    };
    if !control.handshake_ready() {
        return Err(not_connected("NotNegotiated"));
    }
    if control.status() != Status::Attached {
        return Err(not_connected("NotAttached"));
    }
    if control
        .options()
        .attach_role
        .is_some_and(RolePolicy::is_viewer)
    {
        return Err(command(
            "ObserveOnly",
            "Not connected",
            "This window is observing only.",
        ));
    }
    validate_spawn(&request)?;
    let target = request
        .session_id
        .or(control.attached_session())
        .ok_or_else(|| not_connected("InvalidSessionId"))?;
    let Some(topology) = control.topology() else {
        return Err(not_connected("TopologyUnavailable"));
    };
    if !topology.sessions.iter().any(|session| session.id == target) {
        return Err(command(
            "SessionNotFound",
            "Not connected",
            "That session is not on this server.",
        ));
    }
    if request.initial_size.is_some()
        && !control
            .server()
            .is_some_and(|server| server.has(ServerFeature::SpawnInitialSize))
    {
        return Err(cannot_spawn(
            "UnsupportedSpawnInitialSize",
            "This server does not accept an initial size.",
        ));
    }
    request.session_id = Some(target);
    Ok(control.spawn_terminal(request))
}

/// `admitted`: this connection's spawn/attach named the terminal, so it is
/// live even before a topology read lists it.
fn queue_termination(
    control: &mut ControlPlane,
    identity: &ConnectionIdentity,
    id: &ResourceId,
    admitted: bool,
) -> Result<u32, CommandError> {
    let refused = |reason, body| command(reason, "Terminate refused", body);
    let server_id = control
        .server()
        .map(status::server_id)
        .filter(|_| control.handshake_ready());
    let Some(server_id) = server_id else {
        return Err(refused("NotNegotiated", "Not connected."));
    };
    if identity.connection_epoch != control.connection_epoch() || identity.server_id != server_id {
        return Err(refused(
            "StaleConnectionIdentity",
            "The connection changed; try again.",
        ));
    }
    if control.status() != Status::Attached {
        return Err(refused("NotAttached", "Not connected."));
    }
    if matches!(id, ResourceId::Satellite { .. }) {
        return Err(refused(
            "UnsupportedSatelliteTermination",
            "Satellite terminals cannot be terminated from this window.",
        ));
    }
    if !admitted
        && control
            .topology()
            .and_then(|topology| topology.pane(id))
            .is_none()
    {
        return Err(refused("TerminalNotFound", "That terminal is gone."));
    }
    Ok(control.kill_terminal(id))
}

fn validate_spawn(request: &SpawnRequest) -> Result<(), CommandError> {
    let mut total = 0_usize;
    if let Some(argv) = &request.command {
        if argv.is_empty() || argv.len() > MAX_ARGS || argv[0].is_empty() {
            return Err(cannot_spawn("InvalidSpawnCommand", "Invalid command."));
        }
        for arg in argv {
            count_text(&mut total, arg)?;
        }
    }
    if let Some(cwd) = &request.cwd {
        if cwd.is_empty() {
            return Err(cannot_spawn(
                "InvalidSpawnDirectory",
                "A new session needs the focused directory or a chosen folder.",
            ));
        }
        count_text(&mut total, cwd)?;
    }
    if let Some(env) = &request.env {
        let invalid = || cannot_spawn("InvalidSpawnEnvironment", "Invalid environment.");
        if env.len() > MAX_ARGS {
            return Err(invalid());
        }
        let mut names = HashSet::new();
        for (name, value) in env {
            if name.is_empty() || name.contains('=') || !names.insert(name) {
                return Err(invalid());
            }
            count_text(&mut total, name)?;
            count_text(&mut total, value)?;
        }
    }
    Ok(())
}

fn count_text(total: &mut usize, text: &str) -> Result<(), CommandError> {
    if text.contains('\0') || text.len() > MAX_TEXT.saturating_sub(*total) {
        return Err(cannot_spawn(
            "InvalidSpawnText",
            "The request is too large.",
        ));
    }
    *total += text.len();
    Ok(())
}

fn cannot_spawn(reason: &'static str, body: &str) -> CommandError {
    command(reason, "Could not start a terminal", body)
}

fn command(reason: &'static str, title: &'static str, body: &str) -> CommandError {
    CommandError {
        reason,
        title,
        body: body.to_owned(),
    }
}
