//! Palette row catalogues and Go To filtering, ranking, and revision tracking.

use super::*;

pub(super) fn simple_row(
    title: &str,
    detail: String,
    category: &'static str,
    target: Target,
) -> Row {
    Row {
        title: title.to_owned().into(),
        keywords: detail.clone(),
        detail: Some(detail.into()),
        target,
        command: None,
        category,
        chord: None,
        agent: None,
        preview: None,
    }
}

pub(super) fn settings_sections() -> Vec<Section> {
    vec![Section {
        label: Some("Settings"),
        rows: crate::app::settings_panel::Page::ALL
            .into_iter()
            .map(|page| {
                let mut row = simple_row(
                    page.title(),
                    page.description().to_owned(),
                    "Settings",
                    Target::Setting(page),
                );
                row.keywords = page.keywords().to_owned();
                row
            })
            .collect(),
    }]
}

/// Leads the detail of the Go To row this window shows, so the work, terminal
/// and tab in use stay legible among similar names and hosts.
pub(super) const CURRENT: &str = "Current · ";

pub(super) fn current_mark(current: bool) -> &'static str {
    if current { CURRENT } else { "" }
}

fn work_rows(desk: &Desk) -> Vec<Row> {
    desk.work_catalog()
        .into_iter()
        .map(|work| {
            let destination = work.destination;
            let status = if work.online { "" } else { " · offline" };
            let intake = if work.intake { " · Intake" } else { "" };
            let mark = current_mark(desk.work_is_active(&destination));
            let detail = format!(
                "{mark}{} · {} terminals{intake}{status}",
                destination.host_label(),
                work.terminals
            );
            let mut row = simple_row(
                &destination.name.clone(),
                detail,
                "Work",
                Target::Work(destination),
            );
            if let Some((id, name)) = work.project {
                row.keywords.push_str(&format!(" {id} {name}"));
            }
            row
        })
        .collect()
}

pub(super) fn command_sections(desk: &Desk, cx: &App) -> Vec<Section> {
    const ESSENTIALS: &[&str] = &["new", "native-agent", "split-right", "folder", "find"];
    let organizing = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.organization());
    let monitoring = desk.desktop.read(cx).performance_is_open();
    let mut sections: Vec<Section> = Vec::new();
    for command in crate::settings::builtin_commands()
        .iter()
        .filter(|command| {
            command.is_listed()
                && !matches!(command.id, "palette" | "goto")
                && (organizing || !command.needs_organization())
                && (monitoring || command.id != "performance-monitor-stop")
        })
    {
        let mut keywords = String::from(command.id);
        for word in command.keywords {
            keywords.push(' ');
            keywords.push_str(word);
        }
        let row = Row {
            title: command.title.into(),
            detail: Some(command.description.into()),
            keywords,
            target: Target::Command,
            command: Some(command.id),
            category: command.group,
            chord: desk.shown_chord(command.id),
            agent: None,
            preview: None,
        };
        let label = if ESSENTIALS.contains(&command.id) {
            "Essentials"
        } else {
            command.group
        };
        if let Some(section) = sections
            .iter_mut()
            .find(|section| section.label == Some(label))
        {
            section.rows.push(row);
        } else {
            sections.push(Section {
                label: Some(label),
                rows: vec![row],
            });
        }
    }
    if let Some(essentials) = sections
        .iter_mut()
        .find(|section| section.label == Some("Essentials"))
    {
        essentials
            .rows
            .sort_by_key(|row| ESSENTIALS.iter().position(|id| Some(*id) == row.command));
    }
    sections
}

/// A small launch list on open; typing always searches the complete catalogue.
pub(super) fn command_results(desk: &Desk, cx: &App, query: &str) -> Vec<Section> {
    let mut sections = command_sections(desk, cx);
    if !query.trim().is_empty() {
        sections.extend(settings_sections());
        return rank_sections(sections, query);
    }
    if !desk.palette_view.all_commands {
        sections.retain(|section| section.label == Some("Essentials"));
    }
    for section in &mut sections {
        section.label = None;
    }
    vec![Section {
        label: None,
        rows: sections
            .into_iter()
            .flat_map(|section| section.rows)
            .collect(),
    }]
}

