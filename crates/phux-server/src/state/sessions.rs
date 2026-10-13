use phux_core::ids::{ResourceId, SessionId};
use phux_core::registry::Registry;
use phux_core::session::Session;
use phux_core::{Actor, ExitPolicy, Op, Refusal, SessionLifetime, TerminalLaunch};
use phux_protocol::ids::ResourceId as WireResourceId;

use super::org::TerminalTarget;
use super::{KeepEmptyOutcome, RenameOutcome, ServerState};

impl ServerState {
    /// The organization registry: every session and resource record with
    /// its live handle.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.sessions.registry
    }

    /// The registry, mutably, for live facet edits (geometry, cwd, agent
    /// state). Records change only through [`Self::commit`].
    pub const fn registry_mut(&mut self) -> &mut Registry {
        &mut self.sessions.registry
    }

    /// Most-recently-touched live session, if any. Resolves
    /// `AttachTarget::Last`.
    #[must_use]
    pub fn most_recently_touched_session(&self) -> Option<SessionId> {
        self.sessions.most_recently_touched()
    }

    /// Whether this server has any session-touch history, even if every
    /// touched session has since gone away.
    #[must_use]
    pub(crate) fn has_session_touch_history(&self) -> bool {
        self.sessions.has_touch_history()
    }

    /// Mark `session` as touched by attach/input/focus activity.
    pub fn touch_session(&mut self, session: SessionId) {
        self.sessions.touch(session);
    }

    /// The Terminal a session-addressed action without a pane lands on
    /// (R14): the member that last took human input, else the first leaf of
    /// the arrangement's first group, else the oldest Terminal member.
    #[must_use]
    pub fn active_pane_of_session(&self, session: SessionId) -> Option<ResourceId> {
        self.sessions.default_pane_of(session)
    }

    /// The session a request with no session and no touch history lands
    /// on: the oldest session with a live pane, else the oldest one other
    /// than the Inbox. An empty Inbox is never picked: older clients know
    /// nothing of it.
    #[must_use]
    pub fn fallback_session(&self) -> Option<SessionId> {
        let inbox = self.org().authority().inbox_session;
        let mut sessions: Vec<(phux_core::Stamp, SessionId, bool)> = self
            .sessions
            .registry
            .sessions()
            .map(|(id, s)| (s.created, id, s.key == inbox))
            .collect();
        sessions.sort_by_key(|(created, _, is_inbox)| (*is_inbox, *created));
        sessions
            .iter()
            .find(|(_, id, _)| !self.legacy_panes_of(*id).is_empty())
            .or_else(|| sessions.iter().find(|(_, _, is_inbox)| !is_inbox))
            .map(|(_, id, _)| *id)
    }

    /// Borrow the session named `name`, if it exists.
    #[must_use]
    pub fn session_by_name(&self, name: &str) -> Option<&Session> {
        self.sessions.by_name(name)
    }

    /// The [`SessionId`] named `name`.
    pub(crate) fn find_session_by_name(&self, name: &str) -> Option<SessionId> {
        self.sessions.find_by_name(name)
    }

    /// `session`'s Terminal members, in membership order.
    #[must_use]
    pub fn session_terminals(&self, session: SessionId) -> Vec<ResourceId> {
        self.sessions.terminal_members(session)
    }

    /// Rename `current` to `new_name` as the owner; see
    /// [`Self::rename_session_as`].
    pub fn rename_session(&mut self, current: &str, new_name: &str) -> RenameOutcome {
        self.rename_session_as(current, new_name, &Actor::owner())
    }

    /// Commit a rename of `current` to `new_name` attributed to `actor`
    /// (names are unique); the outcome maps to `SESSION_NOT_FOUND`,
    /// `INVALID_COMMAND`, or success.
    pub fn rename_session_as(
        &mut self,
        current: &str,
        new_name: &str,
        actor: &Actor,
    ) -> RenameOutcome {
        let Some(record) = self.sessions.by_name(current) else {
            return RenameOutcome::NotFound;
        };
        let op = Op::SessionRename {
            session: record.key,
            name: new_name.to_owned(),
        };
        match self.commit(op, actor) {
            Ok(_) => RenameOutcome::Renamed,
            Err(Refusal::NotFound) => RenameOutcome::NotFound,
            Err(_) => RenameOutcome::NameTaken,
        }
    }

    /// Commit a persistent session named `name` with one Terminal record
    /// whose actor the caller starts. Tests and the no-PTY seed use it.
    ///
    /// # Panics
    ///
    /// If the name is taken or the store refuses the write.
    #[allow(
        clippy::expect_used,
        reason = "a seed must exist before anything serves"
    )]
    pub fn seed_session(&mut self, name: &str) -> (SessionId, ResourceId) {
        self.create_terminal(
            TerminalTarget::NewSession {
                name,
                lifetime: SessionLifetime::Persistent,
            },
            TerminalLaunch::default(),
            ExitPolicy::Remove,
            &Actor::system(),
        )
        .expect("seed session")
    }

    /// Commit an empty persistent session named `name` (the owner's
    /// `--empty`). `None` when the name is taken or the write is refused.
    pub fn seed_empty_session(&mut self, name: &str) -> Option<SessionId> {
        self.create_empty_session(name, SessionLifetime::Persistent, &Actor::owner())
            .ok()
    }

    /// Commit a Terminal record in `session` whose actor the caller starts.
    /// `None` if `session` is unknown or the write is refused.
    #[must_use]
    pub fn add_pane_to_session(&mut self, session: SessionId) -> Option<ResourceId> {
        self.create_terminal(
            TerminalTarget::Session(session),
            TerminalLaunch::default(),
            ExitPolicy::Remove,
            &Actor::owner(),
        )
        .ok()
        .map(|(_, id)| id)
    }

    /// The session holding the local Terminal `owner` (headless spawn
    /// targeting).
    #[must_use]
    pub fn session_of_terminal_owner(&self, owner: &WireResourceId) -> Option<SessionId> {
        let owner = self.terminal_from_wire(owner)?;
        self.sessions.registry.session_of(owner)
    }

    /// Commit an agent conversation bound to `parent` as the owner (tests
    /// and in-process tools). `None` when refused.
    pub fn new_agent_session(
        &mut self,
        parent: ResourceId,
        facet: &phux_core::resource::AgentFacet,
    ) -> Option<ResourceId> {
        self.create_agent_session(parent, facet, &Actor::owner())
            .ok()
    }

    /// Commit a membership move of `resource` into `to` at the revisions
    /// current now, attributed to `actor`. A no-op when it is already there.
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing moved.
    pub fn move_resource_to(
        &mut self,
        resource: ResourceId,
        to: SessionId,
        actor: &Actor,
    ) -> Result<(), Refusal> {
        let record = self
            .sessions
            .registry
            .record(resource)
            .ok_or(Refusal::NotFound)?;
        let (key, from) = (record.key, record.session);
        let to = self
            .sessions
            .registry
            .session_key(to)
            .ok_or(Refusal::NotFound)?;
        if from == to {
            return Ok(());
        }
        let revision = |k: &phux_core::SessionKey| {
            self.org()
                .session(k)
                .map(|s| s.revision)
                .unwrap_or_default()
        };
        let op = Op::ResourceMove {
            resource: key,
            from,
            from_expected: revision(&from),
            to,
            to_expected: revision(&to),
        };
        self.commit(op, actor).map(|_| ())
    }

    /// Sessions whose every Terminal member is in `targets` (and that have
    /// one): what a legacy group kill deletes once it has emptied them.
    #[must_use]
    pub fn sessions_covered_by(&self, targets: &[ResourceId]) -> Vec<SessionId> {
        self.sessions
            .registry
            .sessions()
            .map(|(id, _)| id)
            .filter(|id| {
                let terminals = self.sessions.terminal_members(*id);
                !terminals.is_empty() && terminals.iter().all(|t| targets.contains(t))
            })
            .collect()
    }

    /// Delete each of `sessions` that is now empty and still exists,
    /// attributed to `actor`; the designated Inbox is never deleted.
    /// Returns the deleted names. Their clients queue for `SESSION_KILLED`.
    pub fn delete_emptied_sessions(
        &mut self,
        sessions: &[SessionId],
        actor: &Actor,
    ) -> Vec<String> {
        let mut names = Vec::new();
        for id in sessions {
            let Some(record) = self.sessions.registry.session(*id) else {
                continue;
            };
            if !record.members.is_empty() || record.key == self.org().authority().inbox_session {
                continue;
            }
            let (key, name) = (record.key, record.name.clone());
            if self
                .commit(Op::SessionDelete { session: key }, actor)
                .is_ok()
            {
                names.push(name);
            }
        }
        names
    }

    /// Translate a `phux.session.keep_empty/v1` write: `true` sets the
    /// session persistent, `false` ephemeral, which deletes it when empty.
    pub fn set_session_keep_empty(
        &mut self,
        name: &str,
        keep: bool,
        actor: &Actor,
    ) -> KeepEmptyOutcome {
        let Some(record) = self.sessions.by_name(name) else {
            return KeepEmptyOutcome::NotFound;
        };
        let lifetime = if keep {
            SessionLifetime::Persistent
        } else {
            SessionLifetime::Ephemeral
        };
        if record.lifetime == lifetime {
            return KeepEmptyOutcome::Unchanged;
        }
        let op = Op::SessionSetLifetime {
            session: record.key,
            lifetime,
        };
        match self.commit(op, actor) {
            Ok(commit) if !commit.handles.sessions_removed.is_empty() => KeepEmptyOutcome::Removed,
            Ok(_) => KeepEmptyOutcome::Changed,
            Err(refusal) => {
                tracing::debug!(%name, %refusal, "keep-empty translation refused");
                KeepEmptyOutcome::Unchanged
            }
        }
    }
}
