//! The one notification feed. Every message a window shows lands here.
//!
//! The list behind the titlebar bell is its home. Its Inbox holds agent
//! attention and server-held approvals (ADR-0128); an approval entry stands
//! exactly while the server lists the approval, and is decided in place.
//! Agent, approval, command, and error entries also
//! interrupt: the newest one peeks as a single card, and is offered to the
//! system notification center while Phux is not the frontmost app. A refusal
//! of a command the user just ran peeks briefly and never enters the list.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use phux_protocol::ResourceId;
use phux_protocol::ids::ApprovalId;

use crate::connection::{AgentAttention as AttentionLevel, AgentState};

/// Entries kept, oldest dropped first. A pending approval is never dropped:
/// it stands exactly while the server lists it.
const LIMIT: usize = 100;
/// How long the peek card stays up.
const PEEK: Duration = Duration::from_secs(10);
/// How long a refusal's card stays up.
const REFUSAL_PEEK: Duration = Duration::from_secs(4);
/// A shell command at least this long that finishes in a pane the person is
/// not looking at asks for attention. Shorter ones finish unremarked.
pub(crate) const COMMAND_ATTENTION: Duration = Duration::from_secs(5);

/// Shell commands in flight, by terminal, from OSC 133 C (start) and D
/// (finish). A finish alerts only for a start this window saw.
#[derive(Default)]
pub(crate) struct Commands {
    started: HashMap<ResourceId, Instant>,
}

impl Commands {
    pub fn started(&mut self, terminal: ResourceId, now: Instant) {
        self.started.insert(terminal, now);
    }

    /// How long the finished command ran, when its finish should alert: it
    /// ran at least [`COMMAND_ATTENTION`] in a pane that is not focused.
    pub fn finished(
        &mut self,
        terminal: &ResourceId,
        now: Instant,
        focused: bool,
    ) -> Option<Duration> {
        let started = self.started.remove(terminal)?;
        let ran = now.saturating_duration_since(started);
        (!focused && ran >= COMMAND_ATTENTION).then_some(ran)
    }

    pub fn forget(&mut self, terminal: &ResourceId) {
        self.started.remove(terminal);
    }

    pub fn clear(&mut self) {
        self.started.clear();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentAttention {
    pub state: AgentState,
    pub attention: AttentionLevel,
}

impl AgentAttention {
    pub fn actionable(&self) -> bool {
        matches!(self.state, AgentState::Done | AgentState::Blocked)
            || self.attention == AttentionLevel::High
    }

    pub fn should_alert(&self, previous: Option<&Self>, focused: bool) -> bool {
        if focused || !self.actionable() {
            return false;
        }
        let Some(previous) = previous else {
            return true;
        };
        self.state != previous.state
            || (self.attention == AttentionLevel::High
                && previous.attention != AttentionLevel::High)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Info,
    Refusal,
    Error,
    Agent,
    /// A shell command finished in a pane nobody was watching.
    Command,
    /// A server-held action awaiting this person's decision.
    Approval,
}

impl Kind {
    /// Agent, approval, command, and error entries peek, count as unread, and
    /// reach the system; info waits in the list.
    pub fn interrupts(self) -> bool {
        matches!(
            self,
            Self::Agent | Self::Approval | Self::Command | Self::Error
        )
    }

    /// Inbox entries: something waits on the person, not a report.
    pub fn inbox(self) -> bool {
        matches!(self, Self::Agent | Self::Approval)
    }

    fn peeks(self) -> bool {
        self.interrupts() || self == Self::Refusal
    }

    fn lifetime(self) -> Duration {
        if self == Self::Refusal {
            REFUSAL_PEEK
        } else {
            PEEK
        }
    }
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub kind: Kind,
    pub title: String,
    pub body: Option<String>,
    pub at: Instant,
    /// The terminal a click reveals.
    pub terminal: Option<ResourceId>,
    /// The agent state an agent entry reports, for its colour. The entry
    /// stands only while its agent still reports this state.
    pub agent_state: Option<AgentState>,
    /// The held action an approval entry decides.
    pub approval: Option<ApprovalId>,
    pub read: bool,
}

/// Where a system notification's click goes back to: an entry of one window,
/// or a server-held approval. An approval's tag names only the approval, so
/// every window on its server offers the same notification and the system
/// replaces it instead of stacking one per window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemTag {
    Entry { desk: u64, entry: u64 },
    Approval(ApprovalId),
}

impl SystemTag {
    const APPROVAL: &'static str = "approval/";

    pub(crate) fn of(desk: u64, entry: &Notification) -> Self {
        match entry.approval {
            Some(id) => Self::Approval(id),
            None => Self::Entry {
                desk,
                entry: entry.id,
            },
        }
    }

    pub(crate) fn parse(tag: &str) -> Option<Self> {
        if let Some(id) = tag.strip_prefix(Self::APPROVAL) {
            return ApprovalId::parse(id).map(Self::Approval);
        }
        let (desk, entry) = tag.split_once('/')?;
        Some(Self::Entry {
            desk: desk.parse().ok()?,
            entry: entry.parse().ok()?,
        })
    }
}

impl std::fmt::Display for SystemTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Entry { desk, entry } => write!(f, "{desk}/{entry}"),
            Self::Approval(id) => write!(f, "{}{id}", Self::APPROVAL),
        }
    }
}

