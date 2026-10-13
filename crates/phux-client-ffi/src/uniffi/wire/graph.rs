//! The session graph and organization records as Swift and Kotlin read
//! them: mirrors of [`crate::projection::topology`], lowered by `From`.
//! Keys are full base32 text and group identities 32 hex digits.

use crate::projection::{organization, topology};

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

impl From<topology::ActivationOutcome> for ActivationOutcome {
    fn from(value: topology::ActivationOutcome) -> Self {
        match value {
            topology::ActivationOutcome::Exited(code) => Self::Exited { code },
            topology::ActivationOutcome::Signaled(signal) => Self::Signaled { signal },
            topology::ActivationOutcome::Unobserved => Self::Unobserved,
            topology::ActivationOutcome::Other => Self::Other,
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

impl From<topology::Activation> for ActivationDescriptor {
    fn from(value: topology::Activation) -> Self {
        Self {
            id: value.id,
            state: value.state.to_owned(),
            outcome: value.outcome.map(Into::into),
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
    /// Whether this is the designated Inbox session.
    pub inbox: bool,
    pub focused: bool,
}

impl From<topology::Session> for SessionDescriptor {
    fn from(value: topology::Session) -> Self {
        Self {
            id: value.id,
            name: value.name,
            attached_client_count: value.attached_client_count,
            created_at_unix_secs: value.created_at_unix_secs,
            key: value.key,
            lifetime: value.lifetime.map(str::to_owned),
            revision: value.revision,
            member_count: value.member_count,
            arrangement: value.arrangement,
            awaiting_organization: value.awaiting_organization,
            inbox: value.inbox,
            focused: value.focused,
        }
    }
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
}

impl From<topology::Pane> for PaneDescriptor {
    fn from(value: topology::Pane) -> Self {
        Self {
            terminal_id: value.terminal_id,
            session_id: value.session_id,
            session_name: value.session_name,
            session_key: value.session_key,
            cols: value.cols,
            rows: value.rows,
            title: value.title,
            cwd: value.cwd,
            is_focused: value.is_focused,
            key: value.key,
            revision: value.revision,
            name: value.name,
            activation: value.activation.map(Into::into),
            group_id: value.group_id,
            group_index: value.group_index,
            group_name: value.group_name,
        }
    }
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

impl From<topology::AgentSession> for AgentSessionDescriptor {
    fn from(value: topology::AgentSession) -> Self {
        Self {
            id: value.id,
            parent: value.parent,
            provider: value.provider,
            native_id: value.native_id,
            state: value.state,
        }
    }
}

/// One resource record from the organization replica, live or not.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ResourceRecordDescriptor {
    pub key: String,
    pub session_key: String,
    pub name: Option<String>,
    /// `terminal`, `agent_session`, or `kind-<tag>`.
    pub kind: String,
    /// `remove` or `retain`.
    pub on_exit: String,
    pub activation: Option<ActivationDescriptor>,
    pub revision: u64,
    /// The live terminal id while it executes here.
    pub live: Option<String>,
}

impl From<topology::Record> for ResourceRecordDescriptor {
    fn from(value: topology::Record) -> Self {
        Self {
            key: value.key,
            session_key: value.session_key,
            name: value.name,
            kind: value.kind,
            on_exit: value.on_exit.to_owned(),
            activation: value.activation.map(Into::into),
            revision: value.revision,
            live: value.live,
        }
    }
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
    /// The designated Inbox session's key.
    pub inbox: Option<String>,
}

impl From<topology::SessionGraph> for SessionTopology {
    fn from(value: topology::SessionGraph) -> Self {
        Self {
            sessions: value.sessions.into_iter().map(Into::into).collect(),
            panes: value.panes.into_iter().map(Into::into).collect(),
            agent_sessions: value.agent_sessions.into_iter().map(Into::into).collect(),
            records: value.records.into_iter().map(Into::into).collect(),
            focused_session: value.focused_session,
            focused_pane: value.focused_pane,
            authority: value.authority,
            inbox: value.inbox,
        }
    }
}

/// One target of a kill-session workflow: the session first, then each
/// member in membership order.
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

impl From<organization::TargetResult> for OrgTargetResult {
    fn from(value: organization::TargetResult) -> Self {
        use phux_protocol::ids::{ResourceKey, SessionKey};
        let key = if value.is_session {
            SessionKey::new(value.key).map(|key| key.to_string())
        } else {
            ResourceKey::new(value.key).map(|key| key.to_string())
        };
        Self {
            key: key.unwrap_or_default(),
            is_session: value.is_session,
            status: value.status.word().to_owned(),
            refusal: value.refusal,
            message: value.message,
        }
    }
}

/// The owner-local Inbox: the designated Inbox session and every session
/// awaiting organization, with their member records.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct InboxDescriptor {
    pub inbox: Option<String>,
    /// Session keys, the designated one first.
    pub sessions: Vec<String>,
    pub records: Vec<ResourceRecordDescriptor>,
}
