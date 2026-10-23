//! One runtime client, one event, path-answer, and file-upload drain.
//!
//! The runtime owns dial, reconnect, and input delivery. This module installs
//! the sole [`Listener`] and wakes a bounded channel; it does not poll and it
//! does not retry a refused command.

mod activity;
mod commands;
mod connect;
pub mod home;
pub mod org;
pub mod path;
pub mod transcript;

#[cfg(test)]
use activity::note_agent;
pub(crate) use commands::session_create;

use phux_client_core::session::agent_stream::AgentPublication;
pub use phux_client_ffi::projection::agent::{AgentAttention, AgentState};
use phux_client_ffi::projection::{agent::badge, id, status};
use phux_client_runtime::approvals::{Approval, Decision, DecisionOutcome};
use phux_client_runtime::control::{
    AgentSessionInfo, ControlOptions, ControlPlane, DeliveryOutcome, Event, FileUploadReceipt,
    OrganizationReplica, PaneDescriptor, PathAnswer, ServerInfo, SessionDescriptor,
    SessionRecovery, SpawnRequest, Status, Topology, mint_operation,
};
use phux_client_runtime::{Client, ClientOptions, Listener, Runtime, Target};
use phux_protocol::ResourceId;
use phux_protocol::caps::{ServerFeature, ServerFeatureExt};
use phux_protocol::ids::{ApprovalId, ArrangementKey, OperationId, SessionKey};
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

    /// What a person reads for the state. An undeclared state reads as nothing.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self.state {
            AgentState::Blocked => "needs you",
            AgentState::Done => "finished",
            AgentState::Working => "working",
            AgentState::Idle => "idle",
            AgentState::Unknown => "",
        }
    }

    /// Declared working.
    #[must_use]
    pub fn working(&self) -> bool {
        self.state == AgentState::Working
    }

    /// Declared idle.
    #[must_use]
    pub fn idle(&self) -> bool {
        self.state == AgentState::Idle
    }

    /// Declared finished (`done`).
    #[must_use]
    pub fn finished(&self) -> bool {
        self.state == AgentState::Done
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

/// How one decision this window sent ended (ADR-0128). Never retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalOutcome {
    /// The approval decided.
    pub id: ApprovalId,
    /// What was sent.
    pub decision: Decision,
    /// How it ended.
    pub outcome: DecisionOutcome,
}

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
    /// The last drain's agent metadata changed a badge.
    agents_noted: bool,
    /// Committed native conversations keyed by their AgentSession resource.
    pub conversations: HashMap<ResourceId, transcript::Conversation>,
    conversation_heads: HashMap<ResourceId, ResourceId>,
    /// Project label from each session's typed organization attributes.
    pub projects: HashMap<u32, String>,
    /// The project a person linked each session into, which outranks its
    /// repository.
    pub links: HashMap<u32, String>,
    /// Canonical checkout root for new terminals in that session.
    pub directories: HashMap<u32, String>,
    /// The arrangement group that shows each terminal, by name.
    pub groups: HashMap<ResourceId, String>,
    /// Server-held approvals awaiting a decision (ADR-0128), oldest first.
    pub approvals: Vec<Approval>,
    approval_outcomes: Vec<ApprovalOutcome>,
    session: String,
    home_key: Option<SessionKey>,
    key_bootstrap_pending: bool,
    pinned_home: bool,
    expected_authority: Option<phux_protocol::ids::AuthorityId>,
    pinned_verified_epoch: Option<u64>,
    /// Arrangements asked for on this connection, so each is read once.
    fetched: HashSet<ArrangementKey>,
    closed: bool,
    known: HashSet<ResourceId>,
    /// Terminals this window spawned or attached, admitted before the topology
    /// refresh lists them. The successful reply names the terminal and the server
    /// pumps its replica at once, so a new pane can appear without waiting a
    /// whole `GET_STATE` round trip (which is a full server snapshot on a
    /// remote link). Cleared on any connection boundary or close.
    admitted: HashSet<ResourceId>,
    identity: Option<ConnectionIdentity>,
    project_tag: Option<(String, String)>,
    /// Inventory-only connections never attach terminal streams or fetch layouts.
    browsing: bool,
    path_answers: Vec<PathAnswer>,
    file_uploads: Vec<FileUploadReceipt>,
    wake_rx: async_channel::Receiver<()>,
    revision: u64,
}

/// The home session: by its persistent key when one is known, else by name.
pub(crate) fn home_entry<'a>(
    topology: &'a Topology,
    name: &str,
    key: Option<SessionKey>,
) -> Option<&'a SessionDescriptor> {
    match key {
        Some(key) => topology.session_keyed(&key),
        None => topology.session_named(name),
    }
}

