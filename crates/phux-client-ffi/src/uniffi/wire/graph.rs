//! The session graph and organization records as Swift and Kotlin read
//! them.
//!
//! The runtime's [`Topology`] keeps sessions, panes, and agent sessions as
//! separate lists joined by id, and its organization replica holds the
//! records behind them. A phone paints a list, so each pane is denormalized
//! with its session and, when the session's arrangement is held, its group.
//! Terminal identities are in their [`id`] string form, keys full base32
//! text, and group identities 32 hex digits. Every field the runtime knows
//! reaches the binding, including records with no live handle.

use phux_client_core::layout::{self, ArrangementDoc};
use phux_client_runtime::control::{OrganizationReplica, Topology};
use phux_protocol::ResourceId;
use phux_protocol::ids::{Revision, SessionKey};
use phux_protocol::wire::frame::{ActivationInfo, Outcome, ResourceRecord};

use crate::projection::{id, topology};

/// How an activation ended.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// The process exited with this status.
    Exited { code: i32 },
    /// The process was ended by this signal.
    Signaled { signal: i32 },
    /// The server did not observe the end.
    Unobserved,
    /// A shape this build does not name.
    Other,
}

impl From<Outcome> for ActivationOutcome {
    fn from(value: Outcome) -> Self {
        match value {
            Outcome::Exited(code) => Self::Exited { code },
            Outcome::Signaled(signal) => Self::Signaled { signal },
            Outcome::Unobserved => Self::Unobserved,
            #[allow(
                unreachable_patterns,
                reason = "Outcome is non_exhaustive across crates"
            )]
            _ => Self::Other,
        }
    }
}

/// The current or most recent execution of a resource.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ActivationDescriptor {
    /// Monotonic per resource.
    pub id: u64,
    /// `starting`, `running`, `stopping`, `completed`, `failed`, `lost`, or
    /// `abandoned`.
    pub state: String,
    pub outcome: Option<ActivationOutcome>,
}

impl From<&ActivationInfo> for ActivationDescriptor {
    fn from(info: &ActivationInfo) -> Self {
        Self {
            id: info.id.get(),
            state: topology::exec_word(info.state).to_owned(),
            outcome: info.outcome.map(Into::into),
        }
    }
}

/// One session the server listed.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SessionDescriptor {
    pub id: u32,
    pub name: String,
    pub attached_client_count: u16,
    pub created_at_unix_secs: i64,
    /// The persistent key, when the server reports it.
    pub key: Option<String>,
    /// `persistent` or `ephemeral`, when the server reports it.
    pub lifetime: Option<String>,
    pub revision: Option<u64>,
    pub member_count: u32,
    /// The arrangement key, from the organization replica.
    pub arrangement: Option<String>,
    pub awaiting_organization: bool,
    /// Whether this is the designated Intake session (legacy binding field name).
    pub inbox: bool,
    pub focused: bool,
    /// The server's recency counter (higher is more recently used), when it reports one.
    pub touched: Option<u64>,
    /// The repository the session was created in, when it has one.
    pub project: Option<SessionProjectDescriptor>,
    /// The project the session was explicitly linked into. Group by this
    /// label when set, else by `project.id`.
    pub project_link: Option<String>,
}

/// The repository a session was created in.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SessionProjectDescriptor {
    /// Grouping key. Compare it; do not parse it.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The branch checked out when the session was created.
    pub branch: Option<String>,
    /// Whether the checkout is a linked worktree rather than the primary one.
    pub linked_worktree: bool,
}

/// One live terminal, denormalized with its session and, when the session's
/// arrangement is held, the group that shows it, so Swift needs no joins.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PaneDescriptor {
    /// Stable string form of the wire `ResourceId` (e.g. "local:7"); the key
    /// Swift hands back to target this pane.
    pub terminal_id: String,
    pub session_id: u32,
    pub session_name: String,
    pub session_key: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// Whether this pane is the attaching client's initial focus.
    pub is_focused: bool,
    /// The resource's persistent key.
    pub key: Option<String>,
    pub revision: Option<u64>,
    /// The resource's display name.
    pub name: Option<String>,
    pub activation: Option<ActivationDescriptor>,
    /// The arrangement group's identity, 32 hex digits.
    pub group_id: Option<String>,
    pub group_index: Option<u32>,
    pub group_name: Option<String>,
    /// The foreground program's name (`zsh`, `claude`), once read.
    #[uniffi(default)]
    pub foreground: Option<String>,
    /// What a list calls the pane: its name, else what runs in it and where
    /// (`claude · phux`), else its title or folder. One rule for every client.
    #[uniffi(default)]
    pub label: String,
}

