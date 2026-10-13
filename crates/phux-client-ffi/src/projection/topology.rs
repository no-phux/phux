//! The session graph, flattened for a foreign caller.
//!
//! The runtime's [`Topology`] keeps sessions, panes, and agent sessions as
//! separate lists joined by id, and its organization replica holds the
//! records behind them. A binding's consumer paints a list, so each pane is
//! denormalized with its session and, when the session's arrangement is
//! held, its group; every terminal identity is already in its
//! [`crate::projection::id`] string form and every key in its full base32
//! text form. The projection is lossless: every field the runtime knows
//! about a session or resource reaches the binding, including records that
//! have no live handle (a completed, retained resource; a lost activation).

use phux_client_core::layout::{self, ArrangementDoc};
use phux_client_runtime::control::{AgentSessionDescriptor, OrganizationReplica, Topology};
use phux_protocol::ResourceId;
use phux_protocol::ResourceKind;
use phux_protocol::ids::{Revision, SessionKey};
use phux_protocol::wire::frame::{
    ActivationInfo, ExecState, ExitPolicy, Outcome, ResourceRecord, SessionLifetime,
};

use super::id;

/// The stable word for a session lifetime.
#[must_use]
pub const fn lifetime_word(lifetime: SessionLifetime) -> &'static str {
    match lifetime {
        SessionLifetime::Persistent => "persistent",
        SessionLifetime::Ephemeral => "ephemeral",
    }
}

/// The stable word for an activation's execution state.
#[must_use]
pub const fn exec_word(state: ExecState) -> &'static str {
    match state {
        ExecState::Starting => "starting",
        ExecState::Running => "running",
        ExecState::Stopping => "stopping",
        ExecState::Completed => "completed",
        ExecState::Failed => "failed",
        ExecState::Lost => "lost",
        ExecState::Abandoned => "abandoned",
    }
}

/// The stable word for a resource kind; an unknown wire kind reads
/// `kind-<tag>`.
#[must_use]
pub fn kind_word(kind: ResourceKind) -> String {
    match kind {
        ResourceKind::Terminal => "terminal".to_owned(),
        ResourceKind::AgentSession => "agent_session".to_owned(),
        other => format!("kind-{}", other.as_wire()),
    }
}

/// The stable word for an exit policy.
#[must_use]
pub const fn exit_policy_word(policy: ExitPolicy) -> &'static str {
    match policy {
        ExitPolicy::Remove => "remove",
        ExitPolicy::Retain => "retain",
    }
}

/// Lowercase hex of a 16-byte arrangement group identity.
#[must_use]
pub fn group_id_text(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    id.iter().fold(String::with_capacity(32), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// How an activation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// The process exited with this status.
    Exited(i32),
    /// The process was ended by this signal.
    Signaled(i32),
    /// The server did not observe the end.
    Unobserved,
    /// A shape this build does not name.
    Other,
}

impl ActivationOutcome {
    const fn from_wire(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Exited(code) => Self::Exited(code),
            Outcome::Signaled(signal) => Self::Signaled(signal),
            Outcome::Unobserved => Self::Unobserved,
            #[allow(
                unreachable_patterns,
                reason = "Outcome is non_exhaustive across crates"
            )]
            _ => Self::Other,
        }
    }
}

/// One activation: the current or most recent execution of a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    /// The activation id, monotonic per resource.
    pub id: u64,
    /// [`exec_word`] of its state.
    pub state: &'static str,
    /// How it ended, once it has.
    pub outcome: Option<ActivationOutcome>,
}

impl Activation {
    fn from_wire(info: &ActivationInfo) -> Self {
        Self {
            id: info.id.get(),
            state: exec_word(info.state),
            outcome: info.outcome.map(ActivationOutcome::from_wire),
        }
    }
}

