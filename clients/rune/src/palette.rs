//! The command palette: one field that reaches every terminal, session, and
//! action, ranked by a small fuzzy match.

use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor, Topology};
use phux_client_runtime::project_groups::{Block, GroupRow, group_rows};
use phux_client_runtime::session_order;
use phux_protocol::ids::{ResourceId, SessionKey};

use crate::addressable::Addressable;
use crate::keys::{COMMANDS, ESSENTIALS, Shortcut};

pub mod phone;

/// One thing the palette can do.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Mirror(crate::desk::resources::Source),
    StartMirror(crate::desk::resources::Source, String),
    /// List every host's displays and windows.
    Screens,
    /// Start mirroring a target on a host (`None`: this one) and open it.
    Screen(Option<String>, String),
    Terminal {
        id: ResourceId,
        session: String,
    },
    Session(String),
    Action(Shortcut),
    /// A theme by id; `None` is System.
    Theme(Option<&'static str>),
    /// A session to create, by the name typed.
    NewSession(String),
    /// A git worktree to create, by the branch typed.
    NewWorktree(String),
    /// A command line to run in a popup.
    Popup(String),
    /// An absolute path on the pane's host, to insert at its prompt.
    Path(String),
}

/// What the palette lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    Mirror(crate::desk::resources::Source),
    /// Terminals, sessions, actions and themes.
    #[default]
    All,
    /// Every chord the app answers to.
    Shortcuts,
    /// The keys worth learning first, then a row to the full list.
    Essentials,
    /// Sessions, and one to create by name.
    Sessions,
    /// Themes, previewed as the highlight moves.
    Themes,
    /// Paths on the focused pane's host, in the server's order.
    Paths,
    /// Every host's displays and windows, to mirror one.
    Screens,
    /// A command line, typed, to run in a popup.
    Popup,
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

/// Everything the palette lists, in its empty-query order: what `shown`
/// can put on screen (terminals, then documents), then sessions (in the
/// stored `order`), then actions, then themes.
pub fn items(
    topology: Option<&Topology>,
    shown: &[Addressable],
    order: &[SessionKey],
) -> Vec<Item> {
    let mut out: Vec<_> = shown
        .iter()
        .map(|resource| {
            let terminal = resource.kind.is_terminal();
            let detail = if terminal {
                format!("{} · {}", resource.session, resource.id)
            } else {
                format!(
                    "{} · {} · {}",
                    resource.session, resource.id, resource.mirror
                )
            };
            Item {
                title: resource.label.clone(),
                detail,
                kind: if terminal { "terminal" } else { "document" },
                target: Target::Terminal {
                    id: resource.id.clone(),
                    session: resource.session.clone(),
                },
            }
        })
        .collect();
    out.extend(session_items(topology, order));
    for command in COMMANDS.iter().filter(|command| command.listed) {
        out.push(Item {
            title: command.title.to_owned(),
            detail: command.hint(),
            kind: "action",
            target: Target::Action(command.action),
        });
    }
    out.extend(theme_items());
    out
}

/// Every terminal, with its session.
pub fn terminal_items(topology: Option<&Topology>) -> Vec<Item> {
    let panes = topology.into_iter().flat_map(|t| &t.panes);
    panes
        .map(|pane| Item {
            title: pane_label(pane),
            detail: format!("{} · {}", pane.session_name, pane.terminal_id),
            kind: "terminal",
            target: Target::Terminal {
                id: pane.terminal_id.clone(),
                session: pane.session_name.clone(),
            },
        })
        .collect()
}

/// Every theme, System first.
pub fn theme_items() -> Vec<Item> {
    let mut out = Vec::new();
    out.push(Item {
        title: "Theme: System".to_owned(),
        detail: "follows the OS".to_owned(),
        kind: "theme",
        target: Target::Theme(None),
    });
    for theme in crate::theme::all() {
        out.push(Item {
            title: format!("Theme: {}", theme.name),
            detail: match (theme.id, theme.dark) {
                ("ghostty", _) => "from your Ghostty config",
                ("omarchy", _) => "follows your Omarchy theme",
                (_, true) => "dark",
                (_, false) => "light",
            }
            .to_owned(),
            kind: "theme",
            target: Target::Theme(Some(theme.id)),
        });
    }
    out
}

/// The sessions, each with its terminal count, in the stored `order` and
/// grouped by project: a grouped session names its project, and a linked
/// worktree is marked `└`.
pub fn session_items(topology: Option<&Topology>, order: &[SessionKey]) -> Vec<Item> {
    let Some(topology) = topology else {
        return Vec::new();
    };
    let mut sessions: Vec<_> = topology.sessions.iter().collect();
    session_order::list(&mut sessions, order, |s| s.key, |s| s.touched);
    let item = |index: usize, project: Option<(&str, bool)>| {
        let session = sessions[index];
        let terminals = format!(
            "{} terminal{}",
            session.members,
            if session.members == 1 { "" } else { "s" }
        );
        Item {
            title: match project {
                Some((_, true)) => format!("└ {}", session.name),
                _ => session.name.clone(),
            },
            detail: match project {
                Some((name, _)) => format!("{name} · {terminals}"),
                None => terminals,
            },
            kind: "session",
            target: Target::Session(session.name.clone()),
        }
    };
    project_blocks(&sessions, |_| 0)
        .into_iter()
        .flat_map(|block| match block {
            Block::Single(index) => vec![item(index, None)],
            Block::Project(project) => project
                .members
                .iter()
                .map(|member| item(member.index, Some((&project.name, member.linked))))
                .collect(),
        })
        .collect()
}