#[derive(Default)]
pub struct Feed {
    /// Oldest first.
    items: Vec<Notification>,
    next: u64,
    peek: Option<u64>,
    /// Interrupting entries not yet offered to the system.
    outbound: Vec<u64>,
}

impl Feed {
    pub fn post(&mut self, kind: Kind, title: &str, body: Option<String>) -> u64 {
        self.push(kind, title.to_owned(), body, None, None)
    }

    /// One entry per held action; posting it again changes nothing.
    pub fn post_approval(&mut self, title: String, body: String, id: ApprovalId) -> u64 {
        if let Some(entry) = self.items.iter().find(|entry| entry.approval == Some(id)) {
            return entry.id;
        }
        let entry = self.push(Kind::Approval, title, Some(body), None, None);
        if let Some(posted) = self.items.iter_mut().find(|item| item.id == entry) {
            posted.approval = Some(id);
        }
        entry
    }

    /// One entry per agent: a newer state replaces the older one.
    pub fn post_agent(
        &mut self,
        title: String,
        body: String,
        terminal: ResourceId,
        state: AgentState,
    ) -> u64 {
        self.retain(|entry| {
            entry.kind != Kind::Agent || entry.terminal.as_ref() != Some(&terminal)
        });
        self.push(Kind::Agent, title, Some(body), Some(terminal), Some(state))
    }

    /// One entry per terminal: a newer finish replaces the older one.
    pub fn post_command(&mut self, title: String, body: String, terminal: ResourceId) -> u64 {
        self.retain(|entry| {
            entry.kind != Kind::Command || entry.terminal.as_ref() != Some(&terminal)
        });
        self.push(Kind::Command, title, Some(body), Some(terminal), None)
    }

    fn push(
        &mut self,
        kind: Kind,
        title: String,
        body: Option<String>,
        terminal: Option<ResourceId>,
        agent_state: Option<AgentState>,
    ) -> u64 {
        self.next += 1;
        let id = self.next;
        self.items.push(Notification {
            id,
            kind,
            title,
            body,
            at: Instant::now(),
            terminal,
            agent_state,
            approval: None,
            read: !kind.interrupts(),
        });
        if kind.peeks() {
            self.unpeek();
            self.peek = Some(id);
        }
        if kind.interrupts() {
            self.outbound.push(id);
        }
        self.evict();
        id
    }

    /// Drop the oldest entries past [`LIMIT`], sparing approvals.
    fn evict(&mut self) {
        let mut excess = self.items.len().saturating_sub(LIMIT);
        if excess == 0 {
            return;
        }
        self.items.retain(|entry| {
            if excess == 0 || entry.kind == Kind::Approval {
                return true;
            }
            excess -= 1;
            false
        });
        self.settle();
    }

