//! What each terminal is doing, from the runtime's events: its declared
//! agent and state, a running shell command, how the last one ended, and
//! a bell. The sidebar and palette draw from it; ⌘J jumps to whatever
//! needs a person.
//!
//! The attention rules match the GPUI desktop's: a blocked agent, one that
//! finished, or one declaring high attention needs you; the shown terminal
//! never does, since you are looking at it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use phux_client_core::agent_record::{AgentAttention, AgentState, parse_agent_record};
use phux_client_runtime::control::Event;
use phux_protocol::ids::ResourceId;

/// A shell command this long that finishes off screen is worth a mark.
const LONG_COMMAND: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default)]
pub struct Activity {
    /// The declared agent's name and state, with its attention.
    pub agent: Option<Agent>,
    /// The agent is waiting on a question.
    pub asked: bool,
    /// The question, when the stream announced it; a gap can leave `asked`
    /// set with nothing to show.
    pub ask: Option<Ask>,
    /// When the shell command now running started (OSC 133).
    pub command_since: Option<Instant>,
    /// A long command finished off screen with this exit code; cleared once seen.
    pub finished: Option<Option<i32>>,
    /// Rang its bell off screen; cleared once seen.
    pub bell: bool,
    /// An agent finished off screen; cleared once seen.
    pub done_unseen: bool,
    /// When the mark last changed, for ranking what needs you.
    pub changed_at: Option<Instant>,
}

/// A question an agent is waiting on.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub id: String,
    pub text: String,
    pub suggestions: Vec<String>,
    /// When it started waiting, when known.
    pub since: Option<Instant>,
}

/// How ACP question ids start (`phux-agent-host`): the agent runs behind the
/// protocol, not at a shell, and the terminal refuses typing.
const ACP_QUESTION: &str = "acp-question:";

impl Ask {
    /// Whether typing into the terminal can answer it.
    pub fn typed(&self) -> bool {
        !self.id.starts_with(ACP_QUESTION)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub name: String,
    /// Its kind slug, when declared.
    pub kind: Option<String>,
    pub state: AgentState,
    pub attention: AgentAttention,
}

/// The one mark a row shows, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mark {
    /// An agent is blocked or asking.
    Blocked,
    /// An agent finished while you were elsewhere.
    Done,
    /// A command failed off screen.
    Failed,
    /// The bell rang off screen.
    Bell,
    /// An agent is working.
    Working,
    /// A shell command is running.
    Running,
    /// A command succeeded off screen.
    Succeeded,
    /// An agent is idle.
    Idle,
    None,
}

impl Mark {
    /// Whether this mark asks for a person.
    pub const fn needs_you(self) -> bool {
        matches!(self, Self::Blocked | Self::Done | Self::Failed | Self::Bell)
    }

    /// Whether something is in progress. Drawn still: anything that moves
    /// here would keep the window drawing for as long as an agent works.
    pub const fn live(self) -> bool {
        matches!(self, Self::Working | Self::Running)
    }

    pub const fn word(self) -> &'static str {
        match self {
            Self::Blocked => "needs you",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Bell => "bell",
            Self::Working => "working",
            Self::Running => "running",
            Self::Succeeded => "finished",
            Self::Idle => "idle",
            Self::None => "",
        }
    }
}

/// What a row shows of a terminal: an event that leaves this alone changes
/// nothing on screen, so it asks for no frame.
type Face = (Mark, Option<Agent>, Option<Ask>);

