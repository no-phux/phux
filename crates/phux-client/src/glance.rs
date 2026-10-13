//! The human glance over one listing: what each session's terminals are
//! doing, and which of them need a person (`docs/consumers/attention.md`).
//!
//! Pure projection over data a listing already holds: the `GET_STATE`
//! snapshot, the organization records beside it, and the `phux.agent/v1`
//! records. Nothing here reaches the wire; `--json` documents stay in
//! [`crate::session_list`].

use std::collections::HashMap;

use phux_protocol::ids::{ResourceId, ResourceKey, ResourceKind};
use phux_protocol::wire::frame::{
    ExecState, Outcome, ResourceLifecycle, ResourceRecord, SessionLifetime,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

use crate::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use crate::org::OrgInventory;
use crate::resource;
use crate::selector::format_terminal_id;

/// How one terminal reads at a glance, most urgent first: the derived
/// `Ord` sorts a session's terminals exceptions first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Condition {
    /// Its agent is blocked on a person or asks for attention.
    Waiting,
    /// It exited with an error, or never started.
    Failed,
    /// A server restart lost track of it; it may still run.
    Lost,
    /// It runs.
    Running,
    /// It was never started (imported, or not yet launched).
    NotStarted,
    /// It ended cleanly, or was released.
    Done,
}

impl Condition {
    /// Whether a person has to act on it.
    #[must_use]
    pub const fn needs_you(self) -> bool {
        matches!(self, Self::Waiting | Self::Failed | Self::Lost)
    }
}

/// One terminal of a session, as the session and detail views print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberGlance {
    /// The live selector (`@N`), or `None` for a record with no live
    /// execution here.
    pub live: Option<ResourceId>,
    /// The organization key, when the server keeps records.
    pub key: Option<ResourceKey>,
    /// Where it is.
    pub condition: Condition,
    /// The state as words: `running`, `failed (exit 1)`, `lost`.
    pub status: String,
    /// The agent and its state (`claude blocked`), when one runs there.
    pub agent: Option<String>,
    /// The pane title.
    pub title: Option<String>,
    /// The working directory.
    pub cwd: Option<String>,
    /// Record revision and activation, for the detail view
    /// (`rev 4, run #2 completed`).
    pub record: Option<String>,
}

impl MemberGlance {
    /// The address a person types: the live selector, else nothing.
    #[must_use]
    pub fn selector(&self) -> String {
        self.live
            .as_ref()
            .map_or_else(|| "-".to_owned(), format_terminal_id)
    }
}

/// One session at a glance: counts per [`Condition`] and the few facts that
/// change what a person expects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionGlance {
    /// Session name.
    pub name: String,
    /// Terminals whose agent waits on a person.
    pub waiting: u32,
    /// Terminals that failed.
    pub failed: u32,
    /// Terminals a restart lost track of.
    pub lost: u32,
    /// Terminals running.
    pub running: u32,
    /// Terminals never started.
    pub not_started: u32,
    /// Terminals ended cleanly.
    pub done: u32,
    /// Whether the session is deleted with its last terminal.
    pub ephemeral: bool,
    /// Imported terminals awaiting organization, when this is the Inbox.
    pub to_organize: Option<u32>,
    /// Clients attached right now.
    pub attached_clients: u16,
}

impl SessionGlance {
    /// How many terminals need a person, the Inbox's work included.
    #[must_use]
    pub fn needs_you(&self) -> u32 {
        self.waiting
            .saturating_add(self.failed)
            .saturating_add(self.lost)
            .saturating_add(self.to_organize.unwrap_or(0))
    }

    /// Whether the session holds nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.waiting == 0
            && self.failed == 0
            && self.lost == 0
            && self.running == 0
            && self.not_started == 0
            && self.done == 0
    }

    const fn count(&mut self, condition: Condition) {
        let slot = match condition {
            Condition::Waiting => &mut self.waiting,
            Condition::Failed => &mut self.failed,
            Condition::Lost => &mut self.lost,
            Condition::Running => &mut self.running,
            Condition::NotStarted => &mut self.not_started,
            Condition::Done => &mut self.done,
        };
        *slot = slot.saturating_add(1);
    }
}

/// What a listing knows, borrowed together.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// The `GET_STATE` snapshot.
    pub snapshot: &'a SessionSnapshot,
    /// Organization records, from a server that keeps them.
    pub organization: Option<&'a OrgInventory>,
    /// Each terminal's `phux.agent/v1` record.
    pub agents: &'a HashMap<ResourceId, AgentRecord>,
}

