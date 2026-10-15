//! One runtime client, one event and path-answer drain.
//!
//! The runtime owns dial, reconnect, and input delivery. This module installs
//! the sole [`Listener`] and wakes a bounded channel; it does not poll and it
//! does not retry a refused command.

pub mod home;
pub mod org;
pub mod path;
pub mod transcript;

pub use path::{
    PathRow, PathTarget, PickerState, accept_path_answer, begin_picker, can_offer_path_picker,
    directory_for_terminal, display_path, host_for_terminal, prepared_insertion, same_target,
    shell_quote_path, start_query,
};

pub use phux_client_ffi::projection::agent::{AgentAttention, AgentState};
use phux_client_ffi::projection::{agent::badge, id, status};
use phux_client_runtime::control::ControlOptions;
use phux_client_runtime::control::{
    DeliveryOutcome, Event, PaneDescriptor, PathAnswer, ServerInfo, SpawnRequest, Status, Topology,
    mint_operation,
};
use phux_client_runtime::{Client, ClientOptions, Listener, Runtime, Target};
use phux_protocol::ResourceId;
use phux_protocol::caps::{ServerFeature, ServerFeatureExt};
use phux_protocol::ids::{ArrangementKey, OperationId, SessionKey};
use phux_protocol::wire::frame::{AttachTarget, Command, LaunchSpec, RolePolicy, SessionLifetime};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A folded `phux.agent/v1` badge the sidebar can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    /// Human-facing name.
    pub name: String,
    /// Provider slug, when declared.
    pub kind: Option<String>,
    /// Declared lifecycle.
    pub state: AgentState,
    /// Effective attention level.
    pub attention: AgentAttention,
    /// Milliseconds since the Unix epoch when state or attention last changed.
    pub changed_at_ms: u64,
}

impl AgentInfo {
    /// Blocked, finished, or asking loudly: the agents a person should look at.
    #[must_use]
    pub fn needs_you(&self) -> bool {
        matches!(self.state, AgentState::Blocked | AgentState::Done) || self.urgent()
    }

    /// The agent asks to be noticed above everything else.
    #[must_use]
    pub fn urgent(&self) -> bool {
        self.attention == AgentAttention::High
    }

    /// What a person reads for the state.
    #[must_use]
    pub fn word(&self) -> &'static str {
        state_word(self.state)
    }
}

/// What a person reads for an agent state. An undeclared state reads as nothing.
#[must_use]
pub fn state_word(state: AgentState) -> &'static str {
    match state {
        AgentState::Blocked => "needs you",
        AgentState::Done => "finished",
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::Unknown => "",
    }
}

/// Most pressing first: blocked, finished, working, idle, undeclared.
#[must_use]
pub fn state_rank(state: AgentState) -> u8 {
    match state {
        AgentState::Blocked => 0,
        AgentState::Done => 1,
        AgentState::Working => 2,
        AgentState::Idle => 3,
        AgentState::Unknown => 4,
    }
}

/// Identity a mutation must still match. The hex server id is the NAPI spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionIdentity {
    /// Lowercase hex of `HELLO_OK` server id bytes.
    pub server_id: String,
    /// Connection epoch.
    pub connection_epoch: u64,
}

/// A command the runtime or this gate refused before a frame was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// Stable reason token.
    pub reason: &'static str,
    /// Notification title.
    pub title: &'static str,
    /// Notification body.
    pub body: String,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.body)
    }
}

impl std::error::Error for CommandError {}

