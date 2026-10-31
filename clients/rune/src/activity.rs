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

#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub name: String,
    pub state: AgentState,
    pub attention: AgentAttention,
}

/// The one mark a row shows, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// Whether it pulses: something is in progress.
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

#[derive(Debug, Default)]
pub struct Activities(HashMap<ResourceId, Activity>);

impl Activities {
    /// Fold one event in. Terminals in `on_screen` are in front of you and
    /// collect no marks. True when a mark may have changed.
    pub fn apply(&mut self, event: &Event, on_screen: &[ResourceId]) -> bool {
        let on_screen = |id: &ResourceId| on_screen.contains(id);
        match event {
            Event::AgentMetadata { terminal_id, value } => {
                let agent = value
                    .as_deref()
                    .and_then(parse_agent_record)
                    .filter(|r| !r.name.is_empty())
                    .map(|r| {
                        let attention = r.effective_attention().0;
                        Agent {
                            name: r.name,
                            state: r.state,
                            attention,
                        }
                    });
                let entry = self.0.entry(terminal_id.clone()).or_default();
                let was = entry.agent.as_ref().map(|a| a.state);
                let finished = agent.as_ref().is_some_and(|a| a.state == AgentState::Done);
                if finished && was != Some(AgentState::Done) && !on_screen(terminal_id) {
                    entry.done_unseen = true;
                }
                if !finished {
                    entry.done_unseen = false;
                }
                entry.agent = agent;
                true
            }
            Event::AgentAskedState { terminal_id, asked } => {
                let entry = self.0.entry(terminal_id.clone()).or_default();
                entry.asked = *asked;
                if !*asked {
                    entry.ask = None;
                }
                true
            }
            Event::AgentAsked {
                terminal_id,
                question_id,
                text,
                suggestions,
                waiting_seconds,
            } => {
                let waited = Duration::from_secs(waiting_seconds.unwrap_or(0));
                let now = Instant::now();
                let entry = self.0.entry(terminal_id.clone()).or_default();
                entry.asked = true;
                entry.ask = Some(Ask {
                    id: question_id.clone(),
                    text: text.clone(),
                    suggestions: suggestions.clone(),
                    since: Some(now.checked_sub(waited).unwrap_or(now)),
                });
                true
            }
            Event::CommandStarted { terminal_id } => {
                let entry = self.0.entry(terminal_id.clone()).or_default();
                entry.command_since = Some(Instant::now());
                entry.finished = None;
                true
            }
            Event::CommandFinished {
                terminal_id,
                exit_code,
            } => {
                let entry = self.0.entry(terminal_id.clone()).or_default();
                let long = entry
                    .command_since
                    .take()
                    .is_some_and(|since| since.elapsed() >= LONG_COMMAND);
                if long && !on_screen(terminal_id) {
                    entry.finished = Some(*exit_code);
                }
                true
            }
            Event::Bell { terminal_id } if !on_screen(terminal_id) => {
                self.0.entry(terminal_id.clone()).or_default().bell = true;
                true
            }
            Event::TerminalClosed { terminal_id, .. } => self.0.remove(terminal_id).is_some(),
            _ => false,
        }
    }

    /// The terminal came on screen: what it was holding for you is seen.
    pub fn seen(&mut self, id: &ResourceId) {
        if let Some(entry) = self.0.get_mut(id) {
            entry.bell = false;
            entry.finished = None;
            entry.done_unseen = false;
        }
    }

    pub fn mark(&self, id: &ResourceId) -> Mark {
        let Some(a) = self.0.get(id) else {
            return Mark::None;
        };
        let state = a.agent.as_ref().map(|agent| agent.state);
        let high = a
            .agent
            .as_ref()
            .is_some_and(|agent| agent.attention == AgentAttention::High);
        if a.asked || state == Some(AgentState::Blocked) || high {
            return Mark::Blocked;
        }
        if a.done_unseen {
            return Mark::Done;
        }
        match a.finished {
            Some(Some(code)) if code != 0 => return Mark::Failed,
            _ if a.bell => return Mark::Bell,
            Some(_) => return Mark::Succeeded,
            None => {}
        }
        match state {
            Some(AgentState::Working) => Mark::Working,
            _ if a.command_since.is_some() => Mark::Running,
            Some(AgentState::Idle | AgentState::Done) => Mark::Idle,
            _ => Mark::None,
        }
    }

    /// The question `id` is waiting on, when one is known.
    pub fn ask(&self, id: &ResourceId) -> Option<&Ask> {
        self.0.get(id)?.ask.as_ref()
    }

    pub fn agent_name(&self, id: &ResourceId) -> Option<&str> {
        self.0.get(id)?.agent.as_ref().map(|a| a.name.as_str())
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
