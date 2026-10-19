//! Activity drain, connection identity admission and metadata folding.
use super::*;

impl Connection {
    /// Wait on this, then [`Self::drain`]. The channel is bounded and coalesced.
    #[must_use]
    pub fn activity(&self) -> async_channel::Receiver<()> {
        self.wake_rx.clone()
    }

    /// Take events, path answers, and file-upload receipts, and refresh snapshots the batch can change.
    pub fn drain(&mut self) -> Vec<Event> {
        let observation = self.client.take_activity();
        if observation.status != self.status {
            log::info!(
                "connection {:?} -> {:?} epoch={} error={:?}",
                self.status,
                observation.status,
                observation.connection_epoch,
                observation.last_error
            );
        }
        log::debug!("drained {} event(s)", observation.events.len());
        for event in &observation.events {
            log::trace!("event {event:?}");
        }
        self.path_answers.extend(self.client.take_path_answers());
        self.file_uploads
            .extend(self.client.take_file_upload_receipts());
        let changed = observation.events.iter().any(Event::changes_session_graph);
        let was = self.status;
        if self.pinned_home && observation.events.iter().any(starts_connection) {
            self.key_bootstrap_pending = true;
            self.pinned_verified_epoch = None;
        }
        self.status = observation.status;
        if observation.last_error.is_some() {
            self.error = observation.last_error;
        }
        if changed {
            self.apply_snapshot(observation.topology, observation.connection_epoch);
        }
        // A successful (re)attach leaves no failure to show.
        if self.status == Status::Attached && was != Status::Attached {
            self.error = None;
        }
        self.accept(&observation.events, observation.connection_epoch);
        self.attach_pinned_home();
        // Inventory needs the same arrangement-group label fallback as an
        // attached window. These are metadata reads, never terminal attaches.
        if changed {
            self.follow_arrangements(&observation.events);
        }
        self.revision = self.revision.saturating_add(1);
        observation.events
    }

    fn apply_snapshot(&mut self, topology: Option<Topology>, epoch: u64) {
        self.known = topology
            .iter()
            .flat_map(|topology| &topology.panes)
            .map(|pane| pane.terminal_id.clone())
            .collect();
        // Once the topology lists a spawn, the admission has done its job.
        self.admitted.retain(|id| !self.known.contains(id));
        if let Some(server) = self.client.server() {
            let next = ConnectionIdentity {
                server_id: status::server_id(&server),
                connection_epoch: epoch,
            };
            if self
                .identity
                .as_ref()
                .is_some_and(|identity| identity.server_id != next.server_id)
            {
                self.conversations.clear();
                self.conversation_heads.clear();
            }
            if self.identity.as_ref() != Some(&next) {
                self.forget_session_metadata();
            }
            self.identity = Some(next);
            self.server = Some(server);
        }
        self.topology = topology.or_else(|| self.client.topology());
        if let Some(topology) = &self.topology {
            sync_home_name(topology, &mut self.session, &mut self.home_key);
        }
        self.attach_pinned_home();
    }

    pub(crate) fn session_validated(&self) -> bool {
        !self.pinned_home || self.pinned_verified_epoch == Some(self.connection_epoch())
    }

    /// Resolve the pinned key before asking the runtime to attach, and verify
    /// it again after the runtime's redial before admitting the workspace.
    pub(crate) fn expect_authority(&mut self, authority: Option<phux_protocol::ids::AuthorityId>) {
        self.expected_authority = authority;
    }

    fn attach_pinned_home(&mut self) {
        if !self.pinned_home || !matches!(self.status, Status::Negotiated | Status::Attached) {
            return;
        }
        let (live, authority) =
            self.with_organization(|replica| (replica.is_live(), replica.authority()));
        if !live {
            if self.status == Status::Attached {
                self.status = Status::Negotiated;
            }
            return;
        }
        if self.expected_authority.is_some() && authority != self.expected_authority {
            self.error = Some("The host now reports a different authority. Refresh Work and select the session again.".into());
            self.status = Status::Failed;
            self.client.close();
            return;
        }
        if self.key_bootstrap_pending {
            let Some(id) = self.home_session() else {
                self.refuse_replaced_home();
                return;
            };
            self.key_bootstrap_pending = false;
            self.client
                .attach_session(AttachTarget::ById(phux_protocol::ids::SessionId::new(id)));
            self.status = Status::Negotiated;
            return;
        }
        self.verify_pinned_home();
    }

