//! Creating resources: a Terminal with its session, or a child of one.

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
        let launch = LaunchSpec::Terminal(launch);
        let resource = NewResource {
            kind: launch.kind(),
            name: None,
            launch: Some(launch),
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

    /// Create `resource` as a child of `parent`, in the parent's session,
    /// and record it running: its engine starts in the same lock. Its
    /// `parent` field is set to `parent`'s key.
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_child(
        &mut self,
        parent: ResourceId,
        mut resource: NewResource,
        actor: &Actor,
    ) -> Result<ResourceId, Refusal> {
        let record = self
            .sessions
            .registry
            .record(parent)
            .ok_or(Refusal::NotFound)?;
        let session = record.session;
        resource.parent = Some(record.key);
        let commit = self.commit(
            Op::ResourceCreate {
                placement: None,
                session: Some(session),
                resource,
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
        self.activation_started(id, None);
        Ok(id)
    }

    /// Create a document resource (an accessibility tree, a pixel surface, a
    /// typed document) under `parent`, labelled `name`; `launch` names the
    /// kind and what its producer observes. See [`Self::create_child`].
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_document(
        &mut self,
        parent: ResourceId,
        launch: LaunchSpec,
        actor: &Actor,
    ) -> Result<ResourceId, Refusal> {
        self.create_child(parent, NewResource::child(launch, None), actor)
    }

    // ---- executor reports -------------------------------------------------
}