/// Sessions grouped by project (ADR-0168), as the TUI sidebar groups them:
/// projects of two or more sessions first, each primary checkout followed
/// by its linked worktrees, then every other session in its place. Order
/// within a group and among the rest is the order `sessions` come in.
pub fn project_blocks(
    sessions: &[&SessionDescriptor],
    needs_you: impl Fn(u32) -> usize,
) -> Vec<Block> {
    let rows: Vec<GroupRow<'_>> = sessions
        .iter()
        .map(|session| GroupRow {
            project: session.project.as_ref(),
            link: session.project_link.as_deref(),
            label: None,
            needs_you: needs_you(session.id),
        })
        .collect();
    let (mut blocks, singles): (Vec<_>, Vec<_>) = group_rows(&rows, 2)
        .into_iter()
        .partition(|block| matches!(block, Block::Project(_)));
    blocks.extend(singles);
    blocks
}

/// A row creating the session `query` names, unless one has that name.
pub fn new_session_item(topology: Option<&Topology>, query: &str) -> Option<Item> {
    let name = query.trim();
    let taken = topology.is_some_and(|t| t.sessions.iter().any(|s| s.name == name));
    (!name.is_empty() && !taken).then(|| Item {
        title: format!("New Session \u{201c}{name}\u{201d}"),
        detail: "create".to_owned(),
        kind: "action",
        target: Target::NewSession(name.to_owned()),
    })
}

/// A row creating a worktree, and its session, for the branch `query` names.
pub fn new_worktree_item(query: &str) -> Option<Item> {
    let branch = query.trim();
    (!branch.is_empty() && !branch.contains(char::is_whitespace)).then(|| Item {
        title: format!("New Worktree \u{201c}{branch}\u{201d}"),
        detail: "branch and session".to_owned(),
        kind: "action",
        target: Target::NewWorktree(branch.to_owned()),
    })
}

/// A path row: named below `origin` when it lies there.
pub fn path_item(path: &str, directory: bool, origin: &str) -> Item {
    let shown = crate::paths::relative(path, origin).unwrap_or(path);
    // Control characters never reach the screen raw.
    let shown: String = shown
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect();
    Item {
        title: if directory {
            format!("{shown}/")
        } else {
            shown
        },
        detail: if directory { "folder" } else { "file" }.to_owned(),
        kind: "path",
        target: Target::Path(path.to_owned()),
    }
}

/// Every chord the app answers to, generated from the command table.
pub fn shortcut_items() -> Vec<Item> {
    // The bare window is an easter egg: its chord is found, not listed.
    let bound = COMMANDS
        .iter()
        .filter(|c| !c.chords().is_empty() && c.action != Shortcut::Bare);
    bound
        .map(|command| Item {
            title: command.title.to_owned(),
            detail: command.hints(),
            kind: "action",
            target: Target::Action(command.action),
        })
        .collect()
}

/// The essentials in teaching order, ending with the row that opens every
/// chord. Picking any row runs it, so reading the list is also trying it.
pub fn essential_items() -> Vec<Item> {
    let essentials = ESSENTIALS
        .iter()
        .filter_map(|action| crate::keys::command(*action));
    let mut out: Vec<Item> = essentials
        .map(|command| Item {
            title: command.title.to_owned(),
            detail: command.hints(),
            kind: "action",
            target: Target::Action(command.action),
        })
        .collect();
    out.push(Item {
        title: "All Keyboard Shortcuts".to_owned(),
        detail: "every chord".to_owned(),
        kind: "action",
        target: Target::Action(Shortcut::KeyboardShortcuts),
    });
    out
}

