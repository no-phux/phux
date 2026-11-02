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
    /// A reconnect left that command in doubt: it may have ended in the gap.
    pub unproven: bool,
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
        self.unproven = false;
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
                self.unproven = false;
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

    /// A new connection is up, and a terminal the server no longer lists is
    /// gone. A command that ended in the gap never reports it, but one still
    /// running will, and clearing that would lose its finish: each stays
    /// until [`Self::prove`] hears otherwise. Declarations and questions come
    /// back on their own, as the runtime re-reads them. True when a row changed.
    pub fn reconnected(&mut self, listed: &[ResourceId]) -> bool {
        let count = self.terminals.len();
        self.terminals.retain(|id, _| listed.contains(id));
        for entry in self.terminals.values_mut() {
            entry.unproven = entry.command_since.is_some();
        }
        let changed = self.terminals.len() != count;
        self.revision += u64::from(changed);
        changed
    }

    /// Whether a reconnect left any command in doubt.
    pub fn unproven(&self) -> bool {
        self.terminals.values().any(|a| a.unproven)
    }

    /// Settle what `running` can answer for among `ids`. `None` is no answer,
    /// as with no replica yet or on the alternate screen. True when a row changed.
    pub fn prove(
        &mut self,
        ids: &[ResourceId],
        running: impl Fn(&ResourceId) -> Option<bool>,
    ) -> bool {
        let mut changed = false;
        for id in ids {
            let Some(entry) = self.terminals.get_mut(id).filter(|a| a.unproven) else {
                continue;
            };
            let Some(running) = running(id) else {
                continue;
            };
            let before = entry.face();
            entry.unproven = false;
            if !running {
                entry.command_since = None;
            }
            changed |= self.settled(id, Some(before));
        }
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
mod tests;