/// One session visible in the server's snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The session id (live handle).
    pub id: u32,
    /// The session name.
    pub name: String,
    /// Clients attached to the session.
    pub attached_client_count: u16,
    /// Creation time, seconds since the Unix epoch.
    pub created_at_unix_secs: i64,
    /// The persistent key, as text, when the server reports it.
    pub key: Option<String>,
    /// [`lifetime_word`], when the server reports it.
    pub lifetime: Option<&'static str>,
    /// The record's revision, from the organization replica.
    pub revision: Option<u64>,
    /// Member resources.
    pub member_count: u32,
    /// Its arrangement key, as text, from the organization replica.
    pub arrangement: Option<String>,
    /// Whether it is imported work awaiting organization.
    pub awaiting_organization: bool,
    /// Whether it is the designated Inbox session.
    pub inbox: bool,
    /// Whether it is the attaching client's focused session.
    pub focused: bool,
}

/// One pane: a Terminal resource with a live handle, denormalized with its
/// session and arrangement group so no joins are needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    /// The terminal's string identity.
    pub terminal_id: String,
    /// The session it belongs to.
    pub session_id: u32,
    /// That session's name.
    pub session_name: String,
    /// That session's key, as text.
    pub session_key: Option<String>,
    /// The terminal's grid width.
    pub cols: u16,
    /// The terminal's grid height.
    pub rows: u16,
    /// The terminal's title, if known.
    pub title: Option<String>,
    /// The terminal's working directory, if known.
    pub cwd: Option<String>,
    /// Whether this is the attaching client's initial focus.
    pub is_focused: bool,
    /// The persistent key, as text.
    pub key: Option<String>,
    /// The record's revision.
    pub revision: Option<u64>,
    /// The record's display name.
    pub name: Option<String>,
    /// The current activation.
    pub activation: Option<Activation>,
    /// The arrangement group that shows it, when the session's arrangement
    /// is held: its identity as 32 hex digits.
    pub group_id: Option<String>,
    /// That group's position in the arrangement.
    pub group_index: Option<u32>,
    /// That group's name.
    pub group_name: Option<String>,
}

/// One `AgentSession` resource: a record stream bound to a Terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    /// The resource's string identity.
    pub id: String,
    /// The Terminal it is bound to, in string form.
    pub parent: Option<String>,
    /// The provider slug, if declared.
    pub provider: Option<String>,
    /// The provider's own session id, if declared.
    pub native_id: Option<String>,
    /// The server-derived state word, if declared.
    pub state: Option<String>,
}

impl From<AgentSessionDescriptor> for AgentSession {
    fn from(agent: AgentSessionDescriptor) -> Self {
        Self {
            id: id::encode(&agent.id),
            parent: agent.parent.as_ref().map(id::encode),
            provider: agent.provider,
            native_id: agent.native_id,
            state: agent.state,
        }
    }
}

/// One resource record from the organization replica, live or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The persistent key, as text.
    pub key: String,
    /// Its session's key, as text.
    pub session_key: String,
    /// Display name.
    pub name: Option<String>,
    /// [`kind_word`].
    pub kind: String,
    /// [`exit_policy_word`].
    pub on_exit: &'static str,
    /// The current or most recent activation.
    pub activation: Option<Activation>,
    /// The record's revision.
    pub revision: u64,
    /// Its live handle here, in string form, while it executes here.
    pub live: Option<String>,
}

impl Record {
    /// Project one replica record.
    #[must_use]
    pub fn from_wire(record: &ResourceRecord) -> Self {
        Self {
            key: record.key.to_string(),
            session_key: record.session.to_string(),
            name: record.name.clone(),
            kind: kind_word(record.kind),
            on_exit: exit_policy_word(record.on_exit),
            activation: record.activation.as_ref().map(Activation::from_wire),
            revision: record.revision.get(),
            live: record.live.as_ref().map(id::encode),
        }
    }
}

/// The flattened session graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGraph {
    /// Every session the server listed.
    pub sessions: Vec<Session>,
    /// Every pane the server listed, in its order.
    pub panes: Vec<Pane>,
    /// Every `AgentSession` resource the server listed.
    pub agent_sessions: Vec<AgentSession>,
    /// Every resource record the organization replica holds, in record
    /// order; empty without `ORGANIZATION`.
    pub records: Vec<Record>,
    /// The focused session.
    pub focused_session: u32,
    /// The attaching client's initial focused pane, in string form.
    pub focused_pane: String,
    /// The authority the records belong to, as text.
    pub authority: Option<String>,
    /// The designated Inbox session's key, as text.
    pub inbox: Option<String>,
}