pub(super) fn goto_sections(desk: &Desk) -> Vec<Section> {
    let Some(link) = desk.link.as_ref() else {
        return vec![Section {
            label: Some("Work"),
            rows: work_rows(desk),
        }];
    };
    let mut agents = Vec::new();
    let mut terminals = Vec::new();
    let focused = desk.focused_terminal();
    for pane in link.connection.panes() {
        let mark = current_mark(focused.as_ref() == Some(&pane.terminal_id));
        let agent = link.connection.agents.get(&pane.terminal_id);
        let title = desk.pane_title(Some(&pane.terminal_id));
        let directory = desk::short_path(pane.cwd.clone());
        let detail = format!("{} · {directory}", pane.session_name);
        let card_title = agent
            .map(|info| info.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| title.clone());
        let preview = goto_facts::facts_card(Some(&goto_facts::GotoSubject::Pane {
            title: card_title,
            agent: agent.cloned(),
            session: pane.session_name.clone(),
            directory,
            tab: tab_label(desk, &pane.terminal_id),
        }));
        let row = Row {
            title: agent
                .map(|info| format!("{} — {title}", info.name))
                .unwrap_or(title)
                .into(),
            keywords: agent
                .map(|info| format!("{detail} {}", info.word()))
                .unwrap_or_else(|| detail.clone()),
            detail: Some(
                (match agent.map(|info| info.word()) {
                    Some(word) if !word.is_empty() => format!("{mark}{word} · {detail}"),
                    _ => format!("{mark}{detail}"),
                })
                .into(),
            ),
            target: Target::Pane(pane.terminal_id.clone()),
            command: None,
            category: if agent.is_some() {
                "Agents"
            } else {
                "Terminals"
            },
            chord: None,
            agent: agent.cloned(),
            preview,
        };
        if agent.is_some() {
            agents.push(row)
        } else {
            terminals.push(row)
        }
    }
    // A one-pane tab is already listed as its terminal.
    let tabs = link
        .workspace
        .tabs
        .iter()
        .enumerate()
        .filter_map(|(index, tab)| {
            let placed = crate::workspace::layout::placements(&tab.root);
            if placed.len() < 2 {
                return None;
            }
            let mark = current_mark(tab.id == link.workspace.active_id);
            let detail = format!("{mark}Tab {} · {} panes", index + 1, placed.len());
            let title = desk::tab_title(desk, tab);
            let panes = placed
                .into_iter()
                .filter_map(|placement| tab_pane(desk, placement))
                .collect();
            Some(Row {
                title: title.clone().into(),
                keywords: detail.clone(),
                detail: Some(detail.into()),
                target: Target::Tab(tab.id.clone()),
                command: None,
                category: "Tabs",
                chord: None,
                agent: None,
                preview: goto_facts::facts_card(Some(&goto_facts::GotoSubject::Tab {
                    title,
                    panes,
                })),
            })
        })
        .collect::<Vec<_>>();
    [
        ("Work", work_rows(desk)),
        ("Agents", agents),
        ("Terminals", terminals),
        ("Tabs", tabs),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .map(|(label, rows)| Section {
        label: Some(label),
        rows,
    })
    .collect()
}

pub(super) fn session_sections(desk: &Desk) -> Vec<Section> {
    let Some(link) = desk.link.as_ref() else {
        return Vec::new();
    };
    let Some(topology) = link.connection.topology.as_ref() else {
        return Vec::new();
    };
    let rows = topology
        .sessions
        .iter()
        .map(|session| {
            let panes = link
                .connection
                .panes()
                .iter()
                .filter(|pane| pane.session_id == session.id)
                .count();
            let detail = format!(
                "{panes} {}",
                if panes == 1 { "terminal" } else { "terminals" }
            );
            let detail = if session.key.is_some() && session.key == topology.inbox {
                format!("Intake · unorganized work · {detail}")
            } else if session.awaiting_organization {
                format!("Imported · awaiting organization · {detail}")
            } else {
                detail
            };
            Row {
                title: session.name.clone().into(),
                keywords: detail.clone(),
                detail: Some(detail.into()),
                target: Target::Session(session.name.clone()),
                command: None,
                category: "Sessions",
                chord: None,
                agent: None,
                preview: None,
            }
        })
        .collect();
    vec![Section {
        label: Some("Sessions"),
        rows,
    }]
}

pub(super) fn move_sections(desk: &Desk) -> Vec<Section> {
    let Some(client) = desk.client() else {
        return Vec::new();
    };
    let replica = client.organization();
    let selected = &desk.organization.selected;
    let destinations: Vec<_> = replica
        .sessions()
        .iter()
        .filter(|session| {
            selected.iter().any(|key| {
                replica
                    .resource(key)
                    .is_some_and(|resource| resource.session != session.key)
            })
        })
        .collect();
    move_destination_sections(&destinations, replica.intake(), selected.len())
}

pub(super) fn move_destination_sections(
    sessions: &[&phux_protocol::wire::frame::SessionRecord],
    intake: Option<SessionKey>,
    count: usize,
) -> Vec<Section> {
    let mut unorganized = Vec::new();
    let mut named = Vec::new();
    for session in sessions {
        let designated = Some(session.key) == intake;
        let row = move_destination_row(session, designated, count);
        if designated {
            unorganized.push(row);
        } else {
            named.push(row);
        }
    }
    [("Unorganized work", unorganized), ("Named sessions", named)]
        .into_iter()
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(label, rows)| Section {
            label: Some(label),
            rows,
        })
        .collect()
}