/// One `AgentSession` resource bound to a terminal.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionDescriptor {
    pub id: String,
    pub parent: Option<String>,
    pub provider: Option<String>,
    pub native_id: Option<String>,
    pub state: Option<String>,
}

impl From<phux_client_runtime::control::AgentSessionDescriptor> for AgentSessionDescriptor {
    fn from(agent: phux_client_runtime::control::AgentSessionDescriptor) -> Self {
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
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ResourceRecordDescriptor {
    pub key: String,
    pub session_key: String,
    pub name: Option<String>,
    /// `terminal`, `agent_session`, `accessibility_tree`, `pixel_surface`,
    /// `document`, or `kind-<tag>`.
    pub kind: String,
    /// `remove` or `retain`.
    pub on_exit: String,
    pub activation: Option<ActivationDescriptor>,
    pub revision: u64,
    /// The live terminal id while it executes here.
    pub live: Option<String>,
    /// Whether a server stop ended it, so restore relaunches it.
    #[uniffi(default)]
    pub ended_by_restart: bool,
}

impl From<&ResourceRecord> for ResourceRecordDescriptor {
    fn from(record: &ResourceRecord) -> Self {
        Self {
            key: record.key.to_string(),
            session_key: record.session.to_string(),
            name: record.name.clone(),
            kind: topology::kind_word(record.kind),
            on_exit: topology::exit_policy_word(record.on_exit).to_owned(),
            activation: record.activation.as_ref().map(Into::into),
            revision: record.revision.get(),
            live: record.live.as_ref().map(id::encode),
            ended_by_restart: record.ended_by_restart(),
        }
    }
}

/// Every resource record the organization replica holds, in record order;
/// empty until it names an authority.
pub(crate) fn records(replica: &OrganizationReplica) -> Vec<ResourceRecordDescriptor> {
    if replica.authority().is_none() {
        return Vec::new();
    }
    replica.resources().iter().map(Into::into).collect()
}

/// The session graph: sessions, live panes, agent sessions, and every
/// resource record the organization replica holds.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SessionTopology {
    pub sessions: Vec<SessionDescriptor>,
    pub panes: Vec<PaneDescriptor>,
    pub agent_sessions: Vec<AgentSessionDescriptor>,
    /// Empty without `ORGANIZATION`.
    pub records: Vec<ResourceRecordDescriptor>,
    pub focused_session: u32,
    /// String form of the attaching client's initial focused pane.
    pub focused_pane: String,
    pub authority: Option<String>,
    /// The designated Intake session's key (legacy binding field name).
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

fn session(
    session: phux_client_runtime::control::SessionDescriptor,
    intake: Option<SessionKey>,
    focused_session: u32,
) -> SessionDescriptor {
    SessionDescriptor {
        id: session.id,
        name: session.name,
        attached_client_count: session.attached_client_count,
        created_at_unix_secs: session.created_at_unix_secs,
        key: session.key.map(|key| key.to_string()),
        lifetime: session
            .lifetime
            .map(|lifetime| topology::lifetime_word(lifetime).to_owned()),
        revision: session.revision.map(Revision::get),
        member_count: session.members,
        arrangement: session.arrangement.map(|key| key.to_string()),
        awaiting_organization: session.awaiting_organization,
        inbox: session.key.is_some() && session.key == intake,
        focused: session.id == focused_session,
        touched: session.touched,
        project: session.project.map(|project| SessionProjectDescriptor {
            id: project.id,
            name: project.name,
            branch: project.branch,
            linked_worktree: project.linked_worktree,
        }),
        project_link: session.project_link,
    }
}

/// Flatten one runtime topology snapshot, with the organization replica's
/// records and arrangements when the binding has them.
pub(crate) fn session_topology(
    topology: Topology,
    organization: Option<&OrganizationReplica>,
) -> SessionTopology {
    let intake = topology.intake();
    let focused_session = topology.focused_session;
    let arrangement_of = |key: Option<SessionKey>| -> Option<ArrangementDoc> {
        let (replica, key) = (organization?, key?);
        replica
            .arrangement_document(&key)
            .map(|(_, _, document, _)| document)
    };
    let docs: Vec<(u32, Option<SessionKey>, Option<ArrangementDoc>)> = topology
        .sessions
        .iter()
        .map(|session| (session.id, session.key, arrangement_of(session.key)))
        .collect();
    let sessions = topology
        .sessions
        .into_iter()
        .map(|descriptor| session(descriptor, intake, focused_session))
        .collect();
    let panes = topology
        .panes
        .into_iter()
        .map(|pane| {
            let held = docs.iter().find(|(id, _, _)| *id == pane.session_id);
            let session_key = held.and_then(|(_, key, _)| *key);
            let placed = held
                .and_then(|(_, _, doc)| doc.as_ref())
                .and_then(|doc| placement(doc, &pane.terminal_id));
            let activation = organization
                .and_then(|replica| replica.resource_by_live(&pane.terminal_id))
                .and_then(|record| record.activation.as_ref())
                .map(Into::into)
                .or_else(|| {
                    pane.activation
                        .zip(pane.exec)
                        .map(|(id, state)| ActivationDescriptor {
                            id: id.get(),
                            state: topology::exec_word(state).to_owned(),
                            outcome: None,
                        })
                });
            let label = pane.label();
            PaneDescriptor {
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
                group_id: placed.as_ref().map(|p| topology::group_id_text(&p.id)),
                group_index: placed.as_ref().map(|p| p.index),
                group_name: placed.map(|p| p.name),
                foreground: pane.foreground,
                label,
            }
        })
        .collect();
    SessionTopology {
        sessions,
        panes,
        agent_sessions: topology
            .agent_sessions
            .into_iter()
            .map(Into::into)
            .collect(),
        records: organization.map(records).unwrap_or_default(),
        focused_session,
        focused_pane: id::encode(&topology.focused_pane),
        authority: topology.authority.map(|key| key.to_string()),
        inbox: intake.map(|key| key.to_string()),
    }
}

/// One target of an organization result.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct OrgTargetResult {
    pub key: String,
    pub is_session: bool,
    /// `done`, `already_gone`, `refused`, `unknown`, `timed_out`,
    /// `no_live_target`, `kept`, `not_sent`, or `pending`.
    pub status: String,
    /// The refusal code (`0` unless refused).
    pub refusal: u32,
    pub message: String,
}

/// The owner-local Intake: the designated Intake session and every session
/// awaiting organization, with their member records.
/// The legacy record and field names preserve existing generated bindings.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct InboxDescriptor {
    pub inbox: Option<String>,
    /// Session keys, the designated one first.
    pub sessions: Vec<String>,
    pub records: Vec<ResourceRecordDescriptor>,
}