/// The terminal an event is about, when it is one this folds.
const fn subject(event: &Event) -> Option<&ResourceId> {
    match event {
        Event::AgentMetadata { terminal_id, .. }
        | Event::AgentAskedState { terminal_id, .. }
        | Event::AgentAsked { terminal_id, .. }
        | Event::CommandStarted { terminal_id }
        | Event::CommandFinished { terminal_id, .. }
        | Event::Bell { terminal_id }
        | Event::TerminalClosed { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

fn declared(value: Option<&[u8]>) -> Option<Agent> {
    value
        .and_then(parse_agent_record)
        .filter(|r| !r.name.is_empty())
        .map(|r| {
            let attention = r.effective_attention().0;
            Agent {
                name: r.name,
                kind: r.kind,
                state: r.state,
                attention,
            }
        })
}

impl Activity {
    fn declare(&mut self, agent: Option<Agent>, on_screen: bool) {
        let was = self.agent.as_ref().map(|a| a.state);
        let finished = agent.as_ref().is_some_and(|a| a.state == AgentState::Done);
        if finished && was != Some(AgentState::Done) && !on_screen {
            self.done_unseen = true;
        }
        if !finished {
            self.done_unseen = false;
        }
        self.agent = agent;
    }

    /// A question announced again keeps the wait it already had.
    fn asking(&mut self, ask: Ask) {
        let same = self.ask.as_ref().filter(|old| old.id == ask.id);
        let since = same.map_or(ask.since, |old| old.since);
        self.asked = true;
        self.ask = Some(Ask { since, ..ask });
    }

    fn command_ended(&mut self, exit_code: Option<i32>, on_screen: bool) {
        let long = self
            .command_since
            .take()
            .is_some_and(|since| since.elapsed() >= LONG_COMMAND);
        if long && !on_screen {
            self.finished = Some(exit_code);
        }
    }

    fn fold(&mut self, event: &Event, on_screen: bool) {
        match event {
            Event::AgentMetadata { value, .. } => {
                self.declare(declared(value.as_deref()), on_screen);
            }
            Event::AgentAskedState { asked, .. } => {
                self.asked = *asked;
                if !*asked {
                    self.ask = None;
                }
            }
            Event::AgentAsked {
                question_id,
                text,
                suggestions,
                waiting_seconds,
                ..
            } => {
                let waited = Duration::from_secs(waiting_seconds.unwrap_or(0));
                let now = Instant::now();
                self.asking(Ask {
                    id: question_id.clone(),
                    text: text.clone(),
                    suggestions: suggestions.clone(),
                    since: Some(now.checked_sub(waited).unwrap_or(now)),
                });
            }
            Event::CommandStarted { .. } => {
                self.command_since = Some(Instant::now());
                self.finished = None;
            }
            Event::CommandFinished { exit_code, .. } => self.command_ended(*exit_code, on_screen),
            Event::Bell { .. } if !on_screen => self.bell = true,
            _ => {}
        }
    }

    fn needs_answer(&self) -> bool {
        let agent = self.agent.as_ref();
        self.asked
            || agent.is_some_and(|a| {
                a.state == AgentState::Blocked || a.attention == AgentAttention::High
            })
    }

    /// What it holds for you off screen, strongest first.
    const fn held(&self) -> Option<Mark> {
        if self.done_unseen {
            return Some(Mark::Done);
        }
        match self.finished {
            Some(Some(code)) if code != 0 => Some(Mark::Failed),
            _ if self.bell => Some(Mark::Bell),
            Some(_) => Some(Mark::Succeeded),
            None => None,
        }
    }

    fn mark(&self) -> Mark {
        if self.needs_answer() {
            return Mark::Blocked;
        }
        if let Some(held) = self.held() {
            return held;
        }
        match self.agent.as_ref().map(|agent| agent.state) {
            Some(AgentState::Working) => Mark::Working,
            _ if self.command_since.is_some() => Mark::Running,
            Some(AgentState::Idle | AgentState::Done) => Mark::Idle,
            _ => Mark::None,
        }
    }

    fn face(&self) -> Face {
        (self.mark(), self.agent.clone(), self.ask.clone())
    }
}

#[derive(Debug, Default)]
pub struct Activities {
    terminals: HashMap<ResourceId, Activity>,
    revision: u64,
}

impl Activities {
    /// Fold one event in. Terminals in `on_screen` are in front of you and
    /// collect no marks. True when what a row shows changed; a busy agent's
    /// steady stream of restatements and commands is not that.
    pub fn apply(&mut self, event: &Event, on_screen: &[ResourceId]) -> bool {
        let Some(id) = subject(event) else {
            return false;
        };
        let before = self.terminals.get(id).map(Activity::face);
        if matches!(event, Event::TerminalClosed { .. }) {
            self.terminals.remove(id);
        } else {
            let entry = self.terminals.entry(id.clone()).or_default();
            entry.fold(event, on_screen.contains(id));
        }
        self.settled(id, before)
    }

    /// Note what moved since `before`: a mark's change dates it for ⌘J.
    fn settled(&mut self, id: &ResourceId, before: Option<Face>) -> bool {
        let before = before.unwrap_or((Mark::None, None, None));
        let after = self.terminals.get_mut(id);
        let face = after
            .as_ref()
            .map_or((Mark::None, None, None), |a| a.face());
        if face == before {
            return false;
        }
        if let Some(entry) = after.filter(|_| face.0 != before.0) {
            entry.changed_at = Some(Instant::now());
        }
        self.revision += 1;
        true
    }

    /// A new connection is up. What it missed can't be replayed: a command
    /// that ended in the gap never reports it, so only the live replica says
    /// one still runs, and a terminal the server no longer lists is gone.
    /// Declarations and questions come back on their own, as the runtime
    /// re-reads them. True when a row changed.
    pub fn reconnected(
        &mut self,
        listed: &[ResourceId],
        running: impl Fn(&ResourceId) -> bool,
    ) -> bool {
        let count = self.terminals.len();
        self.terminals.retain(|id, _| listed.contains(id));
        let mut changed = self.terminals.len() != count;
        for (id, entry) in &mut self.terminals {
            if entry.command_since.is_some() && !running(id) {
                let mark = entry.mark();
                entry.command_since = None;
                changed |= entry.mark() != mark;
            }
        }
        self.revision += u64::from(changed);
        changed
    }

    /// The terminal came on screen: what it was holding for you is seen.
    pub fn seen(&mut self, id: &ResourceId) {
        let before = self.terminals.get(id).map(Activity::face);
        if let Some(entry) = self.terminals.get_mut(id) {
            entry.bell = false;
            entry.finished = None;
            entry.done_unseen = false;
        }
        self.settled(id, before);
    }

    /// Moves whenever a mark, name or question does.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn mark(&self, id: &ResourceId) -> Mark {
        self.terminals.get(id).map_or(Mark::None, Activity::mark)
    }

    /// When the terminal's mark last changed.
    pub fn changed_at(&self, id: &ResourceId) -> Option<Instant> {
        self.terminals.get(id)?.changed_at
    }

    /// The question `id` is waiting on, when one is known.
    pub fn ask(&self, id: &ResourceId) -> Option<&Ask> {
        self.terminals.get(id)?.ask.as_ref()
    }

    fn agent(&self, id: &ResourceId) -> Option<&Agent> {
        self.terminals.get(id)?.agent.as_ref()
    }

    pub fn agent_name(&self, id: &ResourceId) -> Option<&str> {
        self.agent(id).map(|a| a.name.as_str())
    }

    /// The declared agent's kind slug, as in `claude`.
    pub fn agent_kind(&self, id: &ResourceId) -> Option<&str> {
        self.agent(id)?.kind.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    fn record(state: &str) -> Event {
        Event::AgentMetadata {
            terminal_id: id(1),
            value: Some(format!(r#"{{"name":"claude","state":"{state}"}}"#).into_bytes()),
        }
    }

    #[test]
    fn a_question_is_kept_until_the_level_retracts_it() {
        let mut a = Activities::default();
        let asked = Event::AgentAsked {
            terminal_id: id(1),
            question_id: "q1".to_owned(),
            text: "Ship it?".to_owned(),
            suggestions: vec!["yes".to_owned(), "no".to_owned()],
            waiting_seconds: Some(3),
        };
        a.apply(&asked, &[]);
        assert_eq!(a.ask(&id(1)).map(|ask| ask.id.as_str()), Some("q1"));
        assert!(a.mark(&id(1)).needs_you());
        a.apply(
            &Event::AgentAskedState {
                terminal_id: id(1),
                asked: false,
            },
            &[],
        );
        assert!(a.ask(&id(1)).is_none());
        assert!(!a.mark(&id(1)).needs_you());
    }

    #[test]
    fn a_blocked_agent_needs_you_until_it_moves_on() {
        let mut a = Activities::default();
        a.apply(&record("blocked"), &[]);
        assert_eq!(a.mark(&id(1)), Mark::Blocked);
        assert!(a.mark(&id(1)).needs_you());
        a.apply(&record("working"), &[]);
        assert_eq!(a.mark(&id(1)), Mark::Working);
        assert!(a.mark(&id(1)).live());
        assert_eq!(a.agent_name(&id(1)), Some("claude"));
    }

    #[test]
    fn finishing_off_screen_is_held_until_seen() {
        let mut a = Activities::default();
        a.apply(&record("working"), &[]);
        a.apply(&record("done"), &[]);
        assert_eq!(a.mark(&id(1)), Mark::Done);
        a.seen(&id(1));
        assert_eq!(a.mark(&id(1)), Mark::Idle);
    }

    #[test]
    fn the_shown_terminal_collects_no_marks() {
        let mut a = Activities::default();
        a.apply(&record("working"), &[id(1)]);
        a.apply(&record("done"), &[id(1)]);
        a.apply(&Event::Bell { terminal_id: id(1) }, &[id(1)]);
        assert_eq!(a.mark(&id(1)), Mark::Idle);
    }

    #[test]
    fn a_bell_and_a_question_mark_the_row() {
        let mut a = Activities::default();
        a.apply(&Event::Bell { terminal_id: id(1) }, &[]);
        assert_eq!(a.mark(&id(1)), Mark::Bell);
        a.apply(
            &Event::AgentAskedState {
                terminal_id: id(1),
                asked: true,
            },
            &[],
        );
        assert_eq!(a.mark(&id(1)), Mark::Blocked);
    }

    #[test]
    fn a_restated_record_changes_nothing() {
        let mut a = Activities::default();
        assert!(a.apply(&record("working"), &[]));
        let revision = a.revision();
        assert!(!a.apply(&record("working"), &[]));
        // A working agent's commands sit under its own mark.
        assert!(!a.apply(&Event::CommandStarted { terminal_id: id(1) }, &[]));
        let finished = Event::CommandFinished {
            terminal_id: id(1),
            exit_code: Some(0),
        };
        assert!(!a.apply(&finished, &[]));
        assert_eq!(a.revision(), revision);
    }

    #[test]
    fn a_question_announced_again_keeps_its_wait() {
        let asked = |seconds| Event::AgentAsked {
            terminal_id: id(1),
            question_id: "q1".to_owned(),
            text: "Ship it?".to_owned(),
            suggestions: Vec::new(),
            waiting_seconds: Some(seconds),
        };
        let mut a = Activities::default();
        assert!(a.apply(&asked(1), &[]));
        assert!(!a.apply(&asked(900), &[]));
    }

    #[test]
    fn a_mark_is_dated_when_it_changes() {
        let mut a = Activities::default();
        a.apply(&record("blocked"), &[]);
        let first = a.changed_at(&id(1));
        assert!(first.is_some());
        a.apply(&record("blocked"), &[]);
        assert_eq!(a.changed_at(&id(1)), first);
    }

    #[test]
    fn kind_is_kept() {
        let mut a = Activities::default();
        a.apply(
            &Event::AgentMetadata {
                terminal_id: id(1),
                value: Some(br#"{"name":"c","kind":"claude","state":"working"}"#.to_vec()),
            },
            &[],
        );
        assert_eq!(a.agent_kind(&id(1)), Some("claude"));
    }

    #[test]
    fn a_reconnect_clears_what_it_cannot_prove() {
        let mut a = Activities::default();
        a.apply(&Event::CommandStarted { terminal_id: id(1) }, &[]);
        a.apply(&Event::CommandStarted { terminal_id: id(2) }, &[]);
        a.apply(&Event::CommandStarted { terminal_id: id(3) }, &[]);
        assert!(a.reconnected(&[id(1), id(2)], |t| *t == id(2)));
        // Ended in the gap, still running, and no longer listed.
        assert_eq!(a.mark(&id(1)), Mark::None);
        assert_eq!(a.mark(&id(2)), Mark::Running);
        assert_eq!(a.mark(&id(3)), Mark::None);
        assert!(!a.reconnected(&[id(1), id(2)], |t| *t == id(2)));
    }

    #[test]
    fn a_reconnect_keeps_unseen_marks() {
        let mut a = Activities::default();
        a.apply(&Event::Bell { terminal_id: id(1) }, &[]);
        assert!(!a.reconnected(&[id(1)], |_| false));
        assert_eq!(a.mark(&id(1)), Mark::Bell);
    }

    #[test]
    fn only_acp_questions_refuse_typing() {
        let ask = |id: &str| Ask {
            id: id.to_owned(),
            text: String::new(),
            suggestions: Vec::new(),
            since: None,
        };
        assert!(ask("q1").typed());
        assert!(!ask("acp-question:1").typed());
    }

    #[test]
    fn a_running_command_shows_and_a_retracted_record_clears() {
        let mut a = Activities::default();
        a.apply(&Event::CommandStarted { terminal_id: id(1) }, &[]);
        assert_eq!(a.mark(&id(1)), Mark::Running);
        a.apply(
            &Event::CommandFinished {
                terminal_id: id(1),
                exit_code: Some(1),
            },
            &[],
        );
        // Short commands leave no mark behind.
        assert_eq!(a.mark(&id(1)), Mark::None);
        a.apply(&record("working"), &[]);
        a.apply(
            &Event::AgentMetadata {
                terminal_id: id(1),
                value: None,
            },
            &[],
        );
        assert_eq!(a.mark(&id(1)), Mark::None);
    }
}
