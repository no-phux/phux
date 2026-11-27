//! The command palette: one field that reaches every terminal, session, and
//! action, ranked by a small fuzzy match.

use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor, Topology};
use phux_client_runtime::project_groups::{Block, GroupRow, group_rows};
use phux_client_runtime::session_order;
use phux_design::palette::{Fields, Query};
use phux_protocol::ids::{ResourceId, SessionKey};

use crate::addressable::Addressable;
use crate::keys::{COMMANDS, ESSENTIALS, Shortcut};

pub mod phone;
pub mod sections;

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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Extension(phux_plugin::ConfiguredAction),
    MoveToTab,
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
    /// Switch the server's keep-awake mode.
    KeepAwake(phux_config::KeepAwake),
    /// Claim (`true`) or release the view size lock.
    SizeLock(bool),
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
    /// The playground's scenes.
    Playground,
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
pub const ROWS: usize = 64;

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
            title: command.label(),
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
    let listed = topology
        .sessions
        .iter()
        .filter(|s| !crate::desk::quick::hidden(&s.name));
    let mut sessions: Vec<_> = listed.collect();
    session_order::list(&mut sessions, order, |s| s.key, |s| s.touched);
    let item = |index: usize, project: Option<(&str, bool)>| {
        let session = sessions[index];
        // The live panes the graph holds, else the count the server sent.
        let live = topology
            .panes
            .iter()
            .filter(|p| p.session_id == session.id)
            .count();
        let members = if live > 0 {
            live
        } else {
            session.members as usize
        };
        let terminals = format!(
            "{} terminal{}",
            members,
            if members == 1 { "" } else { "s" }
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
        title: format!("New session \u{201c}{name}\u{201d}"),
        detail: "create".to_owned(),
        kind: "action",
        target: Target::NewSession(name.to_owned()),
    })
}

/// A row creating a worktree, and its session, for the branch `query` names.
pub fn new_worktree_item(query: &str) -> Option<Item> {
    let branch = query.trim();
    (!branch.is_empty() && !branch.contains(char::is_whitespace)).then(|| Item {
        title: format!("New worktree \u{201c}{branch}\u{201d}"),
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
            title: command.label(),
            detail: command.hints(),
            kind: "action",
            target: Target::Action(command.action),
        })
        .collect()
}

/// `actions` in order, each with its chords beside it.
pub fn action_items(actions: &[Shortcut]) -> Vec<Item> {
    actions
        .iter()
        .filter_map(|action| crate::keys::command(*action))
        .map(|command| Item {
            title: command.label(),
            detail: command.hints(),
            kind: "action",
            target: Target::Action(command.action),
        })
        .collect()
}

/// The essentials in teaching order, ending with the row that opens every
/// chord. Picking any row runs it, so reading the list is also trying it.
pub fn essential_items() -> Vec<Item> {
    let mut out = action_items(ESSENTIALS);
    out.push(Item {
        title: "All Keyboard Shortcuts".to_owned(),
        detail: "every chord".to_owned(),
        kind: "action",
        target: Target::Action(Shortcut::KeyboardShortcuts),
    });
    out
}

/// A terminal's name, else what runs in it and where (`claude · phux`):
/// the one rule every client shares (`PaneDescriptor::label`), with the
/// pane's icon in front (`phux_config_core::pane_icon`).
pub fn pane_label(pane: &PaneDescriptor) -> String {
    phux_config_core::pane_icon::with_icon(
        pane.label(),
        pane.icon.as_deref(),
        pane.foreground.as_deref(),
        pane.cwd.as_deref(),
    )
}

/// `query` against `text` with the matcher every phux palette shares
/// ([`phux_design::palette`]): every word must match, in any order, as
/// fragments or initials, and the matched characters are placed where they
/// read best (word starts, runs). `None` when some word does not match.
pub fn score(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let fields = Fields {
        title: text,
        ..Fields::default()
    };
    phux_design::palette::score(&Query::parse(query), &fields)
        .map(|hit| (hit.score, hit.title_positions))
}

/// How well `item` matches `query`: its title (lit), else, for less, its
/// detail (the session, or an action's keys).
fn scored(item: &Item, query: &str) -> Option<(i32, Vec<usize>)> {
    let fields = Fields {
        title: &item.title,
        aliases: &[],
        detail: Some(&item.detail),
    };
    phux_design::palette::score(&Query::parse(query), &fields)
        .map(|hit| (hit.score, hit.title_positions))
}

/// The items matching `query`, best first. An empty query keeps list order.
pub fn rank(items: Vec<Item>, query: &str) -> Vec<Hit> {
    rank_grouped(items, &[], query)
}

/// A tab's terminals, which the palette lists under the tab's title.
#[derive(Debug, Clone, PartialEq)]
pub struct TabGroup {
    pub title: String,
    pub session: String,
    pub panes: Vec<ResourceId>,
}

/// [`rank`], with each terminal of `tabs` under a heading for its tab,
/// placed where its best terminal ranks. A tab whose title matches brings
/// all its terminals, and a matched terminal brings its tab's heading.
pub fn rank_grouped(items: Vec<Item>, tabs: &[TabGroup], query: &str) -> Vec<Hit> {
    let tab_of = |item: &Item| match &item.target {
        Target::Terminal { id, .. } if item.kind == "terminal" => {
            tabs.iter().position(|tab| tab.panes.contains(id))
        }
        _ => None,
    };
    let titled: Vec<_> = tabs.iter().map(|tab| score(query, &tab.title)).collect();
    let mut hits: Vec<Hit> = items
        .into_iter()
        .filter_map(|item| {
            let by_tab = || Some((titled[tab_of(&item)?].as_ref()?.0 - 1, Vec::new()));
            let (score, matched) = scored(&item, query).or_else(by_tab)?;
            Some(Hit {
                item,
                score,
                matched,
            })
        })
        .collect();
    if !query.trim().is_empty() {
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
    }
    if tabs.is_empty() {
        return hits;
    }
    let at: Vec<_> = hits.iter().map(|hit| tab_of(&hit.item)).collect();
    let mut placed = vec![false; tabs.len()];
    let mut out = Vec::with_capacity(hits.len() + tabs.len());
    for (hit, tab) in hits.iter().zip(&at) {
        let Some(tab) = *tab else {
            out.push(hit.clone());
            continue;
        };
        if std::mem::replace(&mut placed[tab], true) {
            continue;
        }
        let group = &tabs[tab];
        out.push(Hit {
            item: Item {
                title: group.title.clone(),
                detail: group.session.clone(),
                kind: "tab",
                target: Target::Terminal {
                    id: group.panes[0].clone(),
                    session: group.session.clone(),
                },
            },
            score: 0,
            matched: titled[tab].clone().map(|(_, m)| m).unwrap_or_default(),
        });
        let panes = hits.iter().zip(&at).filter(|(_, at)| **at == Some(tab));
        out.extend(panes.map(|(hit, _)| {
            let mut hit = hit.clone();
            hit.item.kind = "pane";
            hit
        }));
    }
    out
}

#[cfg(test)]
mod tests;
