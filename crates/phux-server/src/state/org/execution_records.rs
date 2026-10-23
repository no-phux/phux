//! Resource creation, retirement and startup reconciliation.
use super::{
    Actor, AgentLaunch, ExecState, ExitOutcome, ExitPolicy, LaunchSpec, NewResource, Op, Outcome,
    Reconciled, Refusal, ResourceId, ResourceKind, ServerState, SessionId, SessionLifetime,
    TerminalLaunch, TerminalTarget,
};
impl ServerState {
    // ---- creation --------------------------------------------------------

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

    /// End `resource`'s activation if it is still current, then remove its
    /// record: a reap, a kill, a purge. A no-op during shutdown, which
    /// leaves records for the next start to reconcile.
    pub(crate) fn retire_record(&mut self, resource: ResourceId, outcome: ExitOutcome) {
        if self.org.shutting_down {
            return;
        }
        // The key's removal is already durable. Writing it again would apply
        // against memory that still has the row.
        if self
            .sessions
            .registry
            .resource_key(resource)
            .is_some_and(|key| self.org.retirement_suppressed.contains(&key))
        {
            return;
        }
        self.activation_ended(resource, outcome, false);
        if self.org.terminating.remove(&resource) {
            return;
        }
        let Some(record) = self.sessions.registry.record(resource) else {
            return;
        };
        let key = record.key;
        if let Err(refusal) = self.commit_system(Op::ResourceRemove { resource: key }) {
            tracing::warn!(?resource, %refusal, "record removal refused");
        }
    }

    pub(super) fn current_activation(
        &self,
        resource: ResourceId,
        accept: impl Fn(ExecState) -> bool,
    ) -> Option<(phux_core::ResourceKey, phux_core::ActivationId)> {
        let record = self.sessions.registry.record(resource)?;
        let activation = record.activation.as_ref()?;
        accept(activation.state).then_some((record.key, activation.id))
    }

    /// Startup reconciliation (ADR-0162 §5): every activation a previous
    /// run left current or lost, and no engine in this process vouches for,
    /// becomes `Lost` when its process still runs on this boot with the
    /// recorded start time, and `Completed`/`Unobserved` otherwise. Nothing
    /// is relaunched. Idempotent.
    pub fn reconcile(&mut self) -> Reconciled {
        self.reconcile_with(crate::reconcile::boot_id(), crate::reconcile::observe)
    }

    /// [`Self::reconcile`] against a given boot identity and OS probe.
    pub fn reconcile_with(
        &mut self,
        boot: Option<[u8; 16]>,
        observe: impl Fn(i32) -> crate::reconcile::Observed,
    ) -> Reconciled {
        use crate::reconcile::{Verdict, decide};
        let candidates: Vec<_> = self
            .org()
            .resources()
            .filter_map(|r| {
                let a = r.activation.as_ref()?;
                Some((r.key, a.id, a.state, a.host_proc))
            })
            .collect();
        let mut out = Reconciled::default();
        for (key, activation, state, proc) in candidates {
            let vouched = self
                .sessions
                .registry
                .resource_id(&key)
                .is_some_and(|id| self.resource_handle(id).is_some());
            let op = match decide(state, vouched, proc, boot, &observe) {
                Verdict::Keep => continue,
                Verdict::Lost => {
                    out.lost += 1;
                    Op::ActivationLost {
                        resource: key,
                        activation,
                    }
                }
                Verdict::Unobserved => {
                    out.unobserved += 1;
                    Op::ActivationEnded {
                        resource: key,
                        activation,
                        state: ExecState::Completed,
                        outcome: Outcome::Unobserved,
                        diagnostic: None,
                    }
                }
            };
            if let Err(refusal) = self.commit_system(op) {
                tracing::warn!(resource = %key, %refusal, "reconcile verdict not recorded");
            }
        }
        out
    }

    /// Whether any activation is current: the server still executes work
    /// (R13).
    #[must_use]
    pub fn has_live_activation(&self) -> bool {
        self.org()
            .resources()
            .any(|r| r.activation.as_ref().is_some_and(|a| a.state.is_current()))
    }
}