fn move_destination_row(
    session: &phux_protocol::wire::frame::SessionRecord,
    designated: bool,
    count: usize,
) -> Row {
    let (title, name) = if designated {
        ("Leave unorganized (Intake)".to_owned(), "Intake".to_owned())
    } else {
        (session.name.clone(), session.name.clone())
    };
    let imported = if !designated && session.awaiting_organization {
        " · imported"
    } else {
        ""
    };
    Row {
        title: title.into(),
        detail: Some(format!("{count} selected{imported}").into()),
        keywords: session.name.clone(),
        target: Target::Move(session.key, name),
        command: None,
        category: "Organization",
        chord: None,
        agent: None,
        preview: None,
    }
}

/// The tab that shows `terminal`, by the same title the tab row uses.
fn tab_label(desk: &Desk, terminal: &ResourceId) -> Option<String> {
    let link = desk.link.as_ref()?;
    let tab = link.workspace.tabs.iter().find(|tab| {
        crate::workspace::layout::placements(&tab.root)
            .iter()
            .any(|placement| placement.terminal_id.as_ref() == Some(terminal))
    })?;
    Some(desk::tab_title(desk, tab))
}

/// One pane on a tab card: the agent name and its state word, or the terminal
/// title and its directory. Layout metadata only.
fn tab_pane(
    desk: &Desk,
    placement: &crate::workspace::layout::Placement,
) -> Option<goto_facts::Fact> {
    let id = placement.terminal_id.as_ref()?;
    let link = desk.link.as_ref()?;
    let pane = link.connection.pane(id)?;
    let agent = link.connection.agents.get(id);
    let name = agent
        .map(|info| info.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| desk.pane_title(Some(id)));
    let detail = agent
        .and_then(|info| {
            let word = info.word();
            (!word.is_empty()).then(|| word.to_owned())
        })
        .unwrap_or_else(|| desk::short_path(pane.cwd.clone()));
    Some(goto_facts::Fact::new(name, detail))
}

pub(super) fn filter_sections(
    mut sections: Vec<Section>,
    filter: goto_filter::GotoFilter,
) -> Vec<Section> {
    if filter == goto_filter::GotoFilter::All {
        return sections;
    }
    for section in &mut sections {
        section
            .rows
            .retain(|row| filter.matches(row.agent.as_ref()));
    }
    sections.retain(|section| !section.rows.is_empty());
    sections
}