/// One desktop connection. Public snapshots are refreshed by [`Self::drain`].
pub struct Connection {
    /// The runtime client. Do not replace its listener.
    pub client: Arc<Client>,
    /// Raw runtime status.
    pub status: Status,
    /// Latest topology, refreshed on structural drains.
    pub topology: Option<Topology>,
    /// Latest negotiated server, after `HELLO_OK`.
    pub server: Option<ServerInfo>,
    /// Last failure message.
    pub error: Option<String>,
    /// Agent badges keyed by terminal.
    pub agents: HashMap<ResourceId, AgentInfo>,
    /// Committed native conversations keyed by their AgentSession resource.
    pub conversations: HashMap<ResourceId, transcript::Conversation>,
    conversation_heads: HashMap<ResourceId, ResourceId>,
    /// Project label from each session's typed organization attributes.
    pub projects: HashMap<u32, String>,
    /// Canonical checkout root for new terminals in that session.
    pub directories: HashMap<u32, String>,
    /// The arrangement group that shows each terminal, by name.
    pub groups: HashMap<ResourceId, String>,
    session: String,
    home_key: Option<SessionKey>,
    key_bootstrap_pending: bool,
    pinned_home: bool,
    pinned_verified_epoch: Option<u64>,
    /// Arrangements asked for on this connection, so each is read once.
    fetched: HashSet<ArrangementKey>,
    closed: bool,
    known: HashSet<ResourceId>,
    /// Terminals this window spawned, admitted before the post-spawn topology
    /// refresh lists them. The spawn reply names the terminal and the server
    /// pumps its replica at once, so a new pane can appear without waiting a
    /// whole `GET_STATE` round trip (which is a full server snapshot on a
    /// remote link). Cleared on any connection boundary or close.
    admitted: HashSet<ResourceId>,
    identity: Option<ConnectionIdentity>,
    project_tag: Option<(String, String)>,
    path_answers: Vec<PathAnswer>,
    wake_rx: async_channel::Receiver<()>,
    revision: u64,
    paint_clock: u64,
    every_terminal: u64,
    paint: HashMap<ResourceId, u64>,
}

pub(crate) fn sync_home_name(topology: &Topology, name: &mut String, key: &mut Option<SessionKey>) {
    let home = match *key {
        Some(key) => topology.session_keyed(&key),
        None => topology.session_named(name),
    };
    if let Some(home) = home {
        *key = home.key;
        name.clone_from(&home.name);
    }
}

fn designated_intake_session(topology: &Topology, id: u32) -> bool {
    topology
        .sessions
        .iter()
        .any(|session| session.id == id && session.key.is_some() && session.key == topology.inbox)
}

struct ActivityListener {
    tx: async_channel::Sender<()>,
}

impl Listener for ActivityListener {
    fn on_activity(&self) {
        let _ = self.tx.try_send(());
    }
}

/// Dial `target`, unless a deterministic window fixture is armed. That fixture
/// has no server; `Runtime::connect` would still spawn the driver, and
/// `ActivityListener` would wake GPUI's test scheduler from that thread.
fn open_client(target: Target, control: ControlOptions) -> anyhow::Result<Client> {
    #[cfg(test)]
    if deterministic_dial::armed() {
        return Ok(Runtime::embedded(control));
    }
    Ok(Runtime::connect(
        target,
        ClientOptions {
            control,
            ..ClientOptions::default()
        },
    )?)
}

impl Connection {
    /// Dial `socket` and attach `session`. Viewport `(0, 0)` casts no geometry vote.
    pub fn connect(socket: PathBuf, session: String) -> anyhow::Result<Self> {
        Self::connect_with_key(socket, session, None)
    }

    pub(crate) fn connect_with_key(
        socket: PathBuf,
        session: String,
        key: Option<SessionKey>,
    ) -> anyhow::Result<Self> {
        if socket.as_os_str().is_empty() {
            anyhow::bail!("InvalidConnectOptions");
        }
        Self::connect_target(Target::uds(socket), session, key)
    }

    /// Attach `session` on a host from the remote registry.
    pub fn connect_remote(host: &str, session: String) -> anyhow::Result<Self> {
        Self::connect_remote_with_key(host, session, None)
    }

    pub(crate) fn connect_remote_with_key(
        host: &str,
        session: String,
        key: Option<SessionKey>,
    ) -> anyhow::Result<Self> {
        let target = Target::resolve(host, None).map_err(|error| anyhow::anyhow!(error))?;
        Self::connect_target(target, session, key)
    }