    fn verify_pinned_home(&mut self) {
        let epoch = self.connection_epoch();
        if self.status != Status::Attached || self.pinned_verified_epoch == Some(epoch) {
            return;
        }
        // A browsing runtime selects a healthy live session with per-terminal
        // subscriptions; a connection-level attach instead records its home.
        let attached = self
            .client
            .attached_session()
            .or_else(|| self.client.selected_session());
        if attached.is_none() || attached != self.home_session() {
            self.refuse_replaced_home();
        } else {
            self.pinned_verified_epoch = Some(epoch);
        }
    }

    fn refuse_replaced_home(&mut self) {
        self.key_bootstrap_pending = false;
        self.error = Some(format!(
            "The original home session no longer matches this connection; refusing a name replacement (expected live session {:?}, attached {:?}, key observed {}).",
            self.home_session(),
            self.client.attached_session(),
            self.home_key.is_some_and(
                |key| self.with_organization(|replica| replica.session(&key).is_some())
            )
        ));
        self.status = Status::Failed;
        self.client.close();
    }

    /// Whether the last [`Self::drain`]'s agent metadata changed any badge.
    /// Metadata that repeats a badge changes nothing a window shows.
    #[must_use]
    pub fn agents_noted(&self) -> bool {
        self.agents_noted
    }

    fn accept(&mut self, events: &[Event], epoch: u64) {
        self.agents_noted = false;
        let boundary = events.iter().rposition(starts_connection);
        let same_identity = self
            .identity
            .as_ref()
            .is_some_and(|identity| identity.connection_epoch == epoch);
        for (index, event) in events.iter().enumerate() {
            let live = same_identity && boundary.is_none_or(|boundary| index > boundary);
            match event {
                _ if starts_connection(event) => {
                    self.forget_session_metadata();
                    // A new boundary can reuse ids; never trust an old admission.
                    self.admitted.clear();
                }
                Event::ServerError { message, .. } => self.error = Some(message.clone()),
                Event::ApprovalDecision {
                    id,
                    decision,
                    outcome,
                } => self.approval_outcomes.push(ApprovalOutcome {
                    id: *id,
                    decision: *decision,
                    outcome: outcome.clone(),
                }),
                Event::AgentMetadata { terminal_id, value } if live => {
                    self.agents_noted |=
                        note_agent(&mut self.agents, terminal_id, value.as_deref(), now_ms());
                }
                Event::AgentPublication {
                    publication,
                    session,
                } if live => self.publish(publication, session, epoch),
                Event::AgentSessionClosed { terminal_id, .. } if live => {
                    if let Some(conversation) = self.conversations.get_mut(terminal_id) {
                        conversation.closed = true;
                        conversation.revision = conversation.revision.saturating_add(1);
                    }
                }
                Event::SessionProject { name, project } => {
                    note_project(&mut self.project_tag, name, project.as_deref());
                }
                Event::TerminalClosed { terminal_id, .. } => {
                    self.agents.remove(terminal_id);
                    self.admitted.remove(terminal_id);
                    self.conversation_heads.remove(terminal_id);
                    self.conversations
                        .retain(|_, conversation| &conversation.parent != terminal_id);
                }
                _ => {
                    if let Some(id) = admitted_terminal(event) {
                        // A successful spawn/attach admits this exact resource;
                        // a topology refresh listing it is a later round trip.
                        self.admitted.insert(id.clone());
                    }
                }
            }
        }
        self.conversations.retain(|resource, conversation| {
            self.conversation_heads.get(&conversation.parent) == Some(resource)
        });
        if events
            .iter()
            .any(|event| matches!(event, Event::ApprovalsChanged) || starts_connection(event))
        {
            self.approvals = self.client.approvals();
        }
        if events.iter().any(|event| {
            event.changes_session_graph() || matches!(event, Event::SessionProject { .. })
        }) {
            self.rebuild_projects();
        }
    }