/// Every session's glance, in snapshot order.
#[must_use]
pub fn sessions(sources: Sources<'_>) -> Vec<(SessionGlance, &SessionInfo)> {
    sources
        .snapshot
        .sessions
        .iter()
        .map(|session| (session_glance(sources, session), session))
        .collect()
}

/// One session's glance.
#[must_use]
pub fn session_glance(sources: Sources<'_>, session: &SessionInfo) -> SessionGlance {
    let members = members(sources, session);
    let mut glance = SessionGlance {
        name: session.name.clone(),
        attached_clients: session.attached_client_count,
        ..SessionGlance::default()
    };
    for member in &members {
        glance.count(member.condition);
    }
    let record = record_of(sources.organization, session);
    let lifetime = record.map(|r| r.lifetime).or(session.lifetime);
    glance.ephemeral = lifetime == Some(SessionLifetime::Ephemeral);
    let is_inbox = record.is_some_and(|r| {
        r.awaiting_organization || sources.organization.and_then(|o| o.inbox) == Some(r.key)
    });
    if is_inbox {
        glance.to_organize = Some(u32::try_from(members.len()).unwrap_or(u32::MAX));
    }
    glance
}

/// A glance over bare `terminals`, named `name`: all a hub can tell of a
/// satellite's session, since it holds no organization records for it.
#[must_use]
pub fn terminals_glance<'a>(
    sources: Sources<'_>,
    name: &str,
    terminals: impl IntoIterator<Item = &'a ResourceInfo>,
) -> SessionGlance {
    let mut glance = SessionGlance {
        name: name.to_owned(),
        ..SessionGlance::default()
    };
    for info in terminals {
        glance.count(from_snapshot(sources, info).condition);
    }
    glance
}

/// A session's terminals, exceptions first, then by selector.
#[must_use]
pub fn members(sources: Sources<'_>, session: &SessionInfo) -> Vec<MemberGlance> {
    let mut out: Vec<MemberGlance> = match (
        sources.organization,
        record_of(sources.organization, session),
    ) {
        (Some(org), Some(record)) => org
            .members(&record.key)
            .filter(|member| member.kind != ResourceKind::AgentSession)
            .map(|member| {
                let live = member
                    .live
                    .as_ref()
                    .and_then(|id| resource::find(sources.snapshot, id));
                from_record(sources, member, live)
            })
            .collect(),
        _ => resource::terminals_in(sources.snapshot, session.id)
            .map(|info| from_snapshot(sources, info))
            .collect(),
    };
    out.sort_by(|a, b| {
        a.condition
            .cmp(&b.condition)
            .then_with(|| live_order(a).cmp(&live_order(b)))
    });
    out
}

fn live_order(member: &MemberGlance) -> (bool, Option<u32>) {
    (
        member.live.is_none(),
        member.live.as_ref().and_then(ResourceId::local_id),
    )
}

fn record_of<'a>(
    organization: Option<&'a OrgInventory>,
    session: &SessionInfo,
) -> Option<&'a phux_protocol::wire::frame::SessionRecord> {
    let org = organization?;
    session
        .key
        .and_then(|key| org.session(&key))
        .or_else(|| org.session_by_live(session.id))
}

fn from_record(
    sources: Sources<'_>,
    record: &ResourceRecord,
    live: Option<&ResourceInfo>,
) -> MemberGlance {
    let (condition, status) = record.activation.as_ref().map_or_else(
        || (Condition::NotStarted, "not started".to_owned()),
        |activation| activation_condition(activation.state, activation.outcome),
    );
    let revision = record.revision.get();
    let run = record.activation.as_ref().map_or_else(
        || "never run".to_owned(),
        |activation| {
            let outcome = activation
                .outcome
                .map(|outcome| format!(", {}", outcome_words(outcome)))
                .unwrap_or_default();
            format!(
                "run #{} {}{outcome}",
                activation.id.get(),
                exec_word(activation.state)
            )
        },
    );
    let mut member = MemberGlance {
        live: record.live.clone(),
        key: Some(record.key),
        condition,
        status,
        agent: None,
        title: record.name.clone(),
        cwd: None,
        record: Some(format!("rev {revision}, {run}")),
    };
    if let Some(info) = live {
        enrich(sources, &mut member, info);
    }
    member
}

