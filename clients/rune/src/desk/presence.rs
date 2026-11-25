//! What a phone shows while its app is closed: how many agents wait on the
//! person and how many work, for the Lock Screen card, the Dynamic Island and
//! the widgets. Never what an agent asks: the phone may be locked.

use phux_client_runtime::control::Status;
use serde::Serialize;

use super::Desk;
use crate::activity::Mark;
use crate::inbox::{Item, approval_title};

/// Opens the inbox.
pub const INBOX: &str = "phux://inbox";
/// Rows a card has room for.
const ROWS: usize = 3;

/// What a [`Presence`] is derived from; while it holds still so does that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    marks: u64,
    listed: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Presence {
    pub waiting: usize,
    pub working: usize,
    /// "2 need you", else "3 working"; empty when neither.
    pub headline: String,
    /// The computer this is about.
    pub host: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Row {
    pub title: String,
    /// The agent and its session.
    pub detail: String,
    /// The status ladder's key: `needs_you`, `failed`, `done`, `working`.
    pub state: &'static str,
    /// What a tap opens.
    pub link: String,
}

impl Desk {
    /// The presence when it may have moved since the host last took it.
    /// `None` while not attached: a dropped link says nothing about agents.
    pub fn take_presence(&mut self) -> Option<Presence> {
        if self.connection.status != Status::Attached {
            return None;
        }
        let inputs = Inputs {
            marks: self.activities.revision(),
            listed: self.listed(),
        };
        let native = self.native.as_mut()?;
        if native.presence.replace(inputs) == Some(inputs) {
            return None;
        }
        Some(self.presence())
    }

    fn presence(&self) -> Presence {
        let items = self.inbox_items();
        let waiting = items.len();
        let working: Vec<_> = (self.connection.topology.iter())
            .flat_map(|t| &t.panes)
            .filter(|pane| self.activities.mark(&pane.terminal_id) == Mark::Working)
            .collect();
        let headline = match (waiting, working.len()) {
            (0, 0) => String::new(),
            (0, n) => format!("{n} {}", Mark::Working.word()),
            (n, _) => phux_design::plural("count.needs_you", n as u64).unwrap_or_default(),
        };
        let mut rows: Vec<Row> = items.iter().take(ROWS).map(waiting_row).collect();
        rows.extend(working.iter().take(ROWS - rows.len()).map(|pane| {
            let id = &pane.terminal_id;
            Row {
                title: crate::palette::pane_label(pane),
                detail: detail(self.activities.agent_name(id), &pane.session_name),
                state: "working",
                link: format!("phux://pane/{id}"),
            }
        }));
        Presence {
            waiting,
            working: working.len(),
            headline,
            host: self.machines.current.clone().unwrap_or_default(),
            rows,
        }
    }
}

fn waiting_row(item: &Item) -> Row {
    match item {
        Item::Approval(approval) => Row {
            title: approval_title(approval),
            detail: "Approval".to_owned(),
            state: "needs_you",
            link: INBOX.to_owned(),
        },
        Item::Terminal(w) => Row {
            title: w.label.clone(),
            detail: detail(w.agent.as_deref(), &w.session),
            state: w.mark.state().map_or("needs_you", phux_design::State::key),
            link: format!("phux://pane/{}", w.terminal),
        },
    }
}

fn detail(agent: Option<&str>, session: &str) -> String {
    match agent {
        Some(agent) if !session.is_empty() => format!("{agent} · {session}"),
        Some(agent) => agent.to_owned(),
        None => session.to_owned(),
    }
}

#[cfg(test)]
#[path = "presence/tests.rs"]
mod tests;
