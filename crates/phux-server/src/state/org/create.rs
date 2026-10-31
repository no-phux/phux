//! Creating resources: a new Terminal or agent session record.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "shares the parent's namespace"
)]
use super::*;

impl ServerState {
    /// Create a Terminal record, and with `session: None` a new session
    /// named `new_session` around it, in one commit. Returns the session and
    /// the Terminal handles; the activation is `Starting` until
    /// [`Self::activation_started`].
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_terminal(
        &mut self,
        target: TerminalTarget<'_>,
        launch: TerminalLaunch,
        on_exit: ExitPolicy,
        actor: &Actor,
    ) -> Result<(SessionId, ResourceId), Refusal> {
        let resource = NewResource {
            kind: ResourceKind::Terminal,
            name: None,
            launch: Some(LaunchSpec::Terminal(launch)),
            on_exit,
            parent: None,
            launch_mode: phux_core::LaunchMode::Immediate,
        };
        let op = match target {
            TerminalTarget::Session(session) => Op::ResourceCreate {
                placement: None,
                session: Some(
                    self.sessions
                        .registry
                        .session_key(session)
                        .ok_or(Refusal::NotFound)?,
                ),
                resource,
            },
            TerminalTarget::NewSession { name, lifetime } => Op::SessionCreate {
                name: name.to_owned(),
                lifetime,
                attrs: phux_core::Attrs::default(),
                resource: Some(resource),
            },
        };
        let commit = self.commit(op, actor)?;
        let created = match commit.applied.reply {
            phux_core::Reply::Ok { created, .. } | phux_core::Reply::Accepted { created } => {
                created
            }
        };
        let resource = created
            .resource
            .and_then(|k| self.sessions.registry.resource_id(&k))
            .ok_or(Refusal::NotFound)?;
        let session = self
            .sessions
            .registry
            .session_of(resource)
            .ok_or(Refusal::NotFound)?;
        Ok((session, resource))
    }

    /// Create an empty session record.
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_empty_session(
        &mut self,
        name: &str,
        lifetime: SessionLifetime,
        actor: &Actor,
    ) -> Result<SessionId, Refusal> {
        let commit = self.commit(
            Op::SessionCreate {
                name: name.to_owned(),
                lifetime,
                attrs: phux_core::Attrs::default(),
                resource: None,
            },
            actor,
        )?;
        let phux_core::Reply::Ok { created, .. } = commit.applied.reply else {
            return Err(Refusal::NotFound);
        };
        created
            .session
            .and_then(|k| self.sessions.registry.session_id(&k))
            .ok_or(Refusal::NotFound)
    }

    /// Create an agent conversation bound to Terminal `parent`, in the
    /// parent's session, and record it running: its engine starts in the
    /// same lock.
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_agent_session(
        &mut self,
        parent: ResourceId,
        facet: &phux_core::AgentFacet,
        actor: &Actor,
    ) -> Result<ResourceId, Refusal> {
        let parent_key = self
            .sessions
            .registry
            .resource_key(parent)
            .ok_or(Refusal::NotFound)?;
        let session = self
            .sessions
            .registry
            .record(parent)
            .map(|r| r.session)
            .ok_or(Refusal::NotFound)?;
        let commit = self.commit(
            Op::ResourceCreate {
                placement: None,
                session: Some(session),
                resource: NewResource {
                    kind: ResourceKind::AgentSession,
                    name: None,
                    launch: Some(LaunchSpec::Agent(AgentLaunch {
                        provider: facet.provider.clone(),
                        native_id: facet.native_id.clone(),
                    })),
                    on_exit: ExitPolicy::Remove,
                    parent: Some(parent_key),
                    launch_mode: phux_core::LaunchMode::Immediate,
                },
            },
            actor,
        )?;
        let phux_core::Reply::Ok { created, .. } = commit.applied.reply else {
            return Err(Refusal::NotFound);
        };
        let id = created
            .resource
            .and_then(|k| self.sessions.registry.resource_id(&k))
            .ok_or(Refusal::NotFound)?;
        if let Some(agent) = self
            .sessions
            .registry
            .resource_mut(id)
            .and_then(|d| d.agent.as_mut())
        {
            agent.state.clone_from(&facet.state);
        }
        self.activation_started(id, None);
        Ok(id)
    }

    // ---- executor reports -------------------------------------------------
}
