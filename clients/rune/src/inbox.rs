//! Everything waiting on a person, most urgent first: actions the server is
//! holding for a decision, then terminals by what they need.
//!
//! The same list drives the inbox, its count, and system notifications.

use std::cmp::Reverse;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use phux_client_core::ask::parse_ask_title;
use phux_client_runtime::approvals::Approval;
use phux_client_runtime::control::{PaneDescriptor, Topology};
use phux_protocol::ids::ResourceId;

use crate::activity::{Activities, Ask, Mark};
use crate::palette::pane_label;

/// A terminal that needs you.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    pub terminal: ResourceId,
    pub session: String,
    pub label: String,
    pub agent: Option<String>,
    pub kind: Option<String>,
    pub mark: Mark,
    pub ask: Option<Ask>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Approval(Approval),
    Terminal(Waiting),
}

/// What the pane is asking: as announced, else its title sentinel. The
/// server keeps only a flag, so an ask raised through a hook before this
/// window connected has no text to recover.
pub fn question(pane: &PaneDescriptor, activities: &Activities) -> Option<Ask> {
    activities.ask(&pane.terminal_id).cloned().or_else(|| {
        let marker = parse_ask_title(pane.title.as_deref()?)?;
        Some(Ask {
            id: marker.id,
            text: marker.question,
            suggestions: marker.suggestions,
            since: None,
        })
    })
}

/// What makes a row the same row: the question, not its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Approval(String),
    Terminal(ResourceId, Option<String>),
}

impl Item {
    pub fn key(&self) -> Key {
        match self {
            Self::Approval(approval) => Key::Approval(approval.id.clone()),
            Self::Terminal(waiting) => Key::Terminal(
                waiting.terminal.clone(),
                waiting.ask.as_ref().map(|ask| ask.id.clone()),
            ),
        }
    }

    /// The time its card shows, which moves with nothing else changing.
    pub fn clock(&self) -> Option<String> {
        match self {
            Self::Approval(approval) => Some(expiry(approval)),
            Self::Terminal(waiting) => waiting.ask.as_ref()?.since.map(waited),
        }
    }
}

const fn rank(mark: Mark) -> u8 {
    match mark {
        Mark::Blocked => 0,
        Mark::Failed => 1,
        Mark::Done => 2,
        _ => 3,
    }
}

/// The list, stable within a rank so rows don't swap under the pointer.
pub fn items(
    topology: Option<&Topology>,
    activities: &Activities,
    approvals: &[Approval],
) -> Vec<Item> {
    let mut waiting: Vec<Waiting> = topology
        .iter()
        .flat_map(|t| t.panes.iter())
        .filter_map(|pane| {
            let mark = activities.mark(&pane.terminal_id);
            mark.needs_you().then(|| Waiting {
                terminal: pane.terminal_id.clone(),
                session: pane.session_name.clone(),
                label: pane_label(pane),
                agent: activities.agent_name(&pane.terminal_id).map(str::to_owned),
                kind: activities.agent_kind(&pane.terminal_id).map(str::to_owned),
                mark,
                ask: question(pane, activities),
            })
        })
        .collect();
    waiting.sort_by_key(|w| rank(w.mark));
    approvals
        .iter()
        .cloned()
        .map(Item::Approval)
        .chain(waiting.into_iter().map(Item::Terminal))
        .collect()
}

/// Where ⌘J goes: the panes that need you by urgency, then newest first,
/// taking the one after `shown` so repeats walk them all. The inbox keeps its
/// rows still instead; a jump has no pointer to slide under.
pub fn next_jump<'a>(
    panes: &'a [PaneDescriptor],
    activities: &Activities,
    shown: Option<&ResourceId>,
) -> Option<&'a PaneDescriptor> {
    let mut order: Vec<&PaneDescriptor> = panes
        .iter()
        .filter(|p| activities.mark(&p.terminal_id).needs_you())
        .collect();
    order.sort_by_key(|p| {
        let id = &p.terminal_id;
        (
            rank(activities.mark(id)),
            Reverse(activities.changed_at(id)),
        )
    });
    let after = order.iter().position(|p| Some(&p.terminal_id) == shown);
    let next = after.map_or(0, |at| (at + 1) % order.len());
    order.get(next).copied()
}