/// Where a terminal sits in an arrangement.
struct Placement {
    id: [u8; 16],
    index: u32,
    name: String,
}

fn placement(doc: &ArrangementDoc, terminal: &ResourceId) -> Option<Placement> {
    doc.groups.iter().enumerate().find_map(|(index, group)| {
        let tree = group.state.tree.as_ref()?;
        layout::leaves(tree).contains(terminal).then(|| Placement {
            id: group.id,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            name: group.name.clone(),
        })
    })
}

/// Flatten one runtime topology snapshot, with the organization replica's
/// records and arrangements when the binding has them.
#[must_use]
pub fn session_graph(
    topology: Topology,
    organization: Option<&OrganizationReplica>,
) -> SessionGraph {
    let inbox = topology.inbox;
    let focused_session = topology.focused_session;
    let arrangement_of = |key: Option<SessionKey>| -> Option<ArrangementDoc> {
        let (replica, key) = (organization?, key?);
        replica
            .arrangement_document(&key)
            .map(|(_, _, document, _)| document)
    };
    let session_keys: Vec<(u32, Option<SessionKey>)> = topology
        .sessions
        .iter()
        .map(|session| (session.id, session.key))
        .collect();
    let docs: Vec<(u32, Option<ArrangementDoc>)> = session_keys
        .iter()
        .map(|(id, key)| (*id, arrangement_of(*key)))
        .collect();
    let sessions = topology
        .sessions
        .into_iter()
        .map(|session| Session {
            id: session.id,
            name: session.name,
            attached_client_count: session.attached_client_count,
            created_at_unix_secs: session.created_at_unix_secs,
            key: session.key.map(|key| key.to_string()),
            lifetime: session.lifetime.map(lifetime_word),
            revision: session.revision.map(Revision::get),
            member_count: session.members,
            arrangement: session.arrangement.map(|key| key.to_string()),
            awaiting_organization: session.awaiting_organization,
            inbox: session.key.is_some() && session.key == inbox,
            focused: session.id == focused_session,
        })
        .collect();
    let panes = topology
        .panes
        .into_iter()
        .map(|pane| {
            let session_key = session_keys
                .iter()
                .find(|(id, _)| *id == pane.session_id)
                .and_then(|(_, key)| *key);
            let placed = docs
                .iter()
                .find(|(id, _)| *id == pane.session_id)
                .and_then(|(_, doc)| doc.as_ref())
                .and_then(|doc| placement(doc, &pane.terminal_id));
            let activation = organization
                .and_then(|replica| replica.resource_by_live(&pane.terminal_id))
                .and_then(|record| record.activation.as_ref())
                .map(Activation::from_wire)
                .or_else(|| {
                    pane.activation
                        .zip(pane.exec)
                        .map(|(id, state)| Activation {
                            id: id.get(),
                            state: exec_word(state),
                            outcome: None,
                        })
                });
            Pane {
                terminal_id: id::encode(&pane.terminal_id),
                session_id: pane.session_id,
                session_name: pane.session_name,
                session_key: session_key.map(|key| key.to_string()),
                cols: pane.cols,
                rows: pane.rows,
                title: pane.title,
                cwd: pane.cwd,
                is_focused: pane.is_focused,
                key: pane.key.map(|key| key.to_string()),
                revision: pane.revision.map(Revision::get),
                name: pane.name,
                activation,
                group_id: placed.as_ref().map(|p| group_id_text(&p.id)),
                group_index: placed.as_ref().map(|p| p.index),
                group_name: placed.map(|p| p.name),
            }
        })
        .collect();
    let agent_sessions = topology
        .agent_sessions
        .into_iter()
        .map(AgentSession::from)
        .collect();
    SessionGraph {
        sessions,
        panes,
        agent_sessions,
        records: organization
            .filter(|replica| replica.authority().is_some())
            .map(|replica| replica.resources().iter().map(Record::from_wire).collect())
            .unwrap_or_default(),
        focused_session,
        focused_pane: id::encode(&topology.focused_pane),
        authority: topology.authority.map(|key| key.to_string()),
        inbox: inbox.map(|key| key.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::session_graph;
    use phux_client_core::layout::wire::HandleMap;
    use phux_client_runtime::control::{
        AgentSessionDescriptor, PaneDescriptor, SessionDescriptor, Topology,
    };
    use phux_protocol::ResourceId;
    use phux_protocol::ids::{ActivationId, ResourceKey, Revision, SessionKey};
    use phux_protocol::wire::frame::{ExecState, SessionLifetime};

    fn session_key() -> SessionKey {
        SessionKey::new([3; 16]).unwrap()
    }

    fn topology() -> Topology {
        Topology {
            sessions: vec![SessionDescriptor {
                id: 1,
                name: "work".to_owned(),
                window_count: 1,
                attached_client_count: 1,
                keep_empty: true,
                created_at_unix_secs: 42,
                key: Some(session_key()),
                lifetime: Some(SessionLifetime::Persistent),
                revision: Some(Revision::new(5)),
                members: 2,
                arrangement: None,
                awaiting_organization: true,
            }],
            panes: vec![PaneDescriptor {
                terminal_id: ResourceId::satellite("box", 4),
                session_id: 1,
                session_name: "work".to_owned(),
                cols: 80,
                rows: 24,
                title: Some("vim".to_owned()),
                cwd: None,
                is_focused: true,
                key: Some(ResourceKey::new([7; 16]).unwrap()),
                revision: Some(Revision::new(9)),
                name: Some("editor".to_owned()),
                activation: Some(ActivationId::new(2)),
                exec: Some(ExecState::Running),
            }],
            agent_sessions: vec![AgentSessionDescriptor {
                id: ResourceId::local(8),
                parent: Some(ResourceId::local(7)),
                provider: Some("claude".to_owned()),
                native_id: None,
                state: Some("working".to_owned()),
            }],
            focused_session: 1,
            focused_pane: ResourceId::local(7),
            authority: None,
            inbox: Some(session_key()),
            handles: HandleMap::new(),
        }
    }

    #[test]
    fn panes_carry_their_session_and_record_context() {
        let graph = session_graph(topology(), None);
        let pane = &graph.panes[0];
        assert_eq!(pane.terminal_id, "satellite:box:4");
        assert_eq!(pane.session_name, "work");
        assert_eq!(pane.session_key, Some(session_key().to_string()));
        assert_eq!((pane.cols, pane.rows), (80, 24));
        assert_eq!(pane.title.as_deref(), Some("vim"));
        assert_eq!(pane.name.as_deref(), Some("editor"));
        assert_eq!(pane.revision, Some(9));
        let activation = pane.activation.as_ref().unwrap();
        assert_eq!((activation.id, activation.state), (2, "running"));
        assert!(pane.group_id.is_none(), "no arrangement held");
        assert!(pane.is_focused);
    }

    #[test]
    fn the_focused_pane_is_a_string_identity() {
        assert_eq!(session_graph(topology(), None).focused_pane, "local:7");
    }

    #[test]
    fn sessions_keep_lifetime_keys_counts_and_intake() {
        let graph = session_graph(topology(), None);
        let session = &graph.sessions[0];
        assert_eq!(session.attached_client_count, 1);
        assert_eq!(session.member_count, 2);
        assert_eq!(session.lifetime, Some("persistent"));
        assert_eq!(session.revision, Some(5));
        assert!(session.awaiting_organization);
        assert!(session.inbox);
        assert!(session.focused);
        assert_eq!(graph.inbox, Some(session_key().to_string()));
    }

    #[test]
    fn agent_sessions_are_projected() {
        let graph = session_graph(topology(), None);
        assert_eq!(graph.agent_sessions.len(), 1);
        assert_eq!(graph.agent_sessions[0].id, "local:8");
        assert_eq!(graph.agent_sessions[0].parent.as_deref(), Some("local:7"));
        assert_eq!(graph.agent_sessions[0].provider.as_deref(), Some("claude"));
    }
}