    /// Fold one committed publication into its conversation and keep the most
    /// recently active open stream as its terminal's head.
    fn publish(&mut self, publication: &AgentPublication, session: &AgentSessionInfo, epoch: u64) {
        let Some(parent) = session
            .parent
            .as_ref()
            .filter(|parent| self.known.contains(*parent))
        else {
            return;
        };
        let resource = &publication.resource_id;
        let native = self.client.with_control(|control| {
            control
                .engine()
                .and_then(|engine| engine.native_agent_view(resource))
        });
        if !self.conversations.contains_key(resource)
            && let Some(conversation) = transcript::Conversation::new(publication, session, epoch)
        {
            self.conversations.insert(resource.clone(), conversation);
        }
        let Some(conversation) = self
            .conversations
            .get_mut(resource)
            .filter(|conversation| !conversation.closed && &conversation.parent == parent)
        else {
            return;
        };
        conversation.apply(publication, session, epoch);
        // Losing a projection is not a cutover to PTY input,
        // nor permission to keep old ACP controls interactive.
        if let Some(native) = native {
            conversation.native = Some(native);
        } else if let Some(native) = conversation.native.as_mut() {
            native.connected = false;
        }
        let activity = conversation.last_activity_ms;
        let newer = self.conversation_heads.get(parent).is_none_or(|head| {
            head == resource
                || self.conversations.get(head).is_none_or(|head| {
                    activity > head.last_activity_ms
                        || (head.closed && activity == head.last_activity_ms)
                })
        });
        if newer {
            self.conversation_heads
                .insert(parent.clone(), resource.clone());
        }
    }

    fn forget_session_metadata(&mut self) {
        self.agents.clear();
        self.project_tag = None;
        self.projects.clear();
        self.directories.clear();
    }

    fn rebuild_projects(&mut self) {
        self.projects.clear();
        self.directories.clear();
        if self.organization() {
            let (projects, directories) = (&mut self.projects, &mut self.directories);
            self.client.with_control(|control| {
                for record in control.organization().sessions() {
                    let Some(id) = record.live.map(phux_protocol::ids::SessionId::get) else {
                        continue;
                    };
                    if let Some(project) = session_attr(&record.attrs, "project") {
                        projects.insert(id, project.to_owned());
                    }
                    if let Some(directory) = session_attr(&record.attrs, "directory") {
                        directories.insert(id, directory.to_owned());
                    }
                }
            });
        }
        // Older servers report only the single, legacy project tag.
        if let Some((name, project)) = &self.project_tag {
            for session in self.topology.iter().flat_map(|topology| &topology.sessions) {
                if &session.name == name {
                    self.projects
                        .entry(session.id)
                        .or_insert_with(|| project.clone());
                }
            }
        }
    }
}

fn session_attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, value)| key == name && !value.is_empty())
        .map(|(_, value)| value.as_str())
}

fn note_project(tag: &mut Option<(String, String)>, name: &str, project: Option<&str>) {
    match project.map(str::trim).filter(|project| !project.is_empty()) {
        Some(project) => *tag = Some((name.to_owned(), project.to_owned())),
        None if tag.as_ref().is_some_and(|(tagged, _)| tagged == name) => *tag = None,
        None => {}
    }
}

/// Fold one badge into `agents`. True when the map changed.
pub(super) fn note_agent(
    agents: &mut HashMap<ResourceId, AgentInfo>,
    terminal: &ResourceId,
    bytes: Option<&[u8]>,
    now: u64,
) -> bool {
    let folded = badge(terminal, bytes);
    if folded.name.is_empty() {
        return agents.remove(terminal).is_some();
    }
    let previous = agents.get(terminal);
    let changed_at_ms = match previous {
        Some(info) if (info.state, info.attention) == (folded.state, folded.attention) => {
            info.changed_at_ms
        }
        _ => now,
    };
    let next = AgentInfo {
        name: folded.name,
        kind: folded.kind,
        state: folded.state,
        attention: folded.attention,
        changed_at_ms,
    };
    if previous == Some(&next) {
        return false;
    }
    agents.insert(terminal.clone(), next);
    true
}