/// Preferred Rust spelling; generated bindings retain the legacy record name.
pub type IntakeDescriptor = InboxDescriptor;

#[cfg(test)]
mod tests {
    use super::session_topology;
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
                attached_client_count: 1,
                keep_empty: true,
                created_at_unix_secs: 42,
                key: Some(session_key()),
                touched: None,
                lifetime: Some(SessionLifetime::Persistent),
                revision: Some(Revision::new(5)),
                members: 2,
                arrangement: None,
                awaiting_organization: true,
                project: None,
                project_link: None,
            }],
            resources: Vec::new(),
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
                foreground: None,
                icon: None,
                parent: None,
                unseen: None,
            }],
            agent_sessions: vec![AgentSessionDescriptor {
                id: ResourceId::local(8),
                parent: Some(ResourceId::local(7)),
                provider: Some("claude".to_owned()),
                native_id: None,
                state: Some("working".to_owned()),
            }],
            documents: Vec::new(),
            satellite_sessions: Vec::new(),
            focused_session: 1,
            focused_pane: ResourceId::local(7),
            authority: None,
            inbox: Some(session_key()),
            handles: HandleMap::new(),
        }
    }

    #[test]
    fn panes_carry_their_session_and_record_context() {
        let graph = session_topology(topology(), None);
        let pane = &graph.panes[0];
        assert_eq!(pane.session_key, Some(session_key().to_string()));
        let activation = pane.activation.as_ref().unwrap();
        assert_eq!((activation.id, activation.state.as_str()), (2, "running"));
        assert!(pane.group_id.is_none(), "no arrangement held");
    }

    #[test]
    fn sessions_keep_lifetime_keys_counts_and_intake() {
        let graph = session_topology(topology(), None);
        let session = &graph.sessions[0];
        assert_eq!(session.attached_client_count, 1);
        assert_eq!(session.member_count, 2);
        assert_eq!(session.lifetime.as_deref(), Some("persistent"));
        assert_eq!(session.revision, Some(5));
        assert!(session.awaiting_organization);
        assert!(session.inbox);
        assert!(session.focused);
        assert_eq!(graph.inbox, Some(session_key().to_string()));
    }
}