pub(super) fn goto_id(row: &Row) -> Option<goto_filter::GotoId> {
    match &row.target {
        Target::Work(destination) => Some(goto_filter::GotoId::Work(destination.clone())),
        Target::Command => row.command.map(goto_filter::GotoId::Command),
        Target::Setting(page) => Some(goto_filter::GotoId::Setting(*page)),
        Target::Pane(id) => Some(goto_filter::GotoId::Pane(id.clone())),
        Target::Tab(id) => Some(goto_filter::GotoId::Tab(id.clone())),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn goto_candidates(sections: &[Section]) -> Vec<goto_filter::GotoCandidate> {
    sections
        .iter()
        .flat_map(|section| section.rows.iter())
        .filter_map(|row| {
            Some(goto_filter::GotoCandidate {
                id: goto_id(row)?,
                agent: row.agent.clone(),
            })
        })
        .collect()
}

pub(super) fn section_ids(sections: &[Section]) -> Vec<Vec<goto_filter::GotoId>> {
    sections
        .iter()
        .map(|section| section.rows.iter().filter_map(goto_id).collect())
        .collect()
}

/// Rank every row against `query` as one flat list; an empty query keeps the sections.
pub(super) fn rank_sections(sections: Vec<Section>, query: &str) -> Vec<Section> {
    goto_search::rank_sections(sections, query)
}

/// Fold Go To rows when their metadata revision changes, then rank the query.
pub(super) fn prepare_goto(desk: &mut Desk, query: &str) {
    let revision = search_revision(desk);
    let filter = desk.goto.filter;
    if desk.goto_search.needs_rebuild(revision) {
        let sections = goto_sections(desk);
        desk.goto_search.rebuild(revision, sections);
    }
    let generation = desk.goto_frecency.generation();
    if desk.goto_search.rank_current(query, filter, generation) {
        return;
    }
    let boosts = desk.goto_frecency.boosts(goto_frecency::now());
    let boot = desk
        .link
        .as_ref()
        .and_then(|link| link.connection.server_id());
    desk.goto_search
        .ensure_ranked(query, filter, generation, |row| {
            goto_boost(&boosts, boot.as_deref(), row)
        });
}

fn goto_boost(
    boosts: &std::collections::HashMap<goto_frecency::Key, i64>,
    boot: Option<&str>,
    row: &Row,
) -> i64 {
    let Some(boot) = boot else {
        return 0;
    };
    let key = match &row.target {
        Target::Pane(id) => goto_frecency::Key::pane(boot, &id.to_string()),
        Target::Tab(id) => goto_frecency::Key::tab(boot, id),
        _ => None,
    };
    key.and_then(|key| boosts.get(&key).copied()).unwrap_or(0)
}

/// Connection drains cover panes and agents. Tabs are local, so they mix in too.
pub(super) fn search_revision(desk: &Desk) -> goto_search::SearchRevision {
    let Some(link) = desk.link.as_ref() else {
        return goto_search::SearchRevision {
            connection: desk.fleet.revision.rotate_left(32),
            workspace: 0,
        };
    };
    goto_search::SearchRevision {
        connection: link
            .connection
            .revision()
            .wrapping_add(desk.fleet.revision.rotate_left(32)),
        workspace: workspace_stamp(&link.workspace),
    }
}

/// Tab rows and the focused terminal follow the active tab, so it mixes in too.
fn workspace_stamp(workspace: &crate::workspace::Workspace) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    hash = mix_bytes(hash, workspace.active_id.as_bytes());
    for tab in &workspace.tabs {
        hash = mix_bytes(hash, tab.id.as_bytes());
        hash = mix_bytes(hash, tab.focused_id.as_bytes());
        hash = mix_bytes(hash, tab.title.as_deref().unwrap_or("").as_bytes());
        hash = mix_node(hash, &tab.root);
    }
    hash
}

fn mix_node(mut hash: u64, node: &crate::workspace::layout::LayoutNode) -> u64 {
    match node {
        crate::workspace::layout::LayoutNode::Leaf { placement } => {
            hash = mix_bytes(hash, placement.id.as_bytes());
            mix_bytes(hash, placement.terminal_label.as_bytes())
        }
        crate::workspace::layout::LayoutNode::Split {
            id, first, second, ..
        } => {
            hash = mix_bytes(hash, id.as_bytes());
            hash = mix_node(hash, first);
            mix_node(hash, second)
        }
    }
}

fn mix_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    hash ^= bytes.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(super) fn query_terms(query: &str) -> Vec<String> {
    goto_search::PreparedQuery::parse(query).into_terms()
}
