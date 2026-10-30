//! Everything waiting on a person, most urgent first: actions the server is
//! holding for a decision, then terminals by what they need.
//!
//! The same list drives the inbox, its count, and system notifications.

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

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::approvals::Requester;

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