/// A terminal's name, else its title, else its folder.
pub fn pane_label(pane: &PaneDescriptor) -> String {
    let nonempty = |s: &Option<String>| s.clone().filter(|s| !s.trim().is_empty());
    nonempty(&pane.name)
        // An ask borrows the title to reach the server; it isn't a name.
        .or_else(|| {
            nonempty(&pane.title).filter(|t| phux_client_core::ask::parse_ask_title(t).is_none())
        })
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

    /// The essentials read in teaching order and end at the full list, so a
    /// newcomer who runs out of rows is one Return from every chord.
    #[test]
    fn essentials_end_with_the_full_list() {
        let items = essential_items();
        assert_eq!(items.len(), ESSENTIALS.len() + 1);
        for (item, action) in items.iter().zip(ESSENTIALS) {
            assert_eq!(item.target, Target::Action(*action));
            assert!(!item.detail.is_empty(), "{} shows no chord", item.title);
        }
        let last = items.last().expect("a last row");
        assert_eq!(last.target, Target::Action(Shortcut::KeyboardShortcuts));
        assert_eq!(last.title, "All Keyboard Shortcuts");
    }

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
        let hits = rank(items(None, &[], &[]), "new tab");
        assert_eq!(hits[0].item.target, Target::Action(Shortcut::NewTerminal));
    }

    #[test]
    fn the_shortcut_list_fits_and_shows_only_bound_commands() {
        let list = shortcut_items();
        assert!(list.len() <= ROWS, "{}", list.len());
        assert!(list.iter().all(|item| !item.detail.is_empty()));
        let tab = list.iter().find(|i| i.title == "Close Tab");
        assert!(tab.is_some());
    }

    #[test]
    fn close_and_step_tab_are_in_the_palette() {
        let list = items(None, &[], &[]);
        for action in [Shortcut::CloseTab, Shortcut::NextTab, Shortcut::PreviousTab] {
            assert!(list.iter().any(|i| i.target == Target::Action(action)));
        }
    }

    #[test]
    fn every_theme_is_a_row_and_its_name_finds_it() {
        let hits = rank(items(None, &[], &[]), "nord");
        assert_eq!(hits[0].item.target, Target::Theme(Some("nord")));
        let themes = items(None, &[], &[])
            .iter()
            .filter(|i| matches!(i.target, Target::Theme(_)))
            .count();
        assert_eq!(themes, crate::theme::all().count() + 1);
    }

    #[test]
    fn a_typed_name_offers_a_new_session_unless_taken() {
        let item = new_session_item(None, " api ").expect("offered");
        assert_eq!(item.target, Target::NewSession("api".to_owned()));
        assert!(new_session_item(None, "  ").is_none());
        let item = new_worktree_item(" feat/api ").expect("offered");
        assert_eq!(item.target, Target::NewWorktree("feat/api".to_owned()));
        assert!(new_worktree_item("two words").is_none());
    }

    #[test]
    fn paths_read_below_their_origin() {
        let row = path_item("/repo/src/main.rs", false, "/repo");
        assert_eq!(row.title, "src/main.rs");
        assert_eq!(path_item("/etc", true, "/repo").title, "/etc/");
        assert_eq!(path_item("/repo/a\nb", false, "/repo").title, "a\u{fffd}b");
    }

    #[test]
    fn sessions_group_by_project_with_linked_worktrees_marked() {
        use phux_client_core::layout::wire::HandleMap;
        use phux_client_runtime::project_identity::SessionProject;
        let project = |linked| SessionProject {
            id: "github.com/no-phux/phux".to_owned(),
            name: "phux".to_owned(),
            checkout: "/w/phux".to_owned(),
            branch: None,
            linked_worktree: linked,
            host: Some("mini".to_owned()),
        };
        let session = |id, name: &str, project| SessionDescriptor {
            id,
            name: name.to_owned(),
            attached_client_count: 0,
            keep_empty: false,
            created_at_unix_secs: 0,
            key: None,
            touched: None,
            lifetime: None,
            revision: None,
            members: 1,
            arrangement: None,
            awaiting_organization: false,
            project,
            project_link: None,
        };
        let mut topology = Topology {
            sessions: vec![
                session(1, "scratch", None),
                session(2, "feat", Some(project(true))),
                session(3, "phux", Some(project(false))),
            ],
            resources: Vec::new(),
            panes: Vec::new(),
            agent_sessions: Vec::new(),
            documents: Vec::new(),
            satellite_sessions: Vec::new(),
            focused_session: 0,
            focused_pane: ResourceId::local(0),
            authority: None,
            inbox: None,
            handles: HandleMap::new(),
        };
        let rows: Vec<_> = session_items(Some(&topology), &[])
            .into_iter()
            .map(|item| (item.title, item.detail))
            .collect();
        assert_eq!(
            rows,
            [
                ("phux".to_owned(), "phux · 1 terminal".to_owned()),
                ("└ feat".to_owned(), "phux · 1 terminal".to_owned()),
                ("scratch".to_owned(), "1 terminal".to_owned()),
            ],
            "the group leads, its primary checkout first"
        );
        // The stored order places the rest; the group still leads.
        let alpha = SessionKey::new([4; 16]);
        topology.sessions.push(SessionDescriptor {
            key: alpha,
            touched: None,
            ..session(4, "alpha", None)
        });
        let titles: Vec<_> = session_items(Some(&topology), &[alpha.expect("nonzero")])
            .into_iter()
            .map(|item| item.title)
            .collect();
        assert_eq!(titles, ["phux", "└ feat", "alpha", "scratch"]);
    }

    #[test]
    fn an_empty_query_keeps_list_order() {
        let items = items(None, &[], &[]);
        let hits = rank(items.clone(), "");
        assert_eq!(hits.len(), items.len());
        assert_eq!(hits[0].item, items[0]);
    }
}