    fn connect_target(
        target: Target,
        session: String,
        key: Option<SessionKey>,
    ) -> anyhow::Result<Self> {
        if session.is_empty() || session.len() > 4096 || session.contains('\0') {
            anyhow::bail!("InvalidSessionName");
        }
        let client = open_client(
            target,
            ControlOptions {
                client_name: "phux-desktop".to_owned(),
                viewport: (0, 0),
                attach: key.is_none().then(|| AttachTarget::ByName(session.clone())),
                attach_role: Some(RolePolicy::PRIMARY),
                auto_attach_foreign_spawns: false,
                subscribe_agent_sessions: true,
                ..ControlOptions::default()
            },
        )?;
        let mut connection = Self::from_client(client, session);
        connection.home_key = key;
        connection.key_bootstrap_pending = key.is_some();
        connection.pinned_home = key.is_some();
        Ok(connection)
    }

    fn from_client(client: Client, session: String) -> Self {
        let (tx, wake_rx) = async_channel::bounded(1);
        client.set_listener(Arc::new(ActivityListener { tx }));
        let status = client.status();
        Self {
            client: Arc::new(client),
            status,
            topology: None,
            server: None,
            error: None,
            agents: HashMap::new(),
            conversations: HashMap::new(),
            conversation_heads: HashMap::new(),
            projects: HashMap::new(),
            directories: HashMap::new(),
            groups: HashMap::new(),
            session,
            home_key: None,
            key_bootstrap_pending: false,
            pinned_home: false,
            pinned_verified_epoch: None,
            fetched: HashSet::new(),
            closed: false,
            known: HashSet::new(),
            admitted: HashSet::new(),
            identity: None,
            project_tag: None,
            path_answers: Vec::new(),
            wake_rx,
            revision: 0,
            paint_clock: 0,
            every_terminal: 0,
            paint: HashMap::new(),
        }
    }

    /// Wait on this, then [`Self::drain`]. The channel is bounded and coalesced.
    #[must_use]
    pub fn activity(&self) -> async_channel::Receiver<()> {
        self.wake_rx.clone()
    }