/// A held command's catalog name in words: `KILL_RESOURCE` is "Kill resource".
pub fn method_words(method: &str) -> String {
    let lower = method.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// What a held action asks to do: its verb and signal.
pub fn approval_title(approval: &Approval) -> String {
    let words = method_words(&approval.method);
    match &approval.signal {
        Some(signal) => format!("{words} ({signal})"),
        None => words,
    }
}

/// Who wants it: the name the connection announced, else its client id.
pub fn approval_requester(approval: &Approval) -> String {
    approval
        .requester
        .client_name
        .clone()
        .unwrap_or_else(|| format!("client {}", approval.requester.client))
}

/// A wait in words: "12s", "4m", "2h".
pub fn age(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// How long something has waited.
pub fn waited(since: Instant) -> String {
    age(since.elapsed().as_secs())
}

/// How long a held action has left.
pub fn expiry(approval: &Approval) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_millis() as u64);
    let left = approval.expires_at_ms.saturating_sub(now_ms) / 1000;
    format!("expires in {}", age(left))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::approvals::Requester;
    use phux_client_runtime::control::Event;

    fn approval(method: &str, signal: Option<&str>, name: Option<&str>) -> Approval {
        Approval {
            id: "0".repeat(32),
            requester: Requester {
                client: 7,
                credential_id: None,
                client_name: name.map(str::to_owned),
            },
            method: method.to_owned(),
            signal: signal.map(str::to_owned),
            subjects: Vec::new(),
            requested_at_ms: 0,
            expires_at_ms: 0,
        }
    }

    #[test]
    fn a_held_action_reads_as_words() {
        assert_eq!(method_words("KILL_RESOURCE"), "Kill resource");
        assert_eq!(method_words(""), "");
        let held = approval("SIGNAL_TERMINAL", Some("TERM"), None);
        assert_eq!(approval_title(&held), "Signal terminal (TERM)");
        assert_eq!(approval_requester(&held), "client 7");
        let named = approval("KILL_RESOURCE", None, Some("claude"));
        assert_eq!(approval_requester(&named), "claude");
    }

    #[test]
    fn held_actions_come_first_with_nothing_else_waiting() {
        let list = items(None, &Activities::default(), &[approval("X", None, None)]);
        assert!(matches!(list.as_slice(), [Item::Approval(_)]));
    }

    #[test]
    fn a_new_question_on_a_pane_is_a_new_row() {
        let waiting = |ask: Option<&str>| {
            Item::Terminal(Waiting {
                terminal: ResourceId::local(1),
                session: "s".to_owned(),
                label: "l".to_owned(),
                agent: None,
                kind: None,
                mark: Mark::Blocked,
                ask: ask.map(|id| Ask {
                    id: id.to_owned(),
                    text: String::new(),
                    suggestions: Vec::new(),
                    since: None,
                }),
            })
        };
        assert_ne!(waiting(Some("q1")).key(), waiting(Some("q2")).key());
        assert_eq!(waiting(Some("q1")).key(), waiting(Some("q1")).key());
    }

    fn pane(n: u32) -> PaneDescriptor {
        PaneDescriptor {
            terminal_id: ResourceId::local(n),
            session_id: 1,
            session_name: "s".to_owned(),
            cols: 80,
            rows: 24,
            title: None,
            cwd: None,
            is_focused: false,
            key: None,
            revision: None,
            name: None,
            activation: None,
            exec: None,
        }
    }

    #[test]
    fn a_jump_takes_urgency_then_the_newest_and_walks_on() {
        let panes = [pane(1), pane(2), pane(3), pane(4)];
        let mut a = Activities::default();
        let blocked = |n: u32| Event::AgentMetadata {
            terminal_id: ResourceId::local(n),
            value: Some(br#"{"name":"c","state":"blocked"}"#.to_vec()),
        };
        let bell = Event::Bell {
            terminal_id: ResourceId::local(1),
        };
        a.apply(&bell, &[]);
        a.apply(&blocked(2), &[]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        a.apply(&blocked(3), &[]);
        let at = |shown: Option<u32>| {
            let shown = shown.map(ResourceId::local);
            next_jump(&panes, &a, shown.as_ref()).map(|p| p.terminal_id.clone())
        };
        let id = |n| Some(ResourceId::local(n));
        // Pane 4 needs nothing, so from it the walk starts at the top.
        assert_eq!(at(Some(4)), id(3));
        assert_eq!(at(Some(3)), id(2));
        assert_eq!(at(Some(2)), id(1));
        assert_eq!(at(Some(1)), id(3));
        assert_eq!(at(None), id(3));
        assert!(next_jump(&[], &a, None).is_none());
    }

    #[test]
    fn urgency_orders_marks() {
        assert!(rank(Mark::Blocked) < rank(Mark::Failed));
        assert!(rank(Mark::Failed) < rank(Mark::Done));
        assert!(rank(Mark::Done) < rank(Mark::Bell));
    }

    #[test]
    fn waits_read_short() {
        assert_eq!(age(12), "12s");
        assert_eq!(age(61), "1m");
        assert_eq!(age(7300), "2h");
    }
}