    /// Newest first, without refusals.
    pub fn items(&self) -> impl Iterator<Item = &Notification> {
        self.items
            .iter()
            .rev()
            .filter(|entry| entry.kind != Kind::Refusal)
    }

    pub fn get(&self, id: u64) -> Option<&Notification> {
        self.items.iter().find(|entry| entry.id == id)
    }

    /// The entry that decides approval `id`, if this feed holds one.
    pub(crate) fn approval_entry(&self, id: ApprovalId) -> Option<u64> {
        self.items
            .iter()
            .find(|entry| entry.approval == Some(id))
            .map(|entry| entry.id)
    }

    pub fn is_empty(&self) -> bool {
        self.items().next().is_none()
    }

    pub fn peek(&self) -> Option<&Notification> {
        self.peek.and_then(|id| self.get(id))
    }

    /// When the peek card expires.
    pub fn peek_expires(&self) -> Option<Instant> {
        self.peek().map(|entry| entry.at + entry.kind.lifetime())
    }

    /// Unread interrupting entries; the bell's count.
    pub fn unread(&self) -> impl Iterator<Item = &Notification> {
        self.items.iter().filter(|entry| !entry.read)
    }

    pub fn read(&mut self, id: u64) {
        if let Some(entry) = self.items.iter_mut().find(|entry| entry.id == id) {
            entry.read = true;
        }
        if self.peek == Some(id) {
            self.unpeek();
        }
    }

    pub fn read_all(&mut self) {
        for entry in &mut self.items {
            entry.read = true;
        }
        self.unpeek();
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.settle();
    }

    pub fn retain(&mut self, keep: impl Fn(&Notification) -> bool) {
        self.items.retain(|entry| keep(entry));
        self.settle();
    }

    /// Drop a peek older than its lifetime. True when it changed.
    pub fn expire_peek(&mut self, now: Instant) -> bool {
        let stale = self
            .peek()
            .is_some_and(|entry| now.duration_since(entry.at) >= entry.kind.lifetime());
        if stale {
            self.unpeek();
        }
        stale
    }

    /// Take down the peek card; a refusal goes with it.
    fn unpeek(&mut self) {
        if let Some(id) = self.peek.take() {
            self.items
                .retain(|entry| entry.id != id || entry.kind != Kind::Refusal);
        }
    }

