//! Passive facts for the highlighted Go To row.
//!
//! The card lists metadata this client already holds: an agent's declared
//! state, its kind, and the directory, session, and tab already on the row.
//! Building it does not acquire, resize, focus, or acknowledge a terminal.

use crate::connection::AgentInfo;

/// How many facts a card lists. The rest are left out.
const MAX_FACTS: usize = 10;

/// One labeled fact. Both sides are metadata, never terminal cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fact {
    pub(crate) name: String,
    pub(crate) value: String,
}

impl Fact {
    pub(crate) fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// The card shown beside a highlighted Go To row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FactsCard {
    pub(crate) title: String,
    pub(crate) facts: Vec<Fact>,
}

/// Metadata already folded into a Go To row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GotoSubject {
    /// A terminal, with the agent badge when one is declared.
    Pane {
        /// Agent name, or the terminal's own title.
        title: String,
        /// Declared badge. Absent for a plain terminal.
        agent: Option<AgentInfo>,
        /// Session the terminal belongs to.
        session: String,
        /// Display directory. Empty when the host has not reported one.
        directory: String,
        /// Tab that shows the terminal, when it sits in one.
        tab: Option<String>,
    },
    /// A multi-pane tab. `panes` are name / status-or-directory pairs.
    Tab { title: String, panes: Vec<Fact> },
}

/// The card for a highlighted subject. `None` when nothing is highlighted, or
/// the subject has no title and no facts. The input is not modified.
#[must_use]
pub(crate) fn facts_card(subject: Option<&GotoSubject>) -> Option<FactsCard> {
    let subject = subject?;
    let (title, mut facts) = match subject {
        GotoSubject::Pane {
            title,
            agent,
            session,
            directory,
            tab,
        } => (
            title.clone(),
            pane_facts(agent.as_ref(), session, directory, tab.as_deref()),
        ),
        GotoSubject::Tab { title, panes } => (
            title.clone(),
            panes
                .iter()
                .filter(|pane| !pane.name.is_empty() || !pane.value.is_empty())
                .cloned()
                .collect(),
        ),
    };
    facts.truncate(MAX_FACTS);
    if title.is_empty() && facts.is_empty() {
        None
    } else {
        Some(FactsCard { title, facts })
    }
}

fn pane_facts(
    agent: Option<&AgentInfo>,
    session: &str,
    directory: &str,
    tab: Option<&str>,
) -> Vec<Fact> {
    let mut facts = Vec::new();
    if let Some(agent) = agent {
        let word = agent.word();
        if !word.is_empty() {
            facts.push(Fact::new("Status", word));
        }
        if let Some(kind) = agent.kind.as_deref().filter(|kind| !kind.is_empty()) {
            facts.push(Fact::new("Kind", kind));
        }
    }
    if !directory.is_empty() {
        facts.push(Fact::new("Directory", directory));
    }
    if !session.is_empty() {
        facts.push(Fact::new("Session", session));
    }
    if let Some(tab) = tab.filter(|tab| !tab.is_empty()) {
        facts.push(Fact::new("Tab", tab));
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::{Fact, GotoSubject, facts_card};
    use crate::connection::{AgentAttention, AgentInfo, AgentState};

    fn agent(state: AgentState, kind: Option<&str>) -> AgentInfo {
        AgentInfo {
            name: "Claude".into(),
            kind: kind.map(str::to_owned),
            state,
            attention: AgentAttention::Normal,
            changed_at_ms: 1_700_000_000_000,
        }
    }

    fn pane(agent: Option<AgentInfo>, directory: &str) -> GotoSubject {
        GotoSubject::Pane {
            title: agent
                .as_ref()
                .map(|info| info.name.clone())
                .unwrap_or_else(|| "shell".into()),
            agent,
            session: "work".into(),
            directory: directory.into(),
            tab: Some("build".into()),
        }
    }

    fn pairs(card: &super::FactsCard) -> Vec<(&str, &str)> {
        card.facts
            .iter()
            .map(|fact| (fact.name.as_str(), fact.value.as_str()))
            .collect()
    }

    #[test]
    fn a_highlighted_agent_shows_its_metadata_and_leaves_the_agent_unchanged() {
        let info = agent(AgentState::Working, Some("claude"));
        let before = info.clone();
        let subject = pane(Some(info.clone()), "~/repo");
        let card = facts_card(Some(&subject)).expect("highlighted agent shows a card");
        assert_eq!(info, before, "the card only reads the badge");
        assert_eq!(subject, pane(Some(before), "~/repo"));
        assert_eq!(card.title, "Claude");
        assert_eq!(
            pairs(&card),
            vec![
                ("Status", "working"),
                ("Kind", "claude"),
                ("Directory", "~/repo"),
                ("Session", "work"),
                ("Tab", "build"),
            ]
        );
        assert_eq!(card.facts[0].value, info.word());
    }

    #[test]
    fn a_terminal_without_an_agent_omits_status() {
        let card = facts_card(Some(&pane(None, "~/logs"))).expect("terminal card");
        assert_eq!(card.title, "shell");
        assert_eq!(
            pairs(&card),
            vec![
                ("Directory", "~/logs"),
                ("Session", "work"),
                ("Tab", "build"),
            ]
        );
        assert!(card.facts.iter().all(|fact| fact.name != "Status"));
    }

    #[test]
    fn a_missing_highlight_or_empty_metadata_shows_no_card() {
        assert!(facts_card(None).is_none());
        assert!(
            facts_card(Some(&GotoSubject::Pane {
                title: String::new(),
                agent: None,
                session: String::new(),
                directory: String::new(),
                tab: None,
            }))
            .is_none()
        );
        assert!(
            facts_card(Some(&GotoSubject::Tab {
                title: String::new(),
                panes: vec![Fact::new("", "")],
            }))
            .is_none()
        );
    }

    #[test]
    fn an_undeclared_state_omits_status_and_keeps_the_other_facts() {
        let card = facts_card(Some(&pane(Some(agent(AgentState::Unknown, None)), "")))
            .expect("title and session remain");
        assert_eq!(pairs(&card), vec![("Session", "work"), ("Tab", "build")]);
    }

    #[test]
    fn a_tab_lists_its_panes_and_stops_at_ten_facts() {
        let panes: Vec<Fact> = (0..12)
            .map(|index| Fact::new(format!("pane-{index}"), format!("/repo/{index}")))
            .collect();
        let card = facts_card(Some(&GotoSubject::Tab {
            title: "logs".into(),
            panes,
        }))
        .expect("tab card");
        assert_eq!(card.title, "logs");
        assert_eq!(card.facts.len(), 10);
        assert_eq!(card.facts[0].name, "pane-0");
        assert_eq!(card.facts[9].value, "/repo/9");
    }
}
