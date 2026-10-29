//! The command palette: one field that reaches every terminal, session, and
//! action, ranked by a small fuzzy match.

use phux_client_runtime::control::{PaneDescriptor, Topology};
use phux_protocol::ids::ResourceId;

use crate::keys::Shortcut;

/// One thing the palette can do.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Terminal { id: ResourceId, session: String },
    Session(String),
    Action(Shortcut),
}

/// The most rows the palette renders; the highlight stays within them.
pub const ROWS: usize = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub title: String,
    /// Dim text beside the title: the session, or an action's keys.
    pub detail: String,
    pub kind: &'static str,
    pub target: Target,
}

/// A ranked item and the title's matched character positions.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub item: Item,
    pub score: i32,
    pub matched: Vec<usize>,
}

const ACTIONS: &[(Shortcut, &str, &str)] = &[
    (Shortcut::NewTerminal, "New terminal", "⌘T"),
    (Shortcut::NewSession, "New session", "⌘N"),
    (Shortcut::JumpToAttention, "Jump to what needs you", "⌘J"),
    (Shortcut::SplitRight, "Split right", "⌘D"),
    (Shortcut::SplitDown, "Split down", "⌘⇧D"),
    (Shortcut::ZoomPane, "Zoom pane", "⌘⇧↩"),
    (Shortcut::CloseTerminal, "Close terminal", "⌘W"),
    (Shortcut::Find, "Find in terminal", "⌘F"),
    (Shortcut::FindSelection, "Find selected text", "⌘E"),
    (Shortcut::FocusLeft, "Focus pane left", "⌘⌥←"),
    (Shortcut::FocusRight, "Focus pane right", "⌘⌥→"),
    (Shortcut::FocusUp, "Focus pane up", "⌘⌥↑"),
    (Shortcut::FocusDown, "Focus pane down", "⌘⌥↓"),
    (Shortcut::NextTerminal, "Next terminal", "⌘]"),
    (Shortcut::PreviousTerminal, "Previous terminal", "⌘["),
    (Shortcut::ToggleSidebar, "Toggle sidebar", "⌘B"),
    (Shortcut::Copy, "Copy selection", "⌘C"),
    (Shortcut::Paste, "Paste", "⌘V"),
    (Shortcut::ZoomIn, "Bigger text", "⌘="),
    (Shortcut::ZoomOut, "Smaller text", "⌘-"),
    (Shortcut::ZoomReset, "Default text size", "⌘0"),
    (Shortcut::ScrollTop, "Scroll to top of history", "⌘↑"),
    (Shortcut::ScrollBottom, "Scroll to bottom", "⌘↓"),
];

/// Everything the palette lists, in its empty-query order: terminals, then
/// sessions, then actions.
pub fn items(topology: Option<&Topology>) -> Vec<Item> {
    let mut out = Vec::new();
    if let Some(topology) = topology {
        for pane in &topology.panes {
            out.push(Item {
                title: pane_label(pane),
                detail: format!("{} · {}", pane.session_name, pane.terminal_id),
                kind: "terminal",
                target: Target::Terminal {
                    id: pane.terminal_id.clone(),
                    session: pane.session_name.clone(),
                },
            });
        }
        for session in &topology.sessions {
            out.push(Item {
                title: session.name.clone(),
                detail: format!(
                    "{} terminal{}",
                    session.members,
                    if session.members == 1 { "" } else { "s" }
                ),
                kind: "session",
                target: Target::Session(session.name.clone()),
            });
        }
    }
    for (action, title, keys) in ACTIONS {
        out.push(Item {
            title: (*title).to_owned(),
            detail: keys_for_platform(keys),
            kind: "action",
            target: Target::Action(*action),
        });
    }
    out
}

/// A terminal's name, else its title, else its folder.
pub fn pane_label(pane: &PaneDescriptor) -> String {
    let nonempty = |s: &Option<String>| s.clone().filter(|s| !s.trim().is_empty());
    nonempty(&pane.name)
        .or_else(|| nonempty(&pane.title))
        .or_else(|| {
            nonempty(&pane.cwd).map(|cwd| {
                let trimmed = cwd.trim_end_matches('/');
                trimmed
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("/")
                    .to_owned()
            })
        })
        .unwrap_or_else(|| "terminal".to_owned())
}

fn keys_for_platform(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        keys.to_owned()
    } else {
        keys.replace('⌘', "Ctrl+Shift+")
    }
}

/// Fuzzy subsequence match of `query` in `text`, case-insensitive. Rewards
/// word starts and runs; `None` when a query character is missing.
pub fn score(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let mut matched = Vec::with_capacity(query.len());
    let mut score = 0;
    let mut at = 0;
    let mut previous: Option<usize> = None;
    for wanted in query {
        let found =
            (at..chars.len()).find(|&i| chars[i].to_lowercase().eq(std::iter::once(wanted)))?;
        let word_start = found == 0 || !chars[found - 1].is_alphanumeric();
        score += 1;
        if word_start {
            score += 8;
        }
        if previous.is_some_and(|p| p + 1 == found) {
            score += 5;
        } else if let Some(p) = previous {
            score -= ((found - p) as i32).min(6);
        }
        matched.push(found);
        previous = Some(found);
        at = found + 1;
    }
    // Shorter titles win ties: the match covers more of them.
    score -= (chars.len() as i32 / 8).min(4);
    Some((score, matched))
}

/// The items matching `query`, best first. An empty query keeps list order.
pub fn rank(items: Vec<Item>, query: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = items
        .into_iter()
        .filter_map(|item| {
            let (title_score, matched) = score(query, &item.title)
                .or_else(|| score(query, &item.detail).map(|(s, _)| (s - 6, Vec::new())))?;
            Some(Hit {
                item,
                score: title_score,
                matched,
            })
        })
        .collect();
    if !query.trim().is_empty() {
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches_and_misses() {
        assert!(score("nt", "New terminal").is_some());
        assert!(score("xyz", "New terminal").is_none());
        assert_eq!(score("", "anything"), Some((0, vec![])));
    }

    #[test]
    fn word_starts_and_runs_outrank_scattered_matches() {
        let (initials, _) = score("nt", "New terminal").expect("matches");
        let (scattered, _) = score("nt", "inventory").expect("matches");
        assert!(initials > scattered, "{initials} vs {scattered}");
    }

    #[test]
    fn matched_positions_are_title_chars() {
        let (_, at) = score("bt", "Bigger text").expect("matches");
        assert_eq!(at, vec![0, 7]);
    }

    #[test]
    fn ranking_puts_the_best_title_first_and_falls_back_to_details() {
        let hits = rank(items(None), "new term");
        assert_eq!(hits[0].item.target, Target::Action(Shortcut::NewTerminal));
    }

    #[test]
    fn an_empty_query_keeps_list_order() {
        let items = items(None);
        let hits = rank(items.clone(), "");
        assert_eq!(hits.len(), items.len());
        assert_eq!(hits[0].item, items[0]);
    }
}