    /// Interrupting entries posted since the last take that still stand.
    pub fn take_outbound(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.outbound)
            .into_iter()
            .filter_map(|id| self.get(id).cloned())
            .collect()
    }

    /// Forget a peek or outbound id whose entry is gone.
    fn settle(&mut self) {
        let items = &self.items;
        let live = |id: &u64| items.iter().any(|entry| entry.id == *id);
        if !self.peek.as_ref().is_some_and(live) {
            self.peek = None;
        }
        self.outbound.retain(live);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attention(state: AgentState, level: AttentionLevel) -> AgentAttention {
        AgentAttention {
            state,
            attention: level,
        }
    }

    #[test]
    fn initially_actionable_agents_alert_but_quiet_or_focused_agents_do_not() {
        assert!(attention(AgentState::Blocked, AttentionLevel::Normal).should_alert(None, false));
        assert!(attention(AgentState::Done, AttentionLevel::Normal).should_alert(None, false));
        assert!(attention(AgentState::Working, AttentionLevel::High).should_alert(None, false));
        assert!(!attention(AgentState::Working, AttentionLevel::Normal).should_alert(None, false));
        assert!(!attention(AgentState::Blocked, AttentionLevel::Normal).should_alert(None, true));
    }

    #[test]
    fn attention_escalation_alerts_without_a_state_change_and_deduplicates() {
        let previous = attention(AgentState::Working, AttentionLevel::Normal);
        let escalated = attention(AgentState::Working, AttentionLevel::High);
        assert!(escalated.should_alert(Some(&previous), false));
        assert!(!escalated.should_alert(Some(&escalated), false));
        assert!(!previous.should_alert(Some(&escalated), false));
        assert!(!previous.actionable());
    }

    #[test]
    fn attention_downgrade_does_not_repeat_an_actionable_state() {
        let previous = attention(AgentState::Blocked, AttentionLevel::High);
        assert!(
            !attention(AgentState::Blocked, AttentionLevel::Normal)
                .should_alert(Some(&previous), false)
        );
        assert!(
            attention(AgentState::Done, AttentionLevel::Normal)
                .should_alert(Some(&previous), false)
        );
    }

    #[test]
    fn only_interrupting_entries_peek_and_count() {
        let mut feed = Feed::default();
        feed.post(Kind::Info, "zsh ended", None);
        assert!(feed.peek().is_none());
        assert_eq!(feed.unread().count(), 0);
        let error = feed.post(Kind::Error, "Detached", Some("gone".into()));
        assert_eq!(feed.peek().map(|entry| entry.id), Some(error));
        assert_eq!(feed.unread().count(), 1);
        assert_eq!(feed.items().count(), 2);
        assert_eq!(
            feed.take_outbound()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![error]
        );
        assert!(feed.take_outbound().is_empty());
    }

    #[test]
    fn a_newer_agent_state_replaces_the_older_entry() {
        let mut feed = Feed::default();
        let terminal = ResourceId::local(7);
        feed.post_agent(
            "a needs you".into(),
            "b".into(),
            terminal.clone(),
            AgentState::Blocked,
        );
        let done = feed.post_agent("a finished".into(), "b".into(), terminal, AgentState::Done);
        assert_eq!(
            feed.items().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![done]
        );
        assert_eq!(feed.take_outbound().len(), 1);
    }

    #[test]
    fn a_long_command_finishing_in_the_background_alerts() {
        let mut commands = Commands::default();
        let terminal = ResourceId::local(7);
        let start = Instant::now();
        commands.started(terminal.clone(), start);
        assert_eq!(
            commands.finished(&terminal, start + COMMAND_ATTENTION, false),
            Some(COMMAND_ATTENTION)
        );
        assert_eq!(
            commands.finished(&terminal, start + COMMAND_ATTENTION, false),
            None,
            "one start alerts once"
        );
    }

    #[test]
    fn a_focused_short_or_unseen_command_does_not_alert() {
        let mut commands = Commands::default();
        let terminal = ResourceId::local(7);
        let start = Instant::now();
        commands.started(terminal.clone(), start);
        assert_eq!(
            commands.finished(&terminal, start + COMMAND_ATTENTION, true),
            None
        );
        commands.started(terminal.clone(), start);
        assert_eq!(
            commands.finished(&terminal, start + Duration::from_secs(1), false),
            None
        );
        assert_eq!(
            commands.finished(&terminal, start + COMMAND_ATTENTION, false),
            None,
            "a finish without a start this window saw"
        );
        commands.started(terminal.clone(), start);
        commands.forget(&terminal);
        assert_eq!(
            commands.finished(&terminal, start + COMMAND_ATTENTION, false),
            None
        );
    }

    #[test]
    fn a_command_entry_interrupts_and_replaces_its_terminals_older_one() {
        let mut feed = Feed::default();
        let terminal = ResourceId::local(7);
        feed.post_command("make finished".into(), "a".into(), terminal.clone());
        let newer = feed.post_command("make finished".into(), "b".into(), terminal);
        assert_eq!(
            feed.items().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![newer]
        );
        assert_eq!(feed.peek().map(|entry| entry.id), Some(newer));
        assert_eq!(feed.unread().count(), 1);
        assert_eq!(feed.take_outbound().len(), 1);
    }

    #[test]
    fn an_approval_is_one_interrupting_inbox_entry() {
        let mut feed = Feed::default();
        let id = ApprovalId::new([7; 16]).unwrap();
        let first = feed.post_approval("Approve kill?".into(), "agent".into(), id);
        assert_eq!(
            feed.post_approval("again".into(), "agent".into(), id),
            first
        );
        assert_eq!(feed.items().count(), 1);
        assert_eq!(feed.peek().map(|entry| entry.id), Some(first));
        assert_eq!(feed.unread().count(), 1);
        assert!(Kind::Approval.inbox() && Kind::Agent.inbox() && !Kind::Error.inbox());
        assert_eq!(feed.get(first).and_then(|entry| entry.approval), Some(id));
    }

    #[test]
    fn an_approval_tag_names_the_approval_not_the_window() {
        let mut feed = Feed::default();
        let id = ApprovalId::new([7; 16]).unwrap();
        let entry = feed.post_approval("Approve kill?".into(), "agent".into(), id);
        let error = feed.post(Kind::Error, "Detached", None);
        let approval = feed.get(entry).cloned().unwrap();
        let tag = SystemTag::of(4, &approval);
        assert_eq!(tag, SystemTag::of(9, &approval));
        assert_eq!(SystemTag::parse(&tag.to_string()), Some(tag));
        assert_eq!(feed.approval_entry(id), Some(entry));
        let other = SystemTag::of(4, feed.get(error).unwrap());
        assert_eq!(other.to_string(), format!("4/{error}"));
        assert_eq!(SystemTag::parse(&other.to_string()), Some(other));
        assert_eq!(SystemTag::parse("approval/zz"), None);
        assert_eq!(SystemTag::parse("4"), None);
    }

    #[test]
    fn reading_clears_the_peek_and_the_count() {
        let mut feed = Feed::default();
        let first = feed.post(Kind::Error, "one", None);
        feed.post(Kind::Error, "two", None);
        feed.read(first);
        assert_eq!(feed.unread().count(), 1);
        assert!(feed.peek().is_some());
        feed.read_all();
        assert_eq!(feed.unread().count(), 0);
        assert!(feed.peek().is_none());
        assert_eq!(feed.items().count(), 2);
    }

    #[test]
    fn removed_entries_leave_no_peek_or_outbound() {
        let mut feed = Feed::default();
        feed.post(Kind::Error, "one", None);
        feed.retain(|_| false);
        assert!(feed.peek().is_none());
        assert!(feed.take_outbound().is_empty());
    }

    #[test]
    fn the_peek_expires_and_the_entry_stays() {
        let mut feed = Feed::default();
        feed.post(Kind::Error, "one", None);
        assert!(!feed.expire_peek(Instant::now()));
        assert!(feed.expire_peek(Instant::now() + PEEK));
        assert!(feed.peek().is_none());
        assert_eq!(feed.unread().count(), 1);
    }

    #[test]
    fn a_refusal_peeks_briefly_and_leaves_nothing_behind() {
        let mut feed = Feed::default();
        let refusal = feed.post(Kind::Refusal, "Nothing selected", None);
        assert_eq!(feed.peek().map(|entry| entry.id), Some(refusal));
        assert_eq!(feed.unread().count(), 0);
        assert!(feed.is_empty());
        assert!(feed.take_outbound().is_empty());
        assert!(feed.expire_peek(Instant::now() + REFUSAL_PEEK));
        assert!(feed.peek().is_none());
        assert!(feed.get(refusal).is_none());
    }

    #[test]
    fn a_pending_approval_outlasts_the_limit() {
        let mut feed = Feed::default();
        let id = ApprovalId::new([7; 16]).unwrap();
        let approval = feed.post_approval("Approve kill?".into(), "agent".into(), id);
        for index in 0..LIMIT + 5 {
            feed.post(Kind::Info, &index.to_string(), None);
        }
        assert_eq!(feed.items().count(), LIMIT);
        assert_eq!(
            feed.get(approval).and_then(|entry| entry.approval),
            Some(id)
        );
        assert_eq!(
            feed.post_approval("again".into(), "agent".into(), id),
            approval
        );
        assert_eq!(
            feed.items()
                .filter(|entry| entry.kind == Kind::Info)
                .last()
                .map(|entry| entry.title.as_str()),
            Some("6")
        );
    }

    #[test]
    fn the_oldest_entries_fall_off() {
        let mut feed = Feed::default();
        for index in 0..LIMIT + 5 {
            feed.post(Kind::Info, &index.to_string(), None);
        }
        assert_eq!(feed.items().count(), LIMIT);
        assert_eq!(
            feed.items().last().map(|entry| entry.title.as_str()),
            Some("5")
        );
    }
}
