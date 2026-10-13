//! One runtime client, one event and path-answer drain.
//!
//! The runtime owns dial, reconnect, and input delivery. This module installs
//! the sole [`Listener`] and wakes a bounded channel; it does not poll and it
//! does not retry a refused command.

pub mod path;

pub use path::{
    PathRow, PathTarget, PickerState, accept_path_answer, begin_picker, can_offer_path_picker,
    directory_for_terminal, display_path, host_for_terminal, prepared_insertion, same_target,
    shell_quote_path, start_query,
};

use phux_client_ffi::projection::{agent::badge, id, status};
use phux_client_runtime::control::{
    Event, PaneDescriptor, PathAnswer, ServerInfo, SpawnRequest, Status, Topology,
};
use phux_client_runtime::control::ControlOptions;
use phux_client_runtime::{Client, ClientOptions, Listener, Runtime, Target};
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::{AttachTarget, RolePolicy};
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
    /// Lowercase lifecycle word.
    pub state: String,
    /// Lowercase attention word.
    pub attention: String,
    /// Milliseconds since the Unix epoch when state or attention last changed.
    pub changed_at_ms: u64,
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
    /// Toast title.
    pub title: &'static str,
    /// Toast body.
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
    /// Session id to `phux.session.project/v1` tag.
    pub projects: HashMap<u32, String>,
    session: String,
    closed: bool,
    known: HashSet<ResourceId>,
    identity: Option<ConnectionIdentity>,
    project_tag: Option<(String, String)>,
    path_answers: Vec<PathAnswer>,
    wake_rx: async_channel::Receiver<()>,
    revision: u64,
    paint_clock: u64,
    every_terminal: u64,
    paint: HashMap<ResourceId, u64>,
}

struct ActivityListener {
    tx: async_channel::Sender<()>,
}

impl Listener for ActivityListener {
    fn on_activity(&self) {
        let _ = self.tx.try_send(());
    }
}

impl Connection {
    /// Dial `socket` and attach `session`. Viewport `(0, 0)` casts no geometry vote.
    pub fn connect(socket: PathBuf, session: String) -> anyhow::Result<Self> {
        if socket.as_os_str().is_empty() {
            anyhow::bail!("InvalidConnectOptions");
        }
        if session.is_empty() || session.len() > 4096 || session.contains('\0') {
            anyhow::bail!("InvalidSessionName");
        }
        let client = Runtime::connect(
            Target::uds(socket),
            ClientOptions {
                control: ControlOptions {
                    client_name: "phux-desktop".to_owned(),
                    viewport: (0, 0),
                    attach: Some(AttachTarget::ByName(session.clone())),
                    attach_role: Some(RolePolicy::PRIMARY),
                    auto_attach_foreign_spawns: false,
                    ..ControlOptions::default()
                },
                ..ClientOptions::default()
            },
        )?;
        Ok(Self::from_client(client, session))
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
            projects: HashMap::new(),
            session,
            closed: false,
            known: HashSet::new(),
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
        let observation = self.client.take_observation();
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
        self.path_answers
            .extend(self.client.take_path_answers());
        let changed = observation.events.iter().any(structural);
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
        self.accept(&observation.events, observation.connection_epoch);
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
        !self.closed && self.known.contains(id) && self.client.input_ready(id)
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

    /// Topology panes, or an empty slice.
    #[must_use]
    pub fn panes(&self) -> &[PaneDescriptor] {
        self.topology
            .as_ref()
            .map(|topology| topology.panes.as_slice())
            .unwrap_or(&[])
    }

    /// Session id whose name is the attach name.
    #[must_use]
    pub fn home_session(&self) -> Option<u32> {
        self.topology
            .as_ref()?
            .sessions
            .iter()
            .find(|session| session.name == self.session)
            .map(|session| session.id)
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
        self.client.with_control(|control| queue_spawn(control, request))
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
        self.identity.as_ref().map(|identity| identity.server_id.clone())
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
        self.paint.retain(|id, _| self.known.contains(id));
        if let Some(server) = self.client.server() {
            let next = ConnectionIdentity {
                server_id: status::server_id(&server),
                connection_epoch: epoch,
            };
            if self.identity.as_ref() != Some(&next) {
                self.agents.clear();
                self.project_tag = None;
                self.projects.clear();
            }
            self.identity = Some(next);
            self.server = Some(server);
        }
        self.topology = topology.or_else(|| self.client.topology());
        self.rebuild_projects();
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
            if let Event::SessionProject { name, project } = event {
                note_project(&mut self.project_tag, name, project.as_deref());
            }
            if let Event::TerminalClosed { terminal_id, .. } = event {
                self.agents.remove(terminal_id);
                self.paint.remove(terminal_id);
            }
        }
        self.rebuild_projects();
    }

    fn rebuild_projects(&mut self) {
        self.projects.clear();
        let Some((name, project)) = &self.project_tag else {
            return;
        };
        for session in self.topology.iter().flat_map(|topology| &topology.sessions) {
            if &session.name == name {
                self.projects.insert(session.id, project.clone());
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
    let state = format!("{:?}", folded.state).to_lowercase();
    let attention = format!("{:?}", folded.attention).to_lowercase();
    let previous = agents.get(terminal).map(|info| {
        (info.state.clone(), info.attention.clone(), info.changed_at_ms)
    });
    let changed = previous.as_ref().is_none_or(|(previous_state, previous_attention, _)| {
        previous_state != &state || previous_attention != &attention
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
    !matches!(
        event,
        Event::TerminalChanged { .. }
            | Event::InputDelivery { .. }
            | Event::AgentMetadata { .. }
            | Event::SessionProject { .. }
    )
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
    let target = request.session_id.or(control.attached_session()).ok_or_else(|| {
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
        return Err(command("NotNegotiated", "Terminate refused", "Not connected."));
    }
    let server_id = control.server().map(|server| status::server_id(server));
    let Some(server_id) = server_id else {
        return Err(command("NotNegotiated", "Terminate refused", "Not connected."));
    };
    if identity.connection_epoch != control.connection_epoch() || identity.server_id != server_id {
        return Err(command(
            "StaleConnectionIdentity",
            "Terminate refused",
            "The connection changed; try again.",
        ));
    }
    if control.status() != Status::Attached {
        return Err(command("NotAttached", "Terminate refused", "Not connected."));
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
        return Err(command("TerminalNotFound", "Terminate refused", "That terminal is gone."));
    }
    Ok(control.kill_terminal(id))
}

fn validate_spawn(request: &SpawnRequest) -> Result<(), CommandError> {
    let mut total = 0_usize;
    if let Some(argv) = &request.command {
        if argv.is_empty() || argv.len() > MAX_ARGS || argv[0].is_empty() {
            return Err(command("InvalidSpawnCommand", "Could not start a terminal", "Invalid command."));
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
        return Err(command("InvalidSpawnText", "Could not start a terminal", "The request is too large."));
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