fn from_snapshot(sources: Sources<'_>, info: &ResourceInfo) -> MemberGlance {
    let (condition, status) = match (info.lifecycle, info.exit.as_ref()) {
        (ResourceLifecycle::Exited, Some(exit)) => {
            let outcome = match (exit.exit_status, exit.signal) {
                (Some(status), _) => Some(Outcome::Exited(status)),
                (None, Some(signal)) => Some(Outcome::Signaled(signal)),
                (None, None) => None,
            };
            activation_condition(ExecState::Completed, outcome)
        }
        (ResourceLifecycle::Exited, None) => (Condition::Done, "exited".to_owned()),
        _ => (Condition::Running, "running".to_owned()),
    };
    let mut member = MemberGlance {
        live: Some(info.id.clone()),
        key: None,
        condition,
        status,
        agent: None,
        title: None,
        cwd: None,
        record: None,
    };
    enrich(sources, &mut member, info);
    member
}

/// Title, directory, and agent from the live resource; a running terminal
/// whose agent waits on a person becomes [`Condition::Waiting`].
fn enrich(sources: Sources<'_>, member: &mut MemberGlance, info: &ResourceInfo) {
    if info.title.is_some() {
        member.title.clone_from(&info.title);
    }
    member.cwd.clone_from(&info.cwd);
    let record = sources.agents.get(&info.id);
    let child =
        resource::children_of(sources.snapshot, &info.id).find_map(|child| child.agent.as_ref());
    let (name, state, attention) = match (record, child) {
        (Some(record), _) => (
            record.name.clone(),
            record.state,
            record.effective_attention(),
        ),
        (None, Some(facet)) => {
            let state = AgentMetaState::from(facet.state.clone());
            let attention = AgentRecord {
                state,
                ..AgentRecord::default()
            }
            .effective_attention();
            (facet.provider.clone(), state, attention)
        }
        (None, None) => return,
    };
    member.agent = Some(format!("{name} {}", state.as_str()));
    if member.condition == Condition::Running && attention == AgentAttention::High {
        member.condition = Condition::Waiting;
        "waiting".clone_into(&mut member.status);
    }
}

/// Signals a person sends to stop work on purpose: hang-up, interrupt,
/// kill, terminate. Any other signal is a crash.
const DELIBERATE_SIGNALS: [i32; 4] = [1, 2, 9, 15];

fn activation_condition(state: ExecState, outcome: Option<Outcome>) -> (Condition, String) {
    match state {
        ExecState::Starting | ExecState::Running | ExecState::Stopping => {
            (Condition::Running, exec_word(state).to_owned())
        }
        ExecState::Failed => (Condition::Failed, "failed to start".to_owned()),
        ExecState::Lost => (Condition::Lost, "lost".to_owned()),
        ExecState::Abandoned => (Condition::Done, "abandoned".to_owned()),
        ExecState::Completed => match outcome {
            Some(Outcome::Exited(status)) if status != 0 => {
                (Condition::Failed, format!("failed (exit {status})"))
            }
            Some(Outcome::Signaled(signal)) if !DELIBERATE_SIGNALS.contains(&signal) => {
                (Condition::Failed, format!("failed (signal {signal})"))
            }
            _ => (Condition::Done, "done".to_owned()),
        },
    }
}

const fn exec_word(state: ExecState) -> &'static str {
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

fn outcome_words(outcome: Outcome) -> String {
    match outcome {
        Outcome::Exited(status) => format!("exit {status}"),
        Outcome::Signaled(signal) => format!("signal {signal}"),
        _ => "end unobserved".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exceptions_sort_before_quiet_work() {
        let mut all = vec![
            Condition::Done,
            Condition::Running,
            Condition::Lost,
            Condition::NotStarted,
            Condition::Waiting,
            Condition::Failed,
        ];
        all.sort();
        assert_eq!(
            all,
            [
                Condition::Waiting,
                Condition::Failed,
                Condition::Lost,
                Condition::Running,
                Condition::NotStarted,
                Condition::Done,
            ]
        );
        assert!(all[..3].iter().all(|c| c.needs_you()));
        assert!(!all[3..].iter().any(|c| c.needs_you()));
    }

    #[test]
    fn an_error_exit_or_a_crash_is_a_failure_and_a_kill_is_not() {
        let done = |outcome| activation_condition(ExecState::Completed, Some(outcome)).0;
        assert_eq!(done(Outcome::Exited(0)), Condition::Done);
        assert_eq!(done(Outcome::Exited(1)), Condition::Failed);
        assert_eq!(done(Outcome::Signaled(15)), Condition::Done);
        assert_eq!(done(Outcome::Signaled(11)), Condition::Failed);
        assert_eq!(done(Outcome::Unobserved), Condition::Done);
        assert_eq!(
            activation_condition(ExecState::Lost, None).0,
            Condition::Lost
        );
    }
}