    /// Take events and path answers, and refresh snapshots the batch can change.
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
        let changed = observation.events.iter().any(structural);
        let was = self.status;
        if changed {
            self.apply_snapshot(
                observation.status,
                observation.last_error.clone(),
                observation.topology.clone(),
                observation.connection_epoch,
            );
        } else {
            self.status = observation.status;
            if observation.last_error.is_some() {
                self.error = observation.last_error;
            }
        }
        // A successful (re)attach leaves no failure to show.
        if self.status == Status::Attached && was != Status::Attached {
            self.error = None;
        }
        self.accept(&observation.events, observation.connection_epoch);
        if changed {
            self.follow_arrangements(&observation.events);
        }
        self.invalidate_paint(&observation.events);
        self.revision = self.revision.saturating_add(1);
        observation.events
    }

    /// Path answers stashed by [`Self::drain`] since the last take.
    pub fn take_path_answers(&mut self) -> Vec<PathAnswer> {
        std::mem::take(&mut self.path_answers)
    }

    /// The terminal is in the latest topology and the runtime will accept input.
    #[must_use]
    pub fn ready(&self, id: &ResourceId) -> bool {
        !self.closed
            && (self.known.contains(id) || self.admitted.contains(id))
            && self.client.input_ready(id)
    }

    /// Unknown delivery is outstanding for a known terminal.
    #[must_use]
    pub fn fenced(&self, id: &ResourceId) -> bool {
        !self.closed && self.known.contains(id) && self.client.delivery_fenced(id)
    }

    /// One pane from the latest topology.
    #[must_use]
    pub fn pane(&self, id: &ResourceId) -> Option<&PaneDescriptor> {
        self.topology.as_ref()?.pane(id)
    }

    /// Most recently published stream for this exact terminal, including a closed stream.
    pub fn conversation(&self, terminal: &ResourceId) -> Option<&transcript::Conversation> {
        self.conversations
            .get(self.conversation_heads.get(terminal)?)
    }

    /// Topology panes, or an empty slice.
    #[must_use]
    pub fn panes(&self) -> &[PaneDescriptor] {
        self.topology
            .as_ref()
            .map(|topology| topology.panes.as_slice())
            .unwrap_or(&[])
    }

    /// Attached home session, following its persistent identity through renames.
    #[must_use]
    pub fn home_session(&self) -> Option<u32> {
        self.topology
            .as_ref()?
            .sessions
            .iter()
            .find(|session| match self.home_key {
                Some(key) => session.key == Some(key),
                None => session.name == self.session,
            })
            .map(|session| session.id)
    }

    pub(crate) const fn home_key(&self) -> Option<SessionKey> {
        self.home_key
    }

    pub(crate) fn is_intake_session(&self, id: u32) -> bool {
        self.topology
            .as_ref()
            .is_some_and(|topology| designated_intake_session(topology, id))
    }

    /// Whether the server takes typed organization commands (`ORGANIZATION`).
    #[must_use]
    pub fn organization(&self) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::Organization))
    }

    /// The arrangement group name that shows `terminal`, when one is known.
    #[must_use]
    pub fn group_name(&self, terminal: &ResourceId) -> Option<&str> {
        self.groups.get(terminal).map(String::as_str)
    }

    /// Read each session's arrangement once per connection and rebuild the
    /// terminal-to-group names from the replica.
    fn follow_arrangements(&mut self, events: &[Event]) {
        if events.iter().any(starts_connection) {
            self.fetched.clear();
        }
        if !self.organization() {
            self.groups.clear();
            return;
        }
        let replica = self.client.organization();
        for session in replica.sessions() {
            if let Some(arrangement) = session.arrangement
                && replica.arrangement(&arrangement).is_none()
                && self.fetched.insert(arrangement)
            {
                let _ = self.client.fetch_arrangement(arrangement);
            }
        }
        self.groups = org::group_names(&replica);
    }

    /// Create a persistent session whose first terminal starts in
    /// `directory`. The outcome is [`Event::OrganizationOperation`] for the
    /// returned id.
    ///
    /// # Errors
    ///
    /// A sentence when the journal refuses the submission.
    pub fn create_session(&self, name: &str, directory: &str) -> Result<OperationId, String> {
        let mut launch = LaunchSpec::new(Vec::new());
        launch.cwd = Some(directory.to_owned());
        self.client
            .submit_organization(Command::SessionCreate {
                operation: mint_operation(),
                name: name.to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: Vec::new(),
                launch: Some(Box::new(launch)),
            })
            .map_err(|error| error.to_string())
    }

    /// Kill `session` with one `SESSION_KILL`. The result is
    /// [`Event::OrganizationOperation`] for the returned operation; `None`
    /// when the server does not take the command.
    #[must_use]
    pub fn kill_session(&self, session: SessionKey) -> Option<OperationId> {
        if self.closed {
            return None;
        }
        self.client.kill_session(session).ok()
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
        self.client
            .with_control(|control| queue_termination(control, identity, terminal))
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

    /// Whether `PATH_QUERY` was advertised.
    #[must_use]
    pub fn has_path_query(&self) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has_ext(phux_protocol::caps::ServerFeatureExt::PathQuery))
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

    /// Paint clock for one terminal. A background flood does not bump every pane.
    #[must_use]
    pub fn paint_revision(&self, id: &ResourceId) -> u64 {
        self.every_terminal
            .max(self.paint.get(id).copied().unwrap_or(0))
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

    fn apply_snapshot(
        &mut self,
        status: Status,
        error: Option<String>,
        topology: Option<Topology>,
        epoch: u64,
    ) {
        self.status = status;
        if error.is_some() {
            self.error = error;
        }
        self.known.clear();
        if let Some(topology) = &topology {
            for pane in &topology.panes {
                self.known.insert(pane.terminal_id.clone());
            }
        }
        // Once the topology lists a spawn, the admission has done its job.
        self.admitted.retain(|id| !self.known.contains(id));
        self.paint
            .retain(|id, _| self.known.contains(id) || self.admitted.contains(id));
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
                self.agents.clear();
                self.project_tag = None;
                self.projects.clear();
                self.directories.clear();
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

    /// Resolve the pinned key before asking the runtime to attach, and verify
    /// it again after the runtime's redial before admitting the workspace.
    fn attach_pinned_home(&mut self) {
        if !self.pinned_home || !matches!(self.status, Status::Negotiated | Status::Attached) {
            return;
        }
        if !self.client.organization().is_live() {
            if self.status == Status::Attached {
                self.status = Status::Negotiated;
            }
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
            self.home_key
                .is_some_and(|key| self.client.organization().session(&key).is_some())
        ));
        self.status = Status::Failed;
        self.client.close();
    }

    fn accept(&mut self, events: &[Event], epoch: u64) {
        let boundary = events.iter().rposition(starts_connection);
        let same_identity = self
            .identity
            .as_ref()
            .is_some_and(|identity| identity.connection_epoch == epoch);
        for (index, event) in events.iter().enumerate() {
            if starts_connection(event) {
                self.agents.clear();
                self.project_tag = None;
                self.projects.clear();
                self.directories.clear();
                // A new boundary can reuse ids; never trust an old admission.
                self.admitted.clear();
            }
            if let Event::ServerError { message, .. } = event {
                self.error = Some(message.clone());
            }
            if let Event::AgentMetadata { terminal_id, value } = event {
                let after_boundary = boundary.is_none_or(|boundary| index > boundary);
                if same_identity && after_boundary {
                    note_agent(&mut self.agents, terminal_id, value.as_deref(), now_ms());
                }
            }
            let after_boundary = boundary.is_none_or(|boundary| index > boundary);
            if same_identity && after_boundary {
                match event {
                    Event::AgentPublication {
                        publication,
                        session,
                    } => {
                        if let Some(parent) = session.parent.as_ref()
                            && self.known.contains(parent)
                        {
                            let resource = &publication.resource_id;
                            if !self.conversations.contains_key(resource)
                                && let Some(conversation) =
                                    transcript::Conversation::new(publication, session, epoch)
                            {
                                self.conversations.insert(resource.clone(), conversation);
                            }
                            if let Some(conversation) = self.conversations.get_mut(resource)
                                && !conversation.closed
                                && &conversation.parent == parent
                            {
                                conversation.apply(publication, session, epoch);
                            }
                            if let Some(conversation) = self.conversations.get(resource)
                                && !conversation.closed
                                && &conversation.parent == parent
                            {
                                let current = self.conversation_heads.get(parent);
                                let newer = current.is_none_or(|head| {
                                    head == resource
                                        || self.conversations.get(head).is_none_or(|head| {
                                            conversation.last_activity_ms > head.last_activity_ms
                                                || (head.closed
                                                    && conversation.last_activity_ms
                                                        == head.last_activity_ms)
                                        })
                                });
                                if newer {
                                    self.conversation_heads
                                        .insert(parent.clone(), resource.clone());
                                }
                            }
                        }
                    }
                    Event::AgentSessionClosed { terminal_id, .. } => {
                        if let Some(conversation) = self.conversations.get_mut(terminal_id) {
                            conversation.closed = true;
                            conversation.revision = conversation.revision.saturating_add(1);
                        }
                    }
                    _ => {}
                }
            }
            if let Event::SessionProject { name, project } = event {
                note_project(&mut self.project_tag, name, project.as_deref());
            }
            if let Some(id) = own_spawn(event) {
                // Our own spawn is live the moment the server answers it; the
                // topology refresh that lists it is a later round trip.
                self.admitted.insert(id.clone());
            }
            if let Event::TerminalClosed { terminal_id, .. } = event {
                self.agents.remove(terminal_id);
                self.paint.remove(terminal_id);
                self.admitted.remove(terminal_id);
                self.conversation_heads.remove(terminal_id);
                self.conversations
                    .retain(|_, conversation| &conversation.parent != terminal_id);
            }
        }
        self.conversations.retain(|resource, conversation| {
            self.conversation_heads.get(&conversation.parent) == Some(resource)
        });
        if events.iter().any(structural)
            || events
                .iter()
                .any(|event| matches!(event, Event::SessionProject { .. }))
        {
            self.rebuild_projects();
        }
    }

    fn rebuild_projects(&mut self) {
        self.projects.clear();
        self.directories.clear();
        if self.organization() {
            let replica = self.client.organization();
            for record in replica.sessions() {
                let Some(id) = record.live.map(phux_protocol::ids::SessionId::get) else {
                    continue;
                };
                if let Some(project) = session_attr(&record.attrs, "project") {
                    self.projects.insert(id, project.to_owned());
                }
                if let Some(directory) = session_attr(&record.attrs, "directory") {
                    self.directories.insert(id, directory.to_owned());
                }
            }
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

    fn invalidate_paint(&mut self, events: &[Event]) {
        let mut output = HashSet::new();
        let mut all = events.is_empty();
        for event in events {
            match event {
                Event::TerminalChanged { terminal_id } => {
                    output.insert(terminal_id.clone());
                }
                Event::AgentMetadata { .. } | Event::SessionProject { .. } => {}
                _ => all = true,
            }
        }
        if !all && output.is_empty() {
            return;
        }
        self.paint_clock = self.paint_clock.saturating_add(1);
        if all {
            self.every_terminal = self.paint_clock;
        }
        for id in output {
            self.paint.insert(id, self.paint_clock);
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

fn note_agent(
    agents: &mut HashMap<ResourceId, AgentInfo>,
    terminal: &ResourceId,
    bytes: Option<&[u8]>,
    now: u64,
) {
    let folded = badge(terminal, bytes);
    if folded.name.is_empty() {
        agents.remove(terminal);
        return;
    }
    let (state, attention) = (folded.state, folded.attention);
    let previous = agents
        .get(terminal)
        .map(|info| (info.state, info.attention, info.changed_at_ms));
    let changed = previous
        .as_ref()
        .is_none_or(|(previous_state, previous_attention, _)| {
            *previous_state != state || *previous_attention != attention
        });
    let changed_at_ms = if changed {
        now
    } else {
        previous.map(|(_, _, at)| at).unwrap_or(now)
    };
    agents.insert(
        terminal.clone(),
        AgentInfo {
            name: folded.name,
            kind: folded.kind,
            state,
            attention,
            changed_at_ms,
        },
    );
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// An event that can change topology, status, or the server snapshot.
#[must_use]
pub fn structural(event: &Event) -> bool {
    event.changes_session_graph()
}

/// The session just became attached. The desk paints every placed terminal once.
#[must_use]
pub fn session_attached(event: &Event) -> bool {
    matches!(event, Event::StatusChanged(Status::Attached))
}

/// Terminal output and a delivered input. The chrome tree does not depend on it.
#[must_use]
pub fn paint_wake(event: &Event) -> bool {
    match event {
        Event::TerminalChanged { .. } => true,
        Event::InputDelivery { outcome, .. } => *outcome != DeliveryOutcome::Refused,
        _ => false,
    }
}

/// The refusal text for acknowledged input, when the outcome was refused.
#[must_use]
pub fn refused_input(event: &Event) -> Option<String> {
    match event {
        Event::InputDelivery {
            outcome: phux_client_runtime::control::DeliveryOutcome::Refused,
            message,
            ..
        } => Some(if message.is_empty() {
            "The terminal did not accept the input.".to_owned()
        } else {
            message.clone()
        }),
        _ => None,
    }
}

/// A new connection boundary. Negotiated is included because the old binding
/// projected it as Connecting.
#[must_use]
pub fn starts_connection(event: &Event) -> bool {
    matches!(
        event,
        Event::StatusChanged(Status::Connecting | Status::Negotiated)
            | Event::ConnectionOpened { .. }
    )
}

const MAX_ARGS: usize = 256;
const MAX_TEXT: usize = 64 * 1024;

fn queue_spawn(
    control: &mut phux_client_runtime::control::ControlPlane,
    mut request: SpawnRequest,
) -> Result<u32, CommandError> {
    if !control.handshake_ready() {
        return Err(command(
            "NotNegotiated",
            "Not connected",
            "Wait for the server, or reconnect with ⌘R.",
        ));
    }
    if control.status() != Status::Attached {
        return Err(command(
            "NotAttached",
            "Not connected",
            "Wait for the server, or reconnect with ⌘R.",
        ));
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
        .ok_or_else(|| {
            command(
                "InvalidSessionId",
                "Not connected",
                "Wait for the server, or reconnect with ⌘R.",
            )
        })?;
    if control.topology().is_none() {
        return Err(command(
            "TopologyUnavailable",
            "Not connected",
            "Wait for the server, or reconnect with ⌘R.",
        ));
    }
    let known = control
        .topology()
        .is_some_and(|topology| topology.sessions.iter().any(|session| session.id == target));
    if !known {
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
        return Err(command(
            "UnsupportedSpawnInitialSize",
            "Could not start a terminal",
            "This server does not accept an initial size.",
        ));
    }
    request.session_id = Some(target);
    Ok(control.spawn_terminal(request))
}

fn queue_termination(
    control: &mut phux_client_runtime::control::ControlPlane,
    identity: &ConnectionIdentity,
    id: &ResourceId,
) -> Result<u32, CommandError> {
    if !control.handshake_ready() {
        return Err(command(
            "NotNegotiated",
            "Terminate refused",
            "Not connected.",
        ));
    }
    let server_id = control.server().map(status::server_id);
    let Some(server_id) = server_id else {
        return Err(command(
            "NotNegotiated",
            "Terminate refused",
            "Not connected.",
        ));
    };
    if identity.connection_epoch != control.connection_epoch() || identity.server_id != server_id {
        return Err(command(
            "StaleConnectionIdentity",
            "Terminate refused",
            "The connection changed; try again.",
        ));
    }
    if control.status() != Status::Attached {
        return Err(command(
            "NotAttached",
            "Terminate refused",
            "Not connected.",
        ));
    }
    if matches!(id, ResourceId::Satellite { .. }) {
        return Err(command(
            "UnsupportedSatelliteTermination",
            "Terminate refused",
            "Satellite terminals cannot be terminated from this window.",
        ));
    }
    let known = control
        .topology()
        .is_some_and(|topology| topology.panes.iter().any(|pane| &pane.terminal_id == id));
    if !known {
        return Err(command(
            "TerminalNotFound",
            "Terminate refused",
            "That terminal is gone.",
        ));
    }
    Ok(control.kill_terminal(id))
}

fn validate_spawn(request: &SpawnRequest) -> Result<(), CommandError> {
    let mut total = 0_usize;
    if let Some(argv) = &request.command {
        if argv.is_empty() || argv.len() > MAX_ARGS || argv[0].is_empty() {
            return Err(command(
                "InvalidSpawnCommand",
                "Could not start a terminal",
                "Invalid command.",
            ));
        }
        for arg in argv {
            count_text(&mut total, arg)?;
        }
    }
    if let Some(cwd) = &request.cwd {
        if cwd.is_empty() {
            return Err(command(
                "InvalidSpawnDirectory",
                "Could not start a terminal",
                "A new session needs the focused directory or a chosen folder.",
            ));
        }
        count_text(&mut total, cwd)?;
    }
    if let Some(env) = &request.env {
        if env.len() > MAX_ARGS {
            return Err(command(
                "InvalidSpawnEnvironment",
                "Could not start a terminal",
                "Invalid environment.",
            ));
        }
        let mut names = HashSet::new();
        for (name, value) in env {
            if name.is_empty() || name.contains('=') || !names.insert(name) {
                return Err(command(
                    "InvalidSpawnEnvironment",
                    "Could not start a terminal",
                    "Invalid environment.",
                ));
            }
            count_text(&mut total, name)?;
            count_text(&mut total, value)?;
        }
    }
    Ok(())
}

fn count_text(total: &mut usize, text: &str) -> Result<(), CommandError> {
    if text.contains('\0') || text.len() > MAX_TEXT.saturating_sub(*total) {
        return Err(command(
            "InvalidSpawnText",
            "Could not start a terminal",
            "The request is too large.",
        ));
    }
    *total += text.len();
    Ok(())
}

fn command(reason: &'static str, title: &'static str, body: &str) -> CommandError {
    CommandError {
        reason,
        title,
        body: body.to_owned(),
    }
}

/// Encode a terminal the way saved layouts spell it.
#[must_use]
pub fn terminal_label(id: &ResourceId) -> String {
    id::encode(id)
}

/// Parse a saved or wire terminal spelling.
#[must_use]
pub fn parse_terminal(value: &str) -> Option<ResourceId> {
    id::parse(value)
}

/// The terminal id of a spawn this window owns and may place before the
/// post-spawn topology refresh lists it. A refused spawn names none.
#[must_use]
pub fn own_spawn(event: &Event) -> Option<&ResourceId> {
    match event {
        Event::TerminalSpawned {
            terminal_id: Some(id),
            error: None,
            ..
        } => Some(id),
        _ => None,
    }
}

/// Keeps [`open_client`] on [`Runtime::embedded`] for the current thread.
///
/// Dropping the guard restores [`Runtime::connect`]. Unit tests run on a
/// thread pool, so the flag is thread-local and cleared on unwind.
#[cfg(test)]
#[must_use = "dropping the guard starts the runtime driver again"]
pub(crate) struct DeterministicDial;

#[cfg(test)]
mod deterministic_dial {
    use std::cell::Cell;

    use super::DeterministicDial;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }

    impl DeterministicDial {
        pub(crate) fn arm() -> Self {
            ARMED.with(|armed| armed.set(true));
            Self
        }
    }

    impl Drop for DeterministicDial {
        fn drop(&mut self) {
            ARMED.with(|armed| armed.set(false));
        }
    }

    pub(super) fn armed() -> bool {
        ARMED.with(Cell::get)
    }
}

#[cfg(test)]
mod paint_wake_tests {
    use super::{own_spawn, paint_wake, session_attached, structural};
    use phux_client_runtime::control::{DeliveryOutcome, Event, Status};
    use phux_protocol::ResourceId;

    #[test]
    fn output_and_delivered_input_skip_chrome() {
        let terminal_id = ResourceId::local(1);
        assert!(paint_wake(&Event::TerminalChanged {
            terminal_id: terminal_id.clone(),
        }));
        assert!(!structural(&Event::TerminalChanged { terminal_id }));
        assert!(paint_wake(&Event::InputDelivery {
            delivery_id: 1,
            outcome: DeliveryOutcome::Delivered,
            code: None,
            message: String::new(),
        }));
        assert!(!paint_wake(&Event::InputDelivery {
            delivery_id: 1,
            outcome: DeliveryOutcome::Refused,
            code: None,
            message: String::new(),
        }));
        assert!(structural(&Event::TopologyChanged));
        assert!(!paint_wake(&Event::TopologyChanged));
        assert!(session_attached(&Event::StatusChanged(Status::Attached)));
        assert!(!session_attached(&Event::StatusChanged(Status::Connecting)));
        assert!(!session_attached(&Event::TopologyChanged));
    }

    #[test]
    fn only_a_successful_spawn_is_admitted_before_the_topology_lists_it() {
        let terminal = ResourceId::local(7);
        assert_eq!(
            own_spawn(&Event::TerminalSpawned {
                request_id: 1,
                terminal_id: Some(terminal.clone()),
                error: None,
            }),
            Some(&terminal)
        );
        assert_eq!(
            own_spawn(&Event::TerminalSpawned {
                request_id: 2,
                terminal_id: Some(terminal),
                error: Some("refused".into()),
            }),
            None
        );
        assert_eq!(
            own_spawn(&Event::TerminalSpawned {
                request_id: 3,
                terminal_id: None,
                error: None,
            }),
            None
        );
        assert_eq!(own_spawn(&Event::TopologyChanged), None);
    }
}