pub(crate) fn sync_home_name(topology: &Topology, name: &mut String, key: &mut Option<SessionKey>) {
    if let Some(home) = home_entry(topology, name, *key) {
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

fn remote_target(host: &str) -> anyhow::Result<Target> {
    if deterministic_dial::armed() {
        return Ok(Target::uds(PathBuf::from(
            "/deterministic-desktop-remote.sock",
        )));
    }
    Target::resolve(host, None).map_err(|error| anyhow::anyhow!(error))
}

impl Connection {
    /// Path answers stashed by [`Self::drain`] since the last take.
    pub fn take_path_answers(&mut self) -> Vec<PathAnswer> {
        std::mem::take(&mut self.path_answers)
    }

    /// File-upload receipts stashed by [`Self::drain`] since the last take.
    ///
    /// The queue is lossless. A caller that ignores a receipt must not paste.
    pub fn take_file_upload_receipts(&mut self) -> Vec<FileUploadReceipt> {
        std::mem::take(&mut self.file_uploads)
    }

    /// Decisions resolved since the last take. Lossless; none is retried.
    pub fn take_approval_outcomes(&mut self) -> Vec<ApprovalOutcome> {
        std::mem::take(&mut self.approval_outcomes)
    }

    /// Decide one held action. The outcome arrives through
    /// [`Self::take_approval_outcomes`]. `false` when nothing was sent: the
    /// connection is closed, the server holds nothing for approval, or a
    /// decision on `id` is already in flight.
    #[must_use]
    pub fn decide_approval(&self, id: ApprovalId, decision: Decision) -> bool {
        !self.closed && self.client.decide_approval(id, decision)
    }

    /// The terminal is inventoried or explicitly admitted, and accepts input.
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

    /// Snapshot the fences for a window's placements with one short control-lock
    /// acquisition. A paint wake must not take the lock once per hidden pane
    /// while the same lock also admits keyboard input.
    pub fn fenced_placements<'a>(
        &self,
        terminals: impl Iterator<Item = &'a ResourceId>,
    ) -> Vec<ResourceId> {
        if self.closed {
            return Vec::new();
        }
        let known: Vec<_> = terminals.filter(|id| self.known.contains(*id)).collect();
        if known.is_empty() {
            return Vec::new();
        }
        self.client.with_control(|control| {
            known
                .into_iter()
                .filter(|id| control.delivery_fenced(id))
                .cloned()
                .collect()
        })
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
        home_entry(self.topology.as_ref()?, &self.session, self.home_key).map(|session| session.id)
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

    /// Read the organization replica in place, under the runtime's control
    /// lock. `read` must not call back into the client.
    pub(crate) fn with_organization<R>(&self, read: impl FnOnce(&OrganizationReplica) -> R) -> R {
        self.client
            .with_control(|control| read(control.organization()))
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
        let (missing, groups) = self.with_organization(|replica| {
            let missing: Vec<_> = replica
                .sessions()
                .iter()
                .filter_map(|session| session.arrangement)
                .filter(|arrangement| replica.arrangement(arrangement).is_none())
                .collect();
            (missing, org::group_names(replica))
        });
        for arrangement in missing {
            if self.fetched.insert(arrangement) {
                let _ = self.client.fetch_arrangement(arrangement);
            }
        }
        self.groups = groups;
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
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
            outcome: DeliveryOutcome::Refused,
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

/// An exact resource the server admitted for this client's spawn/attach.
/// Refusals never grant readiness, even when they name a resource.
fn admitted_terminal(event: &Event) -> Option<&ResourceId> {
    match event {
        Event::TerminalSpawned {
            terminal_id: Some(id),
            error: None,
            ..
        }
        | Event::TerminalAttached {
            terminal_id: id,
            error: None,
            ..
        } => Some(id),
        _ => None,
    }
}

/// Keeps [`open_client`] on [`Runtime::embedded`] for the current thread.
///
/// Dropping the guard restores [`Runtime::connect`]. Unit tests run on a
/// thread pool, so the flag is thread-local and cleared on unwind. The
/// headless window benchmark (`tests/pixels.rs`) arms it too and scripts the
/// server side by feeding frames to the window's embedded client.
#[doc(hidden)]
#[must_use = "dropping the guard starts the runtime driver again"]
pub struct DeterministicDial;

mod deterministic_dial {
    use std::cell::Cell;

    use super::DeterministicDial;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }

    impl DeterministicDial {
        /// Route this thread's dials to [`Runtime::embedded`] until dropped.
        pub fn arm() -> Self {
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
mod authority_tests;
#[cfg(test)]
mod inventory_tests;

#[cfg(test)]
mod remote_acceptance;

#[cfg(test)]
mod paint_wake_tests;

#[cfg(test)]
mod agent_predicates {
    use super::{AgentAttention, AgentInfo, AgentState};

    fn info(state: AgentState, attention: AgentAttention) -> AgentInfo {
        AgentInfo {
            name: "agent".into(),
            kind: None,
            state,
            attention,
            changed_at_ms: 0,
        }
    }

    fn flags(info: &AgentInfo) -> (bool, bool, bool, bool) {
        (
            info.needs_you(),
            info.working(),
            info.idle(),
            info.finished(),
        )
    }

    #[test]
    fn state_predicates_share_one_classifier() {
        // needs you, working, idle, finished
        assert_eq!(
            flags(&info(AgentState::Blocked, AgentAttention::High)),
            (true, false, false, false)
        );
        assert_eq!(
            flags(&info(AgentState::Done, AgentAttention::Low)),
            (true, false, false, true),
            "finished still needs you; it is not a second classifier"
        );
        assert_eq!(
            flags(&info(AgentState::Working, AgentAttention::Normal)),
            (false, true, false, false)
        );
        assert_eq!(
            flags(&info(AgentState::Working, AgentAttention::High)),
            (true, true, false, false),
            "asking loudly does not stop the agent being working"
        );
        assert_eq!(
            flags(&info(AgentState::Idle, AgentAttention::None)),
            (false, false, true, false)
        );
        assert_eq!(
            flags(&info(AgentState::Idle, AgentAttention::High)),
            (true, false, true, false)
        );
        assert_eq!(
            flags(&info(AgentState::Unknown, AgentAttention::Low)),
            (false, false, false, false)
        );
        assert_eq!(
            flags(&info(AgentState::Unknown, AgentAttention::High)),
            (true, false, false, false)
        );
    }
}

#[cfg(test)]
mod note_agent_tests;
